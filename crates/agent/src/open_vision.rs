//! Bounded, offline-capable seams for the approved Open Vision slices.
//!
//! This module owns policy and durable projections only. It does not bypass
//! the existing permission gate, maker/checker flow, or graph execution.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::communication::{AgentEnvelope, AgentProtocolError, ReplayGuard};

const MAX_AUDIT_SUMMARY_BYTES: usize = 2 * 1024;
const MAX_EVENT_PAYLOAD_BYTES: usize = 32 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityAuditEntry {
    pub message_id: String,
    pub correlation_id: String,
    pub from: String,
    pub to: String,
    pub key_id: Option<String>,
    pub outcome: String,
}

#[derive(Clone, Debug)]
pub struct SecurityAuditLog {
    capacity: usize,
    entries: VecDeque<SecurityAuditEntry>,
}

impl SecurityAuditLog {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: VecDeque::new(),
        }
    }

    pub fn entries(&self) -> impl Iterator<Item = &SecurityAuditEntry> {
        self.entries.iter()
    }

    fn record(&mut self, envelope: &AgentEnvelope, outcome: &str) {
        let entry = SecurityAuditEntry {
            message_id: envelope.message_id.clone(),
            correlation_id: envelope.correlation_id.clone(),
            from: envelope.from.clone(),
            to: envelope.to.clone(),
            key_id: envelope.security.as_ref().map(|stamp| stamp.key_id.clone()),
            outcome: outcome.to_string(),
        };
        self.entries.push_back(entry);
        while self.entries.len() > self.capacity {
            self.entries.pop_front();
        }
    }
}

impl Default for SecurityAuditLog {
    fn default() -> Self {
        Self::new(256)
    }
}

/// Authenticated verification with key-id binding and bounded, secret-free
/// audit records. A raw shared secret never enters the audit projection.
pub struct AgentVerifier {
    key_id: String,
    secret: String,
    replay: ReplayGuard,
    audit: SecurityAuditLog,
}

impl AgentVerifier {
    pub fn new(key_id: impl Into<String>, secret: impl Into<String>) -> Self {
        Self {
            key_id: key_id.into(),
            secret: secret.into(),
            replay: ReplayGuard::new(),
            audit: SecurityAuditLog::default(),
        }
    }

    pub fn with_audit_capacity(
        key_id: impl Into<String>,
        secret: impl Into<String>,
        capacity: usize,
    ) -> Self {
        Self {
            key_id: key_id.into(),
            secret: secret.into(),
            replay: ReplayGuard::new(),
            audit: SecurityAuditLog::new(capacity),
        }
    }

    pub fn audit(&self) -> &SecurityAuditLog {
        &self.audit
    }

