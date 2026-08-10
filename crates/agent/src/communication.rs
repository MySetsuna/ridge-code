//! Bounded agent-to-agent protocol shared by dispatch and teammate paths.
//!
//! Business code speaks `AgentEnvelope`; wire details stay in transports. The
//! first two transports are an in-process channel and newline-delimited JSON-RPC.

use std::future::Future;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, Notify};

pub const AGENT_PROTOCOL_VERSION: u16 = 1;
pub const AGENT_EXCHANGE_TIMEOUT: Duration = Duration::from_secs(300);
pub const MAX_AGENT_FRAME_BYTES: usize = 64 * 1024;
pub const MAX_AGENT_CONTEXT_ENTRIES: usize = 32;
pub const MAX_AGENT_CONTEXT_BYTES: usize = 16 * 1024;
pub const MAX_AGENT_REPLAY_ENTRIES: usize = 1024;
pub const MAX_AGENT_CLOCK_SKEW_SECS: u64 = 300;

#[derive(Clone, Default)]
pub struct AgentCancellation {
    cancelled: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl AgentCancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let notified = self.notify.notified();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    Maker,
    Checker,
    Planner,
    Worker,
    Explorer,
    Reviewer,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutonomyLevel {
    ReadOnly,
    Guarded,
}

/// Explicit governance metadata travels with a task, rather than being
/// inferred from model text or a transport-specific header.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutonomyTier {
    Observe,
    Suggest,
    ActWithApproval,
    BoundedAutonomous,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalRequirement {
    None,
    Explicit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceMetadata {
    pub policy_id: String,
    pub audit_id: String,
    pub autonomy: AutonomyTier,
    pub approval: ApprovalRequirement,
    pub approval_granted: bool,
    pub max_steps: usize,
}

impl GovernanceMetadata {
    pub fn validate(&self) -> Result<(), AgentProtocolError> {
        if self.policy_id.trim().is_empty() || self.audit_id.trim().is_empty() {
            return Err(AgentProtocolError::Invalid(
                "governance policy_id and audit_id must be non-empty".to_string(),
            ));
        }
        if self.max_steps == 0 {
            return Err(AgentProtocolError::Invalid(
                "governance max_steps must be positive".to_string(),
            ));
        }
        if matches!(self.approval, ApprovalRequirement::Explicit) && !self.approval_granted {
            return Err(AgentProtocolError::Unauthorized(
                "governance approval is required".to_string(),
            ));
        }
        if matches!(self.autonomy, AutonomyTier::ActWithApproval)
            && !matches!(self.approval, ApprovalRequirement::Explicit)
        {
            return Err(AgentProtocolError::Invalid(
                "act-with-approval governance requires explicit approval".to_string(),
            ));
        }
        if matches!(self.autonomy, AutonomyTier::Observe) && self.approval_granted {
            return Err(AgentProtocolError::Invalid(
                "observe-only governance cannot grant approval".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCapability {
    pub name: String,
    pub read_only: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentHello {
    pub agent_id: String,
    pub roles: Vec<AgentRole>,
    pub protocol_versions: Vec<u16>,
    pub capabilities: Vec<AgentCapability>,
    pub autonomy: AutonomyLevel,
    pub max_in_flight: usize,
}

impl AgentHello {
    pub fn read_only(agent_id: impl Into<String>, role: AgentRole) -> Self {
        Self {
            agent_id: agent_id.into(),
            roles: vec![role],
            protocol_versions: vec![AGENT_PROTOCOL_VERSION],
            capabilities: ["read_only_task", "read_file", "search"]
                .into_iter()
                .map(|name| AgentCapability {
                    name: name.to_string(),
                    read_only: true,
                })
                .collect(),
            autonomy: AutonomyLevel::ReadOnly,
            max_in_flight: 1,
        }
    }

    pub fn guarded(agent_id: impl Into<String>, role: AgentRole) -> Self {
        Self {
            agent_id: agent_id.into(),
            roles: vec![role],
            protocol_versions: vec![AGENT_PROTOCOL_VERSION],
            capabilities: vec![AgentCapability {
                name: "bounded_task".to_string(),
                read_only: false,
            }],
            autonomy: AutonomyLevel::Guarded,
            max_in_flight: 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTask {
    pub task: String,
    #[serde(default)]
    pub context: std::collections::BTreeMap<String, String>,
    pub read_only: bool,
    pub allowed_tools: Vec<String>,
    pub required_capabilities: Vec<String>,
    pub budget_steps: usize,
}

impl AgentTask {
    pub fn new(
        task: impl Into<String>,
        read_only: bool,
        allowed_tools: Vec<String>,
        budget_steps: usize,
    ) -> Self {
        Self {
            task: task.into(),
            context: std::collections::BTreeMap::new(),
            read_only,
            allowed_tools,
            required_capabilities: Vec::new(),
            budget_steps,
        }
    }

    pub fn with_context(mut self, context: std::collections::BTreeMap<String, String>) -> Self {
        self.context = context;
        self
    }

    fn validate_context(&self) -> Result<(), AgentProtocolError> {
        if self.context.len() > MAX_AGENT_CONTEXT_ENTRIES {
            return Err(AgentProtocolError::Invalid(format!(
                "agent context has too many entries: {}",
                self.context.len()
            )));
        }
        let bytes = self
            .context
            .iter()
            .map(|(key, value)| key.len() + value.len())
            .sum::<usize>();
        if bytes > MAX_AGENT_CONTEXT_BYTES {
            return Err(AgentProtocolError::Invalid(format!(
                "agent context exceeds {} bytes",
                MAX_AGENT_CONTEXT_BYTES
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Accepted,
    Running,
    Done,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentResponse {
    pub status: AgentStatus,
    pub approved: bool,
    pub steps: usize,
    pub tokens: usize,
    pub summary: String,
    pub modified_files: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentMessage {
    Hello(AgentHello),
    Task(AgentTask),
    Response(AgentResponse),
    Event {
        status: AgentStatus,
        summary: String,
    },
    Cancel {
        reason: String,
    },
    Error(AgentError),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEnvelope {
    pub version: u16,
    pub message_id: String,
    pub correlation_id: String,
    pub parent_id: Option<String>,
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<AgentSecurityStamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub governance: Option<GovernanceMetadata>,
    pub message: AgentMessage,
}

/// Optional wire-level authenticity metadata. Unsigned envelopes remain
/// compatible with existing local transports; secure callers must opt in via
/// [`AgentEnvelope::sign`] and [`AgentEnvelope::verify`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSecurityStamp {
    pub key_id: String,
    pub nonce: String,
    pub issued_at_epoch: u64,
    pub signature: String,
}

/// Bounded replay memory for one authenticated agent session.
#[derive(Debug)]
pub struct ReplayGuard {
    seen: std::collections::BTreeSet<String>,
    order: std::collections::VecDeque<String>,
    capacity: usize,
}

impl ReplayGuard {
    pub fn new() -> Self {
        Self::with_capacity(MAX_AGENT_REPLAY_ENTRIES)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            seen: std::collections::BTreeSet::new(),
            order: std::collections::VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    fn record_once(&mut self, nonce: &str) -> bool {
        if self.seen.contains(nonce) {
            return false;
        }
        self.seen.insert(nonce.to_string());
        self.order.push_back(nonce.to_string());
        while self.order.len() > self.capacity {
            if let Some(evicted) = self.order.pop_front() {
                self.seen.remove(&evicted);
            }
        }
        true
    }
}

impl Default for ReplayGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentEnvelope {
    pub fn hello(
        message_id: impl Into<String>,
        from: impl Into<String>,
        to: impl Into<String>,
        hello: AgentHello,
    ) -> Self {
        Self::new(
            message_id,
            "handshake",
            None,
            from,
            to,
            AgentMessage::Hello(hello),
        )
    }

    pub fn task(
        message_id: impl Into<String>,
        from: impl Into<String>,
        to: impl Into<String>,
        correlation_id: impl Into<String>,
        task: AgentTask,
    ) -> Self {
        Self::new(
            message_id,
            correlation_id,
            None,
            from,
            to,
            AgentMessage::Task(task),
        )
    }

    pub fn response(
        message_id: impl Into<String>,
        from: impl Into<String>,
        to: impl Into<String>,
        correlation_id: impl Into<String>,
        response: AgentResponse,
    ) -> Self {
        Self::new(
            message_id,
            correlation_id,
            None,
            from,
            to,
            AgentMessage::Response(response),
        )
    }

    pub fn error(
        message_id: impl Into<String>,
        from: impl Into<String>,
        to: impl Into<String>,
        correlation_id: impl Into<String>,
        error: AgentError,
    ) -> Self {
        Self::new(
            message_id,
            correlation_id,
            None,
            from,
            to,
            AgentMessage::Error(error),
        )
    }

    pub fn cancel(
        message_id: impl Into<String>,
        from: impl Into<String>,
        to: impl Into<String>,
        correlation_id: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self::new(
            message_id,
            correlation_id,
            None,
            from,
            to,
            AgentMessage::Cancel {
                reason: reason.into(),
            },
        )
    }

    fn new(
        message_id: impl Into<String>,
        correlation_id: impl Into<String>,
        parent_id: Option<String>,
        from: impl Into<String>,
        to: impl Into<String>,
        message: AgentMessage,
    ) -> Self {
        Self {
            version: AGENT_PROTOCOL_VERSION,
            message_id: message_id.into(),
            correlation_id: correlation_id.into(),
            parent_id,
            from: from.into(),
            to: to.into(),
            security: None,
            governance: None,
            message,
        }
    }

    /// Add an HMAC-SHA256 stamp over the complete unsigned envelope.
    pub fn sign(
        &mut self,
        key_id: impl Into<String>,
        secret: &str,
        nonce: impl Into<String>,
        issued_at_epoch: u64,
    ) -> Result<(), AgentProtocolError> {
        let key_id = key_id.into();
        let nonce = nonce.into();
        if key_id.trim().is_empty() || nonce.trim().is_empty() || secret.is_empty() {
            return Err(AgentProtocolError::Invalid(
                "security key_id, nonce, and secret must be non-empty".to_string(),
            ));
        }
        self.security = Some(AgentSecurityStamp {
            key_id,
            nonce,
            issued_at_epoch,
            signature: String::new(),
        });
        let payload = self.signed_payload()?;
        let signature = hmac_sha256_hex(secret.as_bytes(), &payload);
        if let Some(stamp) = self.security.as_mut() {
            stamp.signature = signature;
            Ok(())
        } else {
            Err(AgentProtocolError::Invalid(
                "security stamp initialization failed".to_string(),
            ))
        }
    }

    /// Verify authenticity, freshness and nonce uniqueness for one session.
    pub fn verify(
        &self,
        secret: &str,
        now_epoch: u64,
        replay: &mut ReplayGuard,
    ) -> Result<(), AgentProtocolError> {
        self.validate()?;
        let Some(stamp) = &self.security else {
            return Err(AgentProtocolError::Unauthorized(
                "secure agent envelope required".to_string(),
            ));
        };
        if secret.is_empty()
            || stamp.key_id.trim().is_empty()
            || stamp.nonce.trim().is_empty()
            || stamp.signature.is_empty()
        {
            return Err(AgentProtocolError::Unauthorized(
                "invalid agent security stamp".to_string(),
            ));
        }
        if stamp.issued_at_epoch.abs_diff(now_epoch) > MAX_AGENT_CLOCK_SKEW_SECS {
            return Err(AgentProtocolError::Unauthorized(
                "agent envelope timestamp outside security window".to_string(),
            ));
        }
        let expected = hmac_sha256_hex(secret.as_bytes(), &self.signed_payload()?);
        if !constant_time_equal(stamp.signature.as_bytes(), expected.as_bytes()) {
            return Err(AgentProtocolError::Unauthorized(
                "agent envelope signature mismatch".to_string(),
            ));
        }
        if !replay.record_once(&stamp.nonce) {
            return Err(AgentProtocolError::Unauthorized(
                "agent envelope nonce replayed".to_string(),
            ));
        }
        Ok(())
    }

    fn signed_payload(&self) -> Result<Vec<u8>, AgentProtocolError> {
        let mut unsigned = self.clone();
        if let Some(stamp) = unsigned.security.as_mut() {
            stamp.signature.clear();
        }
        serde_json::to_vec(&unsigned)
            .map_err(|error| AgentProtocolError::Invalid(error.to_string()))
    }

    pub fn validate(&self) -> Result<(), AgentProtocolError> {
        if self.version != AGENT_PROTOCOL_VERSION {
            return Err(AgentProtocolError::Version(self.version));
        }
        for (name, value) in [
            ("message_id", self.message_id.as_str()),
            ("correlation_id", self.correlation_id.as_str()),
            ("from", self.from.as_str()),
            ("to", self.to.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(AgentProtocolError::Invalid(format!("{name} is empty")));
            }
        }
        if let AgentMessage::Task(task) = &self.message {
            task.validate_context()?;
        }
        if let Some(governance) = &self.governance {
            governance.validate()?;
        }
        let encoded = serde_json::to_vec(self)
            .map_err(|error| AgentProtocolError::Invalid(error.to_string()))?;
        if encoded.len() > MAX_AGENT_FRAME_BYTES {
            return Err(AgentProtocolError::Invalid(
                "agent envelope exceeds size limit".to_string(),
            ));
        }
        Ok(())
    }

    pub fn with_parent(mut self, parent_id: impl Into<String>) -> Self {
        self.parent_id = Some(parent_id.into());
        self
    }

    pub fn with_governance(mut self, governance: GovernanceMetadata) -> Self {
        self.governance = Some(governance);
        self
    }
}

fn hmac_sha256_hex(key: &[u8], message: &[u8]) -> String {
    const BLOCK: usize = 64;
    let mut normalized = if key.len() > BLOCK {
        Sha256::digest(key).to_vec()
    } else {
        key.to_vec()
    };
    normalized.resize(BLOCK, 0);
    let mut inner_pad = [0x36u8; BLOCK];
    let mut outer_pad = [0x5cu8; BLOCK];
    for (index, byte) in normalized.iter().enumerate() {
        inner_pad[index] ^= byte;
        outer_pad[index] ^= byte;
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(64);
    for byte in outer.finalize() {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NegotiatedSession {
    pub protocol_version: u16,
    pub local_id: String,
    pub remote_id: String,
    pub remote_autonomy: AutonomyLevel,
    pub remote_roles: Vec<AgentRole>,
    pub remote_capabilities: Vec<AgentCapability>,
    pub max_in_flight: usize,
}

pub fn negotiate(
    local: &AgentHello,
    remote: &AgentHello,
) -> Result<NegotiatedSession, AgentProtocolError> {
    if local.agent_id.trim().is_empty() || remote.agent_id.trim().is_empty() {
        return Err(AgentProtocolError::Invalid("agent_id is empty".to_string()));
    }
    let protocol_version = local
        .protocol_versions
        .iter()
        .copied()
        .filter(|version| remote.protocol_versions.contains(version))
        .max()
        .ok_or(AgentProtocolError::NoCommonVersion)?;
    let max_in_flight = local.max_in_flight.min(remote.max_in_flight);
    if max_in_flight == 0 {
        return Err(AgentProtocolError::Invalid(
            "max_in_flight must be positive".to_string(),
        ));
    }
    Ok(NegotiatedSession {
        protocol_version,
        local_id: local.agent_id.clone(),
        remote_id: remote.agent_id.clone(),
        remote_autonomy: remote.autonomy.clone(),
        remote_roles: remote.roles.clone(),
        remote_capabilities: remote.capabilities.clone(),
        max_in_flight,
    })
}

pub fn authorize_task(
    session: &NegotiatedSession,
    task: &AgentTask,
) -> Result<(), AgentProtocolError> {
    let capabilities = session
        .remote_capabilities
        .iter()
        .map(|capability| (capability.name.as_str(), capability))
        .collect::<std::collections::BTreeMap<_, _>>();
    if let Some(missing) = task
        .required_capabilities
        .iter()
        .find(|capability| !capabilities.contains_key(capability.as_str()))
    {
        return Err(AgentProtocolError::Unauthorized(format!(
            "remote agent lacks capability {missing}"
        )));
    }
    if let Some(unknown_tool) = task
        .allowed_tools
        .iter()
        .find(|tool| !capabilities.contains_key(tool.as_str()))
    {
        return Err(AgentProtocolError::Unauthorized(format!(
            "remote agent did not negotiate tool {unknown_tool}"
        )));
    }
    if task.read_only
        && task.required_capabilities.iter().any(|capability| {
            capabilities
                .get(capability.as_str())
                .is_some_and(|capability| !capability.read_only)
        })
    {
        return Err(AgentProtocolError::Unauthorized(
            "read-only task cannot require a mutating capability".to_string(),
        ));
    }
    if task.read_only
        && task.allowed_tools.iter().any(|tool| {
            capabilities
                .get(tool.as_str())
                .is_some_and(|capability| !capability.read_only)
        })
    {
        return Err(AgentProtocolError::Unauthorized(
            "read-only task cannot use a mutating capability".to_string(),
        ));
    }
    if task.read_only
        && task.allowed_tools.iter().any(|tool| {
            matches!(
                tool.as_str(),
                "write_file" | "edit_file" | "apply_edits" | "run_shell"
            )
        })
    {
        return Err(AgentProtocolError::Unauthorized(
            "read-only task cannot request side-effecting tools".to_string(),
        ));
    }
    if !task.read_only && session.remote_autonomy == AutonomyLevel::ReadOnly {
        return Err(AgentProtocolError::Unauthorized(
            "read-only agent cannot accept a mutating task".to_string(),
        ));
    }
    if task.budget_steps == 0 {
        return Err(AgentProtocolError::Invalid(
            "budget_steps must be positive".to_string(),
        ));
    }
    Ok(())
}

pub fn authorize_governance(
    session: &NegotiatedSession,
    task: &AgentTask,
    governance: &GovernanceMetadata,
) -> Result<(), AgentProtocolError> {
    governance.validate()?;
    if governance.max_steps < task.budget_steps {
        return Err(AgentProtocolError::Unauthorized(
            "task budget exceeds governance limit".to_string(),
        ));
    }
    if matches!(governance.autonomy, AutonomyTier::Observe) && !task.read_only {
        return Err(AgentProtocolError::Unauthorized(
            "observe-only governance cannot run mutating tasks".to_string(),
        ));
    }
    if matches!(governance.autonomy, AutonomyTier::Suggest)
        && matches!(governance.approval, ApprovalRequirement::None)
        && !task.read_only
    {
        return Err(AgentProtocolError::Unauthorized(
            "suggest governance requires explicit approval for mutating tasks".to_string(),
        ));
    }
    if matches!(governance.autonomy, AutonomyTier::BoundedAutonomous)
        && session.remote_autonomy == AutonomyLevel::ReadOnly
        && !task.read_only
    {
        return Err(AgentProtocolError::Unauthorized(
            "read-only remote agent cannot run autonomous mutating tasks".to_string(),
        ));
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum AgentProtocolError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("invalid envelope: {0}")]
    Invalid(String),
    #[error("unsupported protocol version: {0}")]
    Version(u16),
    #[error("no common agent protocol version")]
    NoCommonVersion,
    #[error("agent exchange cancelled")]
    Cancelled,
    #[error("unauthorized agent task: {0}")]
    Unauthorized(String),
    #[error("agent exchange timed out")]
    Timeout,
    #[error("agent handler failed: {0}")]
    Handler(String),
}

#[async_trait::async_trait]
pub trait AgentTransport: Send {
    async fn send(&mut self, envelope: AgentEnvelope) -> Result<(), AgentProtocolError>;
    async fn recv(&mut self) -> Result<Option<AgentEnvelope>, AgentProtocolError>;
}

pub struct InProcessAgentTransport {
    tx: mpsc::Sender<AgentEnvelope>,
    rx: mpsc::Receiver<AgentEnvelope>,
}

pub fn in_process_pair() -> (InProcessAgentTransport, InProcessAgentTransport) {
    let (left_tx, left_rx) = mpsc::channel(32);
    let (right_tx, right_rx) = mpsc::channel(32);
    (
        InProcessAgentTransport {
            tx: left_tx,
            rx: right_rx,
        },
        InProcessAgentTransport {
            tx: right_tx,
            rx: left_rx,
        },
    )
}

#[async_trait::async_trait]
impl AgentTransport for InProcessAgentTransport {
    async fn send(&mut self, envelope: AgentEnvelope) -> Result<(), AgentProtocolError> {
        envelope.validate()?;
        self.tx
            .send(envelope)
            .await
            .map_err(|_| AgentProtocolError::Transport("in-process peer closed".to_string()))
    }

    async fn recv(&mut self) -> Result<Option<AgentEnvelope>, AgentProtocolError> {
        Ok(self.rx.recv().await)
    }
}

#[derive(Serialize, Deserialize)]
struct JsonRpcAgentMessage {
    jsonrpc: String,
    id: String,
    method: String,
    params: AgentEnvelope,
}

pub struct JsonRpcAgentTransport<R, W> {
    reader: BufReader<R>,
    writer: W,
}

async fn read_bounded_frame<R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> Result<Option<Vec<u8>>, AgentProtocolError> {
    let mut frame = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .await
            .map_err(|error| AgentProtocolError::Transport(error.to_string()))?;
        if available.is_empty() {
            return if frame.is_empty() {
                Ok(None)
            } else {
                Err(AgentProtocolError::Transport(
                    "truncated agent frame".to_string(),
                ))
            };
        }
        if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
            if frame.len() + newline + 1 > MAX_AGENT_FRAME_BYTES {
                return Err(AgentProtocolError::Invalid(
                    "agent frame exceeds size limit".to_string(),
                ));
            }
            frame.extend_from_slice(&available[..newline]);
            reader.consume(newline + 1);
            return Ok(Some(frame));
        }
        if frame.len() + available.len() > MAX_AGENT_FRAME_BYTES {
            return Err(AgentProtocolError::Invalid(
                "agent frame exceeds size limit".to_string(),
            ));
        }
        frame.extend_from_slice(available);
        let consumed = available.len();
        reader.consume(consumed);
    }
}

impl<R, W> JsonRpcAgentTransport<R, W>
where
    R: AsyncRead,
{
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader: BufReader::new(reader),
            writer,
        }
    }
}

#[async_trait::async_trait]
impl<R, W> AgentTransport for JsonRpcAgentTransport<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    async fn send(&mut self, envelope: AgentEnvelope) -> Result<(), AgentProtocolError> {
        envelope.validate()?;
        let message = JsonRpcAgentMessage {
            jsonrpc: "2.0".to_string(),
            id: envelope.message_id.clone(),
            method: "agent/message".to_string(),
            params: envelope,
        };
        let mut encoded = serde_json::to_vec(&message)
            .map_err(|error| AgentProtocolError::Invalid(error.to_string()))?;
        encoded.push(b'\n');
        self.writer
            .write_all(&encoded)
            .await
            .map_err(|error| AgentProtocolError::Transport(error.to_string()))?;
        self.writer
            .flush()
            .await
            .map_err(|error| AgentProtocolError::Transport(error.to_string()))
    }

    async fn recv(&mut self) -> Result<Option<AgentEnvelope>, AgentProtocolError> {
        let Some(frame) = read_bounded_frame(&mut self.reader).await? else {
            return Ok(None);
        };
        let message: JsonRpcAgentMessage = serde_json::from_slice(&frame)
            .map_err(|error| AgentProtocolError::Invalid(error.to_string()))?;
        if message.jsonrpc != "2.0"
            || message.method != "agent/message"
            || message.id != message.params.message_id
        {
            return Err(AgentProtocolError::Invalid(
                "invalid agent JSON-RPC envelope".to_string(),
            ));
        }
        message.params.validate()?;
        Ok(Some(message.params))
    }
}

pub type JsonRpcLoopbackTransport = JsonRpcAgentTransport<
    tokio::io::ReadHalf<tokio::io::DuplexStream>,
    tokio::io::WriteHalf<tokio::io::DuplexStream>,
>;

pub fn json_rpc_loopback_pair() -> (JsonRpcLoopbackTransport, JsonRpcLoopbackTransport) {
    let (left, right) = tokio::io::duplex(64 * 1024);
    let (left_read, left_write) = tokio::io::split(left);
    let (right_read, right_write) = tokio::io::split(right);
    (
        JsonRpcAgentTransport::new(left_read, left_write),
        JsonRpcAgentTransport::new(right_read, right_write),
    )
}

pub async fn handshake<C, S>(
    client: &mut C,
    server: &mut S,
    client_hello: AgentHello,
    server_hello: AgentHello,
) -> Result<NegotiatedSession, AgentProtocolError>
where
    C: AgentTransport,
    S: AgentTransport,
{
    client
        .send(AgentEnvelope::hello(
            "hello-client",
            client_hello.agent_id.clone(),
            server_hello.agent_id.clone(),
            client_hello.clone(),
        ))
        .await?;
    let incoming = server.recv().await?.ok_or_else(|| {
        AgentProtocolError::Transport("server closed during handshake".to_string())
    })?;
    incoming.validate()?;
    if incoming.message_id != "hello-client"
        || incoming.correlation_id != "handshake"
        || incoming.from != client_hello.agent_id
        || incoming.to != server_hello.agent_id
        || incoming.parent_id.is_some()
    {
        return Err(AgentProtocolError::Invalid(
            "client Hello identity mismatch".to_string(),
        ));
    }
    let AgentMessage::Hello(remote_hello) = incoming.message else {
        return Err(AgentProtocolError::Invalid(
            "expected client Hello".to_string(),
        ));
    };
    let server_session = negotiate(&server_hello, &remote_hello)?;
    server
        .send(AgentEnvelope::hello(
            "hello-server",
            server_hello.agent_id.clone(),
            client_hello.agent_id.clone(),
            server_hello.clone(),
        ))
        .await?;
    let response = client.recv().await?.ok_or_else(|| {
        AgentProtocolError::Transport("client closed during handshake".to_string())
    })?;
    response.validate()?;
    if response.message_id != "hello-server"
        || response.correlation_id != "handshake"
        || response.from != server_hello.agent_id
        || response.to != client_hello.agent_id
        || response.parent_id.is_some()
    {
        return Err(AgentProtocolError::Invalid(
            "server Hello identity mismatch".to_string(),
        ));
    }
    let AgentMessage::Hello(client_view_of_server) = response.message else {
        return Err(AgentProtocolError::Invalid(
            "expected server Hello".to_string(),
        ));
    };
    let client_session = negotiate(&client_hello, &client_view_of_server)?;
    if client_session.protocol_version != server_session.protocol_version {
        return Err(AgentProtocolError::NoCommonVersion);
    }
    Ok(client_session)
}

pub async fn exchange_once<C, S, F, Fut>(
    client: &mut C,
    server: &mut S,
    client_hello: AgentHello,
    server_hello: AgentHello,
    request: AgentEnvelope,
    handler: F,
) -> Result<AgentEnvelope, AgentProtocolError>
where
    C: AgentTransport,
    S: AgentTransport,
    F: FnOnce(AgentEnvelope) -> Fut,
    Fut: Future<Output = Result<AgentEnvelope, AgentProtocolError>>,
{
    exchange_once_with_cancellation(
        client,
        server,
        client_hello,
        server_hello,
        request,
        handler,
        None,
    )
    .await
}

pub async fn exchange_once_with_cancellation<C, S, F, Fut>(
    client: &mut C,
    server: &mut S,
    client_hello: AgentHello,
    server_hello: AgentHello,
    request: AgentEnvelope,
    handler: F,
    cancellation: Option<AgentCancellation>,
) -> Result<AgentEnvelope, AgentProtocolError>
where
    C: AgentTransport,
    S: AgentTransport,
    F: FnOnce(AgentEnvelope) -> Fut,
    Fut: Future<Output = Result<AgentEnvelope, AgentProtocolError>>,
{
    let session = handshake(client, server, client_hello, server_hello).await?;
    let AgentMessage::Task(task) = &request.message else {
        return Err(AgentProtocolError::Invalid(
            "exchange request must be Task".to_string(),
        ));
    };
    authorize_task(&session, task)?;
    if let Some(governance) = &request.governance {
        authorize_governance(&session, task, governance)?;
    }
    let correlation_id = request.correlation_id.clone();
    let request_message_id = request.message_id.clone();
    let request_from = request.from.clone();
    let request_to = request.to.clone();
    client.send(request).await?;
    let received = server
        .recv()
        .await?
        .ok_or_else(|| AgentProtocolError::Transport("server closed before Task".to_string()))?;
    received.validate()?;
    if received.message_id != request_message_id
        || received.correlation_id != correlation_id
        || received.from != request_from
        || received.to != request_to
        || !matches!(received.message, AgentMessage::Task(_))
    {
        return Err(AgentProtocolError::Invalid(
            "request identity mismatch".to_string(),
        ));
    }
    let response = if let Some(cancellation) = cancellation {
        tokio::select! {
            response = handler(received) => response?,
            _ = cancellation.cancelled() => {
                let cancel = AgentEnvelope::cancel(
                    format!("{request_message_id}:cancel"),
                    request_to.clone(),
                    request_from.clone(),
                    correlation_id.clone(),
                    "caller cancelled agent task",
                ).with_parent(&request_message_id);
                let _ = client.send(cancel).await;
                return Err(AgentProtocolError::Cancelled);
            }
        }
    } else {
        handler(received).await?
    };
    response.validate()?;
    if response.correlation_id != correlation_id
        || response.from != request_to
        || response.to != request_from
        || response.parent_id.as_deref() != Some(request_message_id.as_str())
        || !matches!(
            response.message,
            AgentMessage::Response(_) | AgentMessage::Error(_)
        )
    {
        return Err(AgentProtocolError::Invalid(
            "response identity mismatch".to_string(),
        ));
    }
    server.send(response).await?;
    client
        .recv()
        .await?
        .ok_or_else(|| AgentProtocolError::Transport("client closed before Response".to_string()))
}

pub async fn in_process_exchange<F, Fut>(
    client_hello: AgentHello,
    server_hello: AgentHello,
    request: AgentEnvelope,
    handler: F,
) -> Result<AgentEnvelope, AgentProtocolError>
where
    F: FnOnce(AgentEnvelope) -> Fut,
    Fut: Future<Output = Result<AgentEnvelope, AgentProtocolError>>,
{
    let (mut client, mut server) = in_process_pair();
    tokio::time::timeout(
        AGENT_EXCHANGE_TIMEOUT,
        exchange_once(
            &mut client,
            &mut server,
            client_hello,
            server_hello,
            request,
            handler,
        ),
    )
    .await
    .map_err(|_| AgentProtocolError::Timeout)?
}

pub async fn in_process_exchange_with_cancellation<F, Fut>(
    client_hello: AgentHello,
    server_hello: AgentHello,
    request: AgentEnvelope,
    handler: F,
    cancellation: AgentCancellation,
) -> Result<AgentEnvelope, AgentProtocolError>
where
    F: FnOnce(AgentEnvelope) -> Fut,
    Fut: Future<Output = Result<AgentEnvelope, AgentProtocolError>>,
{
    let (mut client, mut server) = in_process_pair();
    tokio::time::timeout(
        AGENT_EXCHANGE_TIMEOUT,
        exchange_once_with_cancellation(
            &mut client,
            &mut server,
            client_hello,
            server_hello,
            request,
            handler,
            Some(cancellation),
        ),
    )
    .await
    .map_err(|_| AgentProtocolError::Timeout)?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task() -> AgentEnvelope {
        AgentEnvelope::task(
            "task-1",
            "main",
            "worker",
            "corr-1",
            AgentTask::new("read README", true, vec!["read_file".to_string()], 4),
        )
    }

    #[test]
    fn task_context_round_trips_and_stays_bounded() {
        let mut context = std::collections::BTreeMap::new();
        context.insert("cwd".to_string(), "C:/code/ridge-code".to_string());
        context.insert("parent_goal".to_string(), "inspect protocol".to_string());
        let envelope = AgentEnvelope::task(
            "task-context",
            "main",
            "worker",
            "corr-context",
            AgentTask::new("inspect", true, vec!["read_file".to_string()], 1).with_context(context),
        );
        envelope.validate().expect("bounded context");
        let decoded: AgentEnvelope =
            serde_json::from_str(&serde_json::to_string(&envelope).expect("serialize"))
                .expect("deserialize");
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn envelope_rejects_oversized_context() {
        let mut context = std::collections::BTreeMap::new();
        context.insert(
            "payload".to_string(),
            "x".repeat(MAX_AGENT_CONTEXT_BYTES + 1),
        );
        let envelope = AgentEnvelope::task(
            "task-context",
            "main",
            "worker",
            "corr-context",
            AgentTask::new("inspect", true, Vec::new(), 1).with_context(context),
        );
        let error = envelope.validate().expect_err("context limit");
        assert!(
            matches!(error, AgentProtocolError::Invalid(message) if message.contains("context"))
        );
    }

    #[test]
    fn signed_envelope_rejects_tampering_expiry_and_replay() {
        let mut envelope = task();
        envelope
            .sign("worker-key", "shared-secret", "nonce-1", 1_000)
            .unwrap();
        let encoded = serde_json::to_string(&envelope).unwrap();
        let decoded: AgentEnvelope = serde_json::from_str(&encoded).unwrap();
        let mut replay = ReplayGuard::new();
        decoded.verify("shared-secret", 1_100, &mut replay).unwrap();
        assert!(matches!(
            decoded.verify("shared-secret", 1_100, &mut replay),
            Err(AgentProtocolError::Unauthorized(message)) if message.contains("replayed")
        ));

        let mut tampered = envelope.clone();
        tampered.to = "other-worker".into();
        assert!(matches!(
            tampered.verify("shared-secret", 1_100, &mut ReplayGuard::new()),
            Err(AgentProtocolError::Unauthorized(message)) if message.contains("signature")
        ));
        assert!(matches!(
            envelope.verify("shared-secret", 2_000, &mut ReplayGuard::new()),
            Err(AgentProtocolError::Unauthorized(message)) if message.contains("timestamp")
        ));
        assert!(matches!(
            task().verify("shared-secret", 1_000, &mut ReplayGuard::new()),
            Err(AgentProtocolError::Unauthorized(message)) if message.contains("required")
        ));
    }

    #[test]
    fn security_stamp_rejects_bad_inputs_and_replay_guard_is_bounded() {
        let mut envelope = task();
        assert!(envelope.sign("", "secret", "nonce", 1).is_err());
        assert!(envelope.sign("key", "", "nonce", 1).is_err());
        assert!(envelope.sign("key", "secret", "", 1).is_err());
        envelope.sign("key", "secret", "nonce", 1).unwrap();
        assert!(envelope
            .verify("wrong", 1, &mut ReplayGuard::new())
            .is_err());

        let mut replay = ReplayGuard::with_capacity(2);
        assert!(replay.record_once("a"));
        assert!(!replay.record_once("a"));
        assert!(replay.record_once("b"));
        assert!(replay.record_once("c"));
        assert!(replay.record_once("a"));
    }

    #[test]
    fn hmac_sha256_matches_standard_vector() {
        assert_eq!(
            hmac_sha256_hex(b"key", b"The quick brown fox jumps over the lazy dog"),
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    async fn exercise_transport<C, S>(mut client: C, mut server: S)
    where
        C: AgentTransport,
        S: AgentTransport,
    {
        let response = exchange_once(
            &mut client,
            &mut server,
            AgentHello::guarded("main", AgentRole::Maker),
            AgentHello::read_only("worker", AgentRole::Explorer),
            task(),
            |incoming| async move {
                let AgentMessage::Task(payload) = incoming.message else {
                    return Err(AgentProtocolError::Invalid("expected Task".to_string()));
                };
                Ok(AgentEnvelope::response(
                    "response-1",
                    "worker",
                    "main",
                    incoming.correlation_id,
                    AgentResponse {
                        status: AgentStatus::Done,
                        approved: true,
                        steps: 1,
                        tokens: 2,
                        summary: payload.task,
                        modified_files: Vec::new(),
                    },
                )
                .with_parent(&incoming.message_id))
            },
        )
        .await
        .expect("transport exchange");
        let AgentMessage::Response(response) = response.message else {
            panic!("expected Response");
        };
        assert!(response.approved);
        assert_eq!(response.summary, "read README");
    }

    #[tokio::test]
    async fn in_process_transport_completes_bounded_exchange() {
        let (client, server) = in_process_pair();
        exercise_transport(client, server).await;
    }

    #[tokio::test]
    async fn json_rpc_transport_completes_same_exchange() {
        let (client, server) = json_rpc_loopback_pair();
        exercise_transport(client, server).await;
    }

    #[tokio::test]
    async fn exchange_rejects_correlation_mismatch() {
        let (mut client, mut server) = in_process_pair();
        let result = exchange_once(
            &mut client,
            &mut server,
            AgentHello::guarded("main", AgentRole::Maker),
            AgentHello::read_only("worker", AgentRole::Explorer),
            task(),
            |incoming| async move {
                Ok(AgentEnvelope::response(
                    "response-1",
                    "worker",
                    "main",
                    "wrong-correlation",
                    AgentResponse {
                        status: AgentStatus::Done,
                        approved: true,
                        steps: 1,
                        tokens: 1,
                        summary: incoming.correlation_id,
                        modified_files: Vec::new(),
                    },
                )
                .with_parent(&incoming.message_id))
            },
        )
        .await;
        assert!(
            matches!(result, Err(AgentProtocolError::Invalid(message)) if message.contains("identity"))
        );
    }

    #[tokio::test]
    async fn caller_timeout_bounds_slow_agent_handler() {
        let (mut client, mut server) = in_process_pair();
        let result = tokio::time::timeout(
            Duration::from_millis(5),
            exchange_once(
                &mut client,
                &mut server,
                AgentHello::guarded("main", AgentRole::Maker),
                AgentHello::read_only("worker", AgentRole::Explorer),
                task(),
                |_incoming| async move {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    Err(AgentProtocolError::Timeout)
                },
            ),
        )
        .await;
        assert!(
            result.is_err(),
            "slow handler must be bounded by caller timeout"
        );
    }

    #[tokio::test]
    async fn concurrent_exchanges_keep_correlations_isolated() {
        async fn run(correlation_id: &'static str) -> String {
            let request = AgentEnvelope::task(
                format!("{correlation_id}:task"),
                "main",
                "worker",
                correlation_id,
                AgentTask::new("read README", true, vec!["read_file".to_string()], 1),
            );
            let response = in_process_exchange(
                AgentHello::guarded("main", AgentRole::Maker),
                AgentHello::read_only("worker", AgentRole::Explorer),
                request,
                |incoming| async move {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                    Ok(AgentEnvelope::response(
                        "response",
                        "worker",
                        "main",
                        incoming.correlation_id.clone(),
                        AgentResponse {
                            status: AgentStatus::Done,
                            approved: true,
                            steps: 1,
                            tokens: 1,
                            summary: "done".to_string(),
                            modified_files: Vec::new(),
                        },
                    )
                    .with_parent(&incoming.message_id))
                },
            )
            .await
            .expect("concurrent exchange");
            response.correlation_id
        }

        let (first, second) = tokio::join!(run("corr-a"), run("corr-b"));
        assert_eq!(first, "corr-a");
        assert_eq!(second, "corr-b");
    }

    #[tokio::test]
    async fn closed_peer_is_reported_as_transport_failure() {
        let (mut client, server) = in_process_pair();
        drop(server);
        let error = client.send(task()).await.expect_err("closed peer");
        assert!(
            matches!(error, AgentProtocolError::Transport(message) if message.contains("peer closed"))
        );
    }

    #[tokio::test]
    async fn handshake_negotiates_capabilities_and_enforces_read_only() {
        let (mut client, mut server) = in_process_pair();
        let session = handshake(
            &mut client,
            &mut server,
            AgentHello::guarded("main", AgentRole::Maker),
            AgentHello::read_only("worker", AgentRole::Explorer),
        )
        .await
        .expect("handshake");
        assert_eq!(session.protocol_version, AGENT_PROTOCOL_VERSION);
        assert_eq!(session.max_in_flight, 1);
        assert_eq!(session.remote_autonomy, AutonomyLevel::ReadOnly);
        assert_eq!(session.remote_roles, vec![AgentRole::Explorer]);
        assert!(session
            .remote_capabilities
            .iter()
            .any(|capability| capability.name == "read_file"));
        let mut mutating = task();
        if let AgentMessage::Task(payload) = &mut mutating.message {
            payload.read_only = false;
            payload.allowed_tools = vec!["write_file".to_string()];
        }
        let AgentMessage::Task(payload) = mutating.message else {
            unreachable!();
        };
        let error = authorize_task(&session, &payload).expect_err("read-only boundary");
        assert!(matches!(error, AgentProtocolError::Unauthorized(_)));
    }

    #[test]
    fn negotiation_chooses_highest_common_version() {
        let mut local = AgentHello::guarded("main", AgentRole::Maker);
        local.protocol_versions = vec![1, 2];
        let mut remote = AgentHello::read_only("worker", AgentRole::Explorer);
        remote.protocol_versions = vec![2, 1];
        let session = negotiate(&local, &remote).expect("common version");
        assert_eq!(session.protocol_version, 2);
    }

    #[test]
    fn read_only_task_rejects_side_effecting_tool_even_when_advertised() {
        let local = AgentHello::guarded("main", AgentRole::Maker);
        let mut remote = AgentHello::read_only("worker", AgentRole::Explorer);
        remote.capabilities.push(AgentCapability {
            name: "run_shell".to_string(),
            read_only: false,
        });
        let session = negotiate(&local, &remote).expect("handshake capabilities");
        let task = AgentTask::new("inspect", true, vec!["run_shell".to_string()], 1);
        let error = authorize_task(&session, &task).expect_err("read-only boundary");
        assert!(
            matches!(error, AgentProtocolError::Unauthorized(message) if message.contains("read-only task"))
        );
    }

    #[tokio::test]
    async fn cancellation_sends_correlated_cancel_envelope() {
        let (mut client, mut server) = in_process_pair();
        let cancellation = AgentCancellation::new();
        let trigger = cancellation.clone();
        let trigger_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            trigger.cancel();
        });
        let result = exchange_once_with_cancellation(
            &mut client,
            &mut server,
            AgentHello::guarded("main", AgentRole::Maker),
            AgentHello::read_only("worker", AgentRole::Explorer),
            task(),
            |_incoming| async move {
                tokio::time::sleep(Duration::from_secs(1)).await;
                Err(AgentProtocolError::Timeout)
            },
            Some(cancellation),
        )
        .await;
        trigger_task.await.expect("cancellation trigger");
        assert!(matches!(result, Err(AgentProtocolError::Cancelled)));
        let cancel = server
            .recv()
            .await
            .expect("cancel transport")
            .expect("cancel envelope");
        assert_eq!(cancel.correlation_id, "corr-1");
        assert_eq!(cancel.parent_id.as_deref(), Some("task-1"));
        assert!(matches!(cancel.message, AgentMessage::Cancel { .. }));
    }

    #[test]
    fn cancel_keeps_correlation_and_error_is_structured() {
        let cancel = AgentEnvelope::cancel("cancel-1", "main", "worker", "corr-1", "user takeover");
        assert_eq!(cancel.correlation_id, "corr-1");
        assert!(matches!(cancel.message, AgentMessage::Cancel { .. }));
        let error = AgentEnvelope::error(
            "error-1",
            "worker",
            "main",
            "corr-1",
            AgentError {
                code: "timeout".to_string(),
                message: "bounded exchange timed out".to_string(),
                retryable: false,
            },
        )
        .with_parent("task-1");
        let encoded = serde_json::to_string(&error).expect("serialize");
        assert!(encoded.contains("timeout"));
        assert!(encoded.contains("corr-1"));
    }
}