    pub fn verify(
        &mut self,
        envelope: &AgentEnvelope,
        now_epoch: u64,
    ) -> Result<(), AgentProtocolError> {
        let key_matches = envelope
            .security
            .as_ref()
            .is_some_and(|stamp| stamp.key_id == self.key_id);
        let result = if !key_matches {
            Err(AgentProtocolError::Unauthorized(
                "agent envelope key id is not trusted".to_string(),
            ))
        } else {
            envelope.verify(&self.secret, now_epoch, &mut self.replay)
        };
        self.audit.record(
            envelope,
            if result.is_ok() {
                "accepted"
            } else {
                "rejected"
            },
        );
        result
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnicodeRenderMode {
    Native,
    SafeFallback,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnicodeCapabilities {
    pub mode: UnicodeRenderMode,
    pub grapheme_clusters: bool,
    pub wide_cells: bool,
}

impl UnicodeCapabilities {
    pub fn detect() -> Self {
        Self::from_environment(std::env::vars())
    }

    pub fn from_environment<I, K, V>(variables: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let known_terminal = variables.into_iter().any(|(key, value)| {
            matches!(key.as_ref(), "WT_SESSION" | "TERM_PROGRAM" | "COLORTERM")
                && !value.as_ref().trim().is_empty()
        });
        if known_terminal {
            Self {
                mode: UnicodeRenderMode::Native,
                grapheme_clusters: true,
                wide_cells: true,
            }
        } else {
            Self {
                mode: UnicodeRenderMode::SafeFallback,
                grapheme_clusters: true,
                wide_cells: false,
            }
        }
    }

    pub fn display_width(&self, text: &str) -> usize {
        match self.mode {
            UnicodeRenderMode::Native => UnicodeWidthStr::width(text),
            UnicodeRenderMode::SafeFallback => text
                .graphemes(true)
                .map(|grapheme| usize::from(grapheme != "\n"))
                .sum(),
        }
    }

    /// Replace unsupported graphemes with one-cell placeholders, preserving
    /// line boundaries and preventing fallback-mode layout drift.
    pub fn render_safe(&self, text: &str) -> String {
        if self.mode == UnicodeRenderMode::Native {
            return text.to_string();
        }
        text.graphemes(true)
            .map(|grapheme| {
                if grapheme == "\n" || grapheme.is_ascii() {
                    grapheme.to_string()
                } else {
                    "?".to_string()
                }
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningNodeKind {
    GraphNode,
    Plan,
    Tool,
    Observation,
    Decision,
    Verification,
    Answer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningNodeStatus {
    Started,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningNode {
    pub id: String,
    pub parent_id: Option<String>,
    pub kind: ReasoningNodeKind,
    pub status: ReasoningNodeStatus,
    pub summary: String,
    pub superstep: usize,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
pub struct ReasoningTree {
    max_nodes: usize,
    nodes: Vec<ReasoningNode>,
    ids: BTreeSet<String>,
}

impl ReasoningTree {
    pub fn new(max_nodes: usize) -> Self {
        Self {
            max_nodes: max_nodes.max(1),
            nodes: Vec::new(),
            ids: BTreeSet::new(),
        }
    }

    pub fn nodes(&self) -> &[ReasoningNode] {
        &self.nodes
    }

    pub fn append(&mut self, node: ReasoningNode) -> Result<(), OpenVisionError> {
        if node.id.trim().is_empty() || self.ids.contains(&node.id) {
            return Err(OpenVisionError::InvalidReasoningNode(
                "reasoning node id must be unique and non-empty".to_string(),
            ));
        }
        if node.summary.len() > MAX_AUDIT_SUMMARY_BYTES {
            return Err(OpenVisionError::InvalidReasoningNode(
                "reasoning summary exceeds size limit".to_string(),
            ));
        }
        if node
            .parent_id
            .as_ref()
            .is_some_and(|parent| !self.ids.contains(parent))
        {
            return Err(OpenVisionError::InvalidReasoningNode(
                "reasoning parent does not exist".to_string(),
            ));
        }
        if self.nodes.len() >= self.max_nodes {
            return Err(OpenVisionError::Limit(
                "reasoning tree node limit".to_string(),
            ));
        }
        self.ids.insert(node.id.clone());
        self.nodes.push(node);
        Ok(())
    }

    pub fn audit(&self) -> Result<ReasoningAuditReport, OpenVisionError> {
        let bytes = serde_json::to_vec(&self.nodes)?;
        let mut digest = Sha256::new();
        digest.update(bytes);
        let hash = hex_digest(digest.finalize().as_slice());
        let roots = self
            .nodes
            .iter()
            .filter(|node| node.parent_id.is_none())
            .count();
        let leaves = self
            .nodes
            .iter()
            .filter(|node| {
                !self
                    .nodes
                    .iter()
                    .any(|other| other.parent_id.as_deref() == Some(node.id.as_str()))
            })
            .count();
        Ok(ReasoningAuditReport {
            node_count: self.nodes.len(),
            roots,
            leaves,
            sha256: hash,
        })
    }
}

impl Default for ReasoningTree {
    fn default() -> Self {
        Self::new(1024)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningAuditReport {
    pub node_count: usize,
    pub roots: usize,
    pub leaves: usize,
    pub sha256: String,
}

/// Adapter boundary for langgraph node/superstep events. The graph engine
/// remains generic; callers explicitly project only safe summaries here.
pub struct GraphReasoningBridge {
    tree: ReasoningTree,
}

impl GraphReasoningBridge {
    pub fn new(max_nodes: usize) -> Self {
        Self {
            tree: ReasoningTree::new(max_nodes),
        }
    }

    pub fn record_node(
        &mut self,
        superstep: usize,
        node: impl Into<String>,
        parent_id: Option<String>,
        status: ReasoningNodeStatus,
        summary: impl Into<String>,
    ) -> Result<(), OpenVisionError> {
        let node = node.into();
        self.tree.append(ReasoningNode {
            id: format!("graph:{superstep}:{node}"),
            parent_id,
            kind: ReasoningNodeKind::GraphNode,
            status,
            summary: summary.into(),
            superstep,
            metadata: BTreeMap::new(),
        })
    }

    pub fn tree(&self) -> &ReasoningTree {
        &self.tree
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ReasoningAuditRecord {
    previous_hash: String,
    node: ReasoningNode,
    hash: String,
}

pub struct ReasoningAuditLog {
    path: PathBuf,
    previous_hash: String,
    records: usize,
}

impl ReasoningAuditLog {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, OpenVisionError> {
        let path = path.into();
        let mut previous_hash = String::new();
        let mut records = 0;
        if path.exists() {
            let file = File::open(&path)?;
            for line in BufReader::new(file).lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let record: ReasoningAuditRecord = serde_json::from_str(&line)?;
                let expected = audit_record_hash(&record.previous_hash, &record.node)?;
                if record.previous_hash != previous_hash || record.hash != expected {
                    return Err(OpenVisionError::AuditIntegrity);
                }
                previous_hash = record.hash;
                records += 1;
            }
        }
        Ok(Self {
            path,
            previous_hash,
            records,
        })
    }

    pub fn append(&mut self, node: ReasoningNode) -> Result<String, OpenVisionError> {
        if node.summary.len() > MAX_AUDIT_SUMMARY_BYTES {
            return Err(OpenVisionError::InvalidReasoningNode(
                "reasoning summary exceeds size limit".to_string(),
            ));
        }
        let hash = audit_record_hash(&self.previous_hash, &node)?;
        let record = ReasoningAuditRecord {
            previous_hash: self.previous_hash.clone(),
            node,
            hash: hash.clone(),
        };
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        serde_json::to_writer(&mut file, &record)?;
        file.write_all(b"\n")?;
        file.flush()?;
        self.previous_hash = hash.clone();
        self.records += 1;
        Ok(hash)
    }

    pub fn records(&self) -> usize {
        self.records
    }

    pub fn verify(path: impl AsRef<Path>) -> Result<usize, OpenVisionError> {
        let log = Self::open(path.as_ref().to_path_buf())?;
        Ok(log.records)
    }
}

fn audit_record_hash(previous_hash: &str, node: &ReasoningNode) -> Result<String, OpenVisionError> {
    let bytes = serde_json::to_vec(&(previous_hash, node))?;
    let mut digest = Sha256::new();
    digest.update(bytes);
    Ok(hex_digest(digest.finalize().as_slice()))
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[derive(Debug, Error)]
pub enum OpenVisionError {
    #[error("open vision io: {0}")]
    Io(#[from] std::io::Error),
    #[error("open vision json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("open vision protocol: {0}")]
    Protocol(#[from] AgentProtocolError),
    #[error("open vision limit: {0}")]
    Limit(String),
    #[error("invalid reasoning node: {0}")]
    InvalidReasoningNode(String),
    #[error("reasoning audit integrity check failed")]
    AuditIntegrity,
    #[error("event stream takeover owned by another actor")]
    TakeoverOwned,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnqueueResult {
    Queued,
    Duplicate,
}

/// Durable newline-delimited store-and-forward queue. It validates and
/// deduplicates envelopes before replacing the queue file atomically.
pub struct StoreForwardQueue {
    path: PathBuf,
    max_items: usize,
    entries: VecDeque<AgentEnvelope>,
}

impl StoreForwardQueue {
    pub fn open(path: impl Into<PathBuf>, max_items: usize) -> Result<Self, OpenVisionError> {
        let path = path.into();
        let mut entries = VecDeque::new();
        if path.exists() {
            let file = File::open(&path)?;
            for line in BufReader::new(file).lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let envelope: AgentEnvelope = serde_json::from_str(&line)?;
                envelope.validate()?;
                if entries
                    .iter()
                    .any(|item: &AgentEnvelope| item.message_id == envelope.message_id)
                {
                    continue;
                }
                if entries.len() >= max_items.max(1) {
                    return Err(OpenVisionError::Limit(
                        "store-forward queue capacity".to_string(),
                    ));
                }
                entries.push_back(envelope);
            }
        }
        Ok(Self {
            path,
            max_items: max_items.max(1),
            entries,
        })
    }

    pub fn pending(&self) -> Vec<AgentEnvelope> {
        self.entries.iter().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn enqueue(&mut self, envelope: AgentEnvelope) -> Result<EnqueueResult, OpenVisionError> {
        envelope.validate()?;
        if self
            .entries
            .iter()
            .any(|item| item.message_id == envelope.message_id)
        {
            return Ok(EnqueueResult::Duplicate);
        }
        if self.entries.len() >= self.max_items {
            return Err(OpenVisionError::Limit(
                "store-forward queue capacity".to_string(),
            ));
        }
        self.entries.push_back(envelope);
        self.persist()?;
        Ok(EnqueueResult::Queued)
    }

    pub fn drain(&self, limit: usize) -> Vec<AgentEnvelope> {
        self.entries.iter().take(limit).cloned().collect()
    }

    pub fn ack(&mut self, message_id: &str) -> Result<bool, OpenVisionError> {
        let Some(index) = self
            .entries
            .iter()
            .position(|item| item.message_id == message_id)
        else {
            return Ok(false);
        };
        self.entries.remove(index);
        self.persist()?;
        Ok(true)
    }

    fn persist(&self) -> Result<(), OpenVisionError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp = self
            .path
            .with_extension(format!("tmp-{}", std::process::id()));
        {
            let mut file = File::create(&temp)?;
            for envelope in &self.entries {
                serde_json::to_writer(&mut file, envelope)?;
                file.write_all(b"\n")?;
            }
            file.flush()?;
        }
        fs::rename(temp, &self.path)?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FederationPeer {
    pub peer_id: String,
    pub endpoint: String,
}

pub struct FederatedOutbox {
    queue: StoreForwardQueue,
    peers: BTreeMap<String, FederationPeer>,
}

impl FederatedOutbox {
    pub fn new(queue: StoreForwardQueue) -> Self {
        Self {
            queue,
            peers: BTreeMap::new(),
        }
    }

    pub fn register_peer(&mut self, peer: FederationPeer) -> Result<(), OpenVisionError> {
        if peer.peer_id.trim().is_empty() || peer.endpoint.trim().is_empty() {
            return Err(OpenVisionError::Limit(
                "federation peer identity".to_string(),
            ));
        }
        self.peers.insert(peer.peer_id.clone(), peer);
        Ok(())
    }

    pub fn enqueue_for(
        &mut self,
        peer_id: &str,
        envelope: AgentEnvelope,
    ) -> Result<EnqueueResult, OpenVisionError> {
        if !self.peers.contains_key(peer_id) || envelope.to != peer_id {
            return Err(OpenVisionError::Protocol(AgentProtocolError::Unauthorized(
                "federation peer is not registered for envelope target".to_string(),
            )));
        }
        self.queue.enqueue(envelope)
    }

    pub fn queue(&self) -> &StoreForwardQueue {
        &self.queue
    }

    pub fn queue_mut(&mut self) -> &mut StoreForwardQueue {
        &mut self.queue
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebEventKind {
    Snapshot,
    Delta,
    TakeoverRequested,
    TakeoverGranted,
    TakeoverReleased,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebEvent {
    pub sequence: u64,
    pub kind: WebEventKind,
    pub payload: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TakeoverLease {
    actor_id: String,
    correlation_id: String,
}

pub struct BoundedEventStream {
    capacity: usize,
    max_payload_bytes: usize,
    next_sequence: u64,
    events: VecDeque<WebEvent>,
    takeover: Option<TakeoverLease>,
}

impl BoundedEventStream {
    pub fn new(capacity: usize, max_payload_bytes: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            max_payload_bytes: max_payload_bytes.clamp(1, MAX_EVENT_PAYLOAD_BYTES),
            next_sequence: 1,
            events: VecDeque::new(),
            takeover: None,
        }
    }

    pub fn publish(
        &mut self,
        kind: WebEventKind,
        payload: impl Into<String>,
    ) -> Result<u64, OpenVisionError> {
        let payload = payload.into();
        if payload.len() > self.max_payload_bytes {
            return Err(OpenVisionError::Limit("web event payload".to_string()));
        }
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.events.push_back(WebEvent {
            sequence,
            kind,
            payload,
        });
        while self.events.len() > self.capacity {
            self.events.pop_front();
        }
        Ok(sequence)
    }

    pub fn since(&self, sequence: u64, limit: usize) -> Vec<WebEvent> {
        self.events
            .iter()
            .filter(|event| event.sequence > sequence)
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn request_takeover(
        &mut self,
        actor_id: impl Into<String>,
        correlation_id: impl Into<String>,
    ) -> Result<bool, OpenVisionError> {
        let lease = TakeoverLease {
            actor_id: actor_id.into(),
            correlation_id: correlation_id.into(),
        };
        if lease.actor_id.trim().is_empty() || lease.correlation_id.trim().is_empty() {
            return Err(OpenVisionError::Limit("takeover identity".to_string()));
        }
        if self
            .takeover
            .as_ref()
            .is_some_and(|current| current != &lease)
        {
            return Ok(false);
        }
        let granted = self.takeover.is_none();
        self.takeover = Some(lease.clone());
        if granted {
            let payload = format!("{}:{}", lease.actor_id, lease.correlation_id);
            self.publish(WebEventKind::TakeoverGranted, payload)?;
        }
        Ok(true)
    }

    pub fn release_takeover(
        &mut self,
        actor_id: &str,
        correlation_id: &str,
    ) -> Result<bool, OpenVisionError> {
        let Some(current) = self.takeover.as_ref() else {
            return Ok(false);
        };
        if current.actor_id != actor_id || current.correlation_id != correlation_id {
            return Err(OpenVisionError::TakeoverOwned);
        }
        self.takeover = None;
        self.publish(
            WebEventKind::TakeoverReleased,
            format!("{actor_id}:{correlation_id}"),
        )?;
        Ok(true)
    }

    pub fn takeover_active(&self) -> bool {
        self.takeover.is_some()
    }
}

impl Default for BoundedEventStream {
    fn default() -> Self {
        Self::new(256, MAX_EVENT_PAYLOAD_BYTES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::communication::{
        authorize_governance, negotiate, AgentHello, AgentRole, AgentTask, ApprovalRequirement,
        AutonomyTier, GovernanceMetadata,
    };

    fn task_envelope(id: &str, to: &str) -> AgentEnvelope {
        AgentEnvelope::task(
            id,
            "main",
            to,
            id,
            AgentTask::new("inspect", true, vec!["read_file".to_string()], 2),
        )
    }

    #[test]
    fn verifier_binds_key_and_keeps_secret_out_of_audit() {
        let mut envelope = task_envelope("msg-1", "worker");
        envelope
            .sign("key-a", "secret", "nonce-a", 100)
            .expect("sign");
        let mut verifier = AgentVerifier::with_audit_capacity("key-a", "secret", 1);
        verifier.verify(&envelope, 100).expect("verify");
        assert!(verifier.audit().entries().next().unwrap().outcome == "accepted");
        assert!(!format!("{:?}", verifier.audit()).contains("secret"));

        let mut wrong_key = AgentVerifier::new("key-b", "secret");
        assert!(wrong_key.verify(&envelope, 100).is_err());
        assert_eq!(wrong_key.audit().entries().count(), 1);
    }

    #[test]
    fn unicode_detection_has_native_and_safe_fallback_paths() {
        let native =
            UnicodeCapabilities::from_environment([("WT_SESSION", "1"), ("TERM_PROGRAM", "")]);
        assert_eq!(native.mode, UnicodeRenderMode::Native);
        assert_eq!(native.display_width("你🚀"), 4);

        let fallback = UnicodeCapabilities::from_environment([("TERM", "dumb")]);
        assert_eq!(fallback.mode, UnicodeRenderMode::SafeFallback);
        assert_eq!(fallback.render_safe("你🚀\nA"), "??\nA");
        assert_eq!(fallback.display_width("你🚀"), 2);
    }

    #[test]
    fn graph_bridge_and_audit_tree_enforce_parent_and_bounds() {
        let mut bridge = GraphReasoningBridge::new(2);
        bridge
            .record_node(1, "reason", None, ReasoningNodeStatus::Completed, "plan")
            .expect("root");
        bridge
            .record_node(
                2,
                "act",
                Some("graph:1:reason".to_string()),
                ReasoningNodeStatus::Completed,
                "tool result",
            )
            .expect("child");
        assert_eq!(bridge.tree().audit().unwrap().leaves, 1);
        assert!(bridge
            .tree()
            .nodes()
            .iter()
            .all(|node| node.summary != "hidden reasoning"));
    }

    #[test]
    fn reasoning_tree_rejects_missing_parent_and_overflow() {
        let mut tree = ReasoningTree::new(1);
        let bad = ReasoningNode {
            id: "child".into(),
            parent_id: Some("missing".into()),
            kind: ReasoningNodeKind::Decision,
            status: ReasoningNodeStatus::Started,
            summary: "x".into(),
            superstep: 1,
            metadata: BTreeMap::new(),
        };
        assert!(tree.append(bad).is_err());
        tree.append(ReasoningNode {
            id: "root".into(),
            parent_id: None,
            kind: ReasoningNodeKind::Plan,
            status: ReasoningNodeStatus::Completed,
            summary: "x".into(),
            superstep: 1,
            metadata: BTreeMap::new(),
        })
        .unwrap();
        assert!(tree
            .append(ReasoningNode {
                id: "overflow".into(),
                parent_id: Some("root".into()),
                kind: ReasoningNodeKind::Answer,
                status: ReasoningNodeStatus::Completed,
                summary: "x".into(),
                superstep: 2,
                metadata: BTreeMap::new(),
            })
            .is_err());
    }

    #[test]
    fn offline_reasoning_audit_is_hash_chained_and_detects_tampering() {
        let path = test_path("reasoning-audit");
        let _ = fs::remove_file(&path);
        let node = ReasoningNode {
            id: "n1".into(),
            parent_id: None,
            kind: ReasoningNodeKind::Observation,
            status: ReasoningNodeStatus::Completed,
            summary: "safe summary".into(),
            superstep: 1,
            metadata: BTreeMap::new(),
        };
        let mut log = ReasoningAuditLog::open(&path).unwrap();
        let first_hash = log.append(node).unwrap();
        assert_eq!(first_hash.len(), 64);
        assert_eq!(ReasoningAuditLog::verify(&path).unwrap(), 1);
        fs::write(&path, "{}\n").unwrap();
        assert!(matches!(
            ReasoningAuditLog::verify(&path),
            Err(OpenVisionError::Json(_))
        ));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn store_forward_queue_persists_deduplicates_and_acks() {
        let path = test_path("store-forward");
        let _ = fs::remove_file(&path);
        let envelope = task_envelope("queued-1", "worker");
        let mut queue = StoreForwardQueue::open(&path, 1).unwrap();
        assert_eq!(
            queue.enqueue(envelope.clone()).unwrap(),
            EnqueueResult::Queued
        );
        assert_eq!(queue.enqueue(envelope).unwrap(), EnqueueResult::Duplicate);
        assert_eq!(StoreForwardQueue::open(&path, 1).unwrap().len(), 1);
        assert!(queue.drain(1)[0].message_id == "queued-1");
        assert!(queue.ack("queued-1").unwrap());
        assert!(queue.is_empty());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn federation_rejects_unknown_or_wrong_target_peer() {
        let path = test_path("federated-outbox");
        let _ = fs::remove_file(&path);
        let queue = StoreForwardQueue::open(&path, 2).unwrap();
        let mut outbox = FederatedOutbox::new(queue);
        outbox
            .register_peer(FederationPeer {
                peer_id: "worker".into(),
                endpoint: "offline://worker".into(),
            })
            .unwrap();
        assert!(outbox
            .enqueue_for("worker", task_envelope("m1", "other"))
            .is_err());
        assert!(outbox
            .enqueue_for("missing", task_envelope("m2", "missing"))
            .is_err());
        assert_eq!(
            outbox
                .enqueue_for("worker", task_envelope("m3", "worker"))
                .unwrap(),
            EnqueueResult::Queued
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn bounded_web_stream_evicts_old_events_and_serializes_takeover() {
        let mut stream = BoundedEventStream::new(2, 8);
        assert_eq!(stream.publish(WebEventKind::Delta, "one").unwrap(), 1);
        assert_eq!(stream.publish(WebEventKind::Delta, "two").unwrap(), 2);
        assert_eq!(stream.publish(WebEventKind::Delta, "three").unwrap(), 3);
        assert!(stream.since(0, 10).iter().all(|event| event.sequence > 1));
        assert!(stream.request_takeover("web", "corr").unwrap());
        assert!(!stream.request_takeover("other", "corr-2").unwrap());
        assert!(matches!(
            stream.release_takeover("other", "corr-2"),
            Err(OpenVisionError::TakeoverOwned)
        ));
        assert!(stream.release_takeover("web", "corr").unwrap());
        assert!(stream.publish(WebEventKind::Delta, "too long!").is_err());
    }

    #[test]
    fn governed_envelope_round_trips_and_requires_explicit_approval() {
        let mut envelope = task_envelope("gov-1", "worker").with_governance(GovernanceMetadata {
            policy_id: "policy-1".into(),
            audit_id: "audit-1".into(),
            autonomy: AutonomyTier::ActWithApproval,
            approval: ApprovalRequirement::Explicit,
            approval_granted: false,
            max_steps: 2,
        });
        assert!(envelope.validate().is_err());
        envelope.governance.as_mut().unwrap().approval_granted = true;
        let encoded = serde_json::to_string(&envelope).unwrap();
        let decoded: AgentEnvelope = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.governance.unwrap().policy_id, "policy-1");
        let session = negotiate(
            &AgentHello::guarded("main", AgentRole::Maker),
            &AgentHello::guarded("worker", AgentRole::Worker),
        )
        .unwrap();
        let mutating = AgentTask::new("bounded edit", false, vec!["bounded_task".into()], 2);
        authorize_governance(
            &session,
            &mutating,
            &GovernanceMetadata {
                policy_id: "policy-1".into(),
                audit_id: "audit-1".into(),
                autonomy: AutonomyTier::ActWithApproval,
                approval: ApprovalRequirement::Explicit,
                approval_granted: true,
                max_steps: 2,
            },
        )
        .expect("approved governance");
    }

    fn test_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ridgecode-open-vision-{name}-{}",
            std::process::id()
        ))
    }
}
