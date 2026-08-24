//! # eval —— agent 的 eval harness
//!
//! loop engineering 的核心是「验证者才是瓶颈」:光跑通一次不算数,要能**批量**度量成功率与成本。
//! 这里给最小的度量闭环:一组 case,每个跑一遍 agent,统计 pass-rate + token 成本。
//!
//! case 的 provider 可以是离线 `ScriptedProvider`(CI 零联网、确定性),也可以是真实模型
//! (量真实成功率/成本)。验收仍走 agent 的**确定性闸**(`approved`),不看模型自述。

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agent::{build_llm_agent, AgentState};
use langgraph::{MemoryCheckpointer, RunConfig};
use provider::LlmProvider;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 一个 eval case:名字 + 任务 + 用哪个 provider 跑。
pub struct EvalCase {
    pub name: String,
    pub task: String,
    pub provider: Arc<dyn LlmProvider>,
    pub revision: Option<String>,
}

/// Constraint evaluated against one completed case.
///
/// The harness reports only the kind and numeric outcome of a check.  Marker
/// text is used for matching, but is never copied into evidence or logs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Invariant {
    Approved,
    MaxSteps(usize),
    MaxTokens(usize),
    Marker(String),
}

impl Invariant {
    pub fn approved() -> Self {
        Self::Approved
    }

    pub fn max_steps(limit: usize) -> Self {
        Self::MaxSteps(limit)
    }

    pub fn max_tokens(limit: usize) -> Self {
        Self::MaxTokens(limit)
    }

    pub fn marker(value: impl Into<String>) -> Self {
        Self::Marker(value.into())
    }

    pub fn steps_at_most(limit: usize) -> Self {
        Self::MaxSteps(limit)
    }

    pub fn tokens_at_most(limit: usize) -> Self {
        Self::MaxTokens(limit)
    }

    pub fn marker_present(value: impl Into<String>) -> Self {
        Self::Marker(value.into())
    }
}

/// Stable, non-sensitive category for one invariant observation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvariantKind {
    Approved,
    Steps,
    Tokens,
    Marker,
}

/// Bounded evidence for an invariant.  `observed` is a boolean (0/1) for
/// approval/marker checks and the measured count for step/token checks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvariantEvidence {
    pub kind: InvariantKind,
    pub passed: bool,
    pub observed: usize,
    pub limit: Option<usize>,
}

/// Alias used by callers that call these checks assertions rather than
/// invariants.
pub type InvariantResult = InvariantEvidence;

/// Per-case execution status for the options-based harness.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaseStatus {
    Completed,
    TimedOut,
    Failed,
}

/// Bounded harness controls. `run_eval` uses the default instance; pass custom
/// options to [`run_eval_with_options`] for tighter concurrency, timeout, and
/// invariant evidence policies.
#[derive(Clone, Debug)]
pub struct HarnessOptions {
    pub max_concurrency: usize,
    pub case_timeout: Duration,
    pub invariants: Vec<Invariant>,
    pub max_invariants: usize,
    pub manifest_path: Option<PathBuf>,
}

const MAX_HARNESS_CONCURRENCY: usize = 32;

impl Default for HarnessOptions {
    fn default() -> Self {
        Self {
            max_concurrency: 1,
            case_timeout: Duration::from_secs(30),
            invariants: Vec::new(),
            max_invariants: 32,
            manifest_path: None,
        }
    }
}

impl HarnessOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_max_concurrency(mut self, value: usize) -> Self {
        self.max_concurrency = value.clamp(1, MAX_HARNESS_CONCURRENCY);
        self
    }

    pub fn with_concurrency(self, value: usize) -> Self {
        self.with_max_concurrency(value)
    }

    pub fn with_case_timeout(mut self, value: Duration) -> Self {
        self.case_timeout = value;
        self
    }

    pub fn with_timeout(self, value: Duration) -> Self {
        self.with_case_timeout(value)
    }

    pub fn without_timeout(mut self) -> Self {
        self.case_timeout = Duration::ZERO;
        self
    }

    pub fn with_invariant(mut self, invariant: Invariant) -> Self {
        self.invariants.push(invariant);
        self
    }

    pub fn with_invariants(mut self, invariants: impl IntoIterator<Item = Invariant>) -> Self {
        self.invariants.extend(invariants);
        self
    }

    pub fn require_approved(self) -> Self {
        self.with_invariant(Invariant::Approved)
    }

    pub fn require_max_steps(self, limit: usize) -> Self {
        self.with_invariant(Invariant::MaxSteps(limit))
    }

    pub fn require_max_tokens(self, limit: usize) -> Self {
        self.with_invariant(Invariant::MaxTokens(limit))
    }

    pub fn require_marker(self, marker: impl Into<String>) -> Self {
        self.with_invariant(Invariant::Marker(marker.into()))
    }

    pub fn with_max_invariants(mut self, value: usize) -> Self {
        self.max_invariants = value;
        self
    }

    /// Use an append-only JSONL manifest for crash-safe case reuse.
    ///
    /// The path is validated against the process cwd when the harness starts;
    /// storing it here keeps this builder infallible and preserves the legacy
    /// options API.
    pub fn with_manifest_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.manifest_path = Some(path.into());
        self
    }

    pub fn with_manifest(self, path: impl Into<PathBuf>) -> Self {
        self.with_manifest_path(path)
    }
}

impl EvalCase {
    pub fn new(
        name: impl Into<String>,
        task: impl Into<String>,
        provider: Arc<dyn LlmProvider>,
    ) -> Self {
        Self {
            name: name.into(),
            task: task.into(),
            provider,
            revision: None,
        }
    }

    /// Explicit provider/fixture revision. Changing it invalidates prior
    /// manifest records without exposing the revision in the manifest.
    pub fn with_revision(mut self, revision: impl Into<String>) -> Self {
        self.revision = Some(revision.into());
        self
    }

    pub fn with_version(self, revision: impl Into<String>) -> Self {
        self.with_revision(revision)
    }
}

/// 单个 case 的结果。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseResult {
    pub name: String,
    pub approved: bool,
    pub steps: usize,
    pub tokens: usize,
    pub duration_ms: u64,
    pub status: CaseStatus,
    pub timed_out: bool,
    pub invariants: Vec<InvariantEvidence>,
    pub evidence: Vec<InvariantEvidence>,
}

/// 整批 eval 的报告。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvalReport {
    pub results: Vec<CaseResult>,
    pub passed: usize,
    pub total: usize,
    pub total_tokens: usize,
    #[serde(default)]
    pub resumed: usize,
    #[serde(default)]
    pub executed: usize,
}

const MANIFEST_VERSION: u32 = 1;
const CASE_FINGERPRINT_DOMAIN: &[u8] = b"ridgecode-eval-case-v2";
const CASE_EXECUTION_FINGERPRINT_DOMAIN: &[u8] = b"ridgecode-eval-execution-v1";

/// Stable SHA-256 case identity. The task itself never enters the manifest;
/// only this deterministic digest does.
pub fn case_fingerprint(name: &str, task: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(CASE_FINGERPRINT_DOMAIN);
    update_text(&mut hasher, name);
    update_text(&mut hasher, task);
    hex_digest(hasher.finalize())
}

/// Fingerprint all inputs that can change a case result or approval gate.
/// Concurrency and manifest location intentionally do not participate.
pub fn case_execution_fingerprint(case: &EvalCase, options: &HarnessOptions) -> String {
    let mut hasher = Sha256::new();
    hasher.update(CASE_EXECUTION_FINGERPRINT_DOMAIN);
    update_text(&mut hasher, &case.name);
    update_text(&mut hasher, &case.task);
    update_text(&mut hasher, case.revision.as_deref().unwrap_or(""));
    hasher.update(options.case_timeout.as_secs().to_le_bytes());
    hasher.update(options.case_timeout.subsec_nanos().to_le_bytes());
    hasher.update((options.max_invariants as u64).to_le_bytes());
    hasher.update((options.invariants.len() as u64).to_le_bytes());
    for invariant in &options.invariants {
        match invariant {
            Invariant::Approved => {
                hasher.update([0]);
            }
            Invariant::MaxSteps(limit) => {
                hasher.update([1]);
                hasher.update((*limit as u64).to_le_bytes());
            }
            Invariant::MaxTokens(limit) => {
                hasher.update([2]);
                hasher.update((*limit as u64).to_le_bytes());
            }
            Invariant::Marker(marker) => {
                hasher.update([3]);
                update_text(&mut hasher, marker);
            }
        }
    }
    hex_digest(hasher.finalize())
}

fn update_text(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn hex_digest(digest: impl AsRef<[u8]>) -> String {
    digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ManifestRecord {
    version: u32,
    name: String,
    fingerprint: String,
    result: CaseResult,
}

struct ManifestSession {
    records: BTreeMap<String, ManifestRecord>,
    writer: Arc<ManifestWriter>,
}

struct ManifestWriter {
    state: Mutex<ManifestWriterState>,
}

struct ManifestWriterState {
    path: PathBuf,
    needs_separator: bool,
}

impl ManifestWriter {
    fn append(&self, record: &ManifestRecord) -> anyhow::Result<()> {
        let line = serde_json::to_vec(record)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("eval manifest writer lock poisoned"))?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&state.path)?;
        if state.needs_separator {
            file.write_all(b"\n")?;
            state.needs_separator = false;
        }
        file.write_all(&line)?;
        file.write_all(b"\n")?;
        file.flush()?;
        file.sync_data()?;
        Ok(())
    }
}

fn open_manifest(path: &Path) -> anyhow::Result<ManifestSession> {
    let path = validate_manifest_path(path)?;
    let (records, needs_separator) = load_manifest(&path)?;
    OpenOptions::new().create(true).append(true).open(&path)?;
    Ok(ManifestSession {
        records,
        writer: Arc::new(ManifestWriter {
            state: Mutex::new(ManifestWriterState {
                path,
                needs_separator,
            }),
        }),
    })
}

fn validate_manifest_path(path: &Path) -> anyhow::Result<PathBuf> {
    let cwd = fs::canonicalize(std::env::current_dir()?)?;
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let resolved = if fs::symlink_metadata(&candidate).is_ok() {
        fs::canonicalize(&candidate).map_err(|error| {
            anyhow::anyhow!(
                "manifest path cannot be resolved: {} ({error})",
                candidate.display()
            )
        })?
    } else {
        let parent = candidate
            .parent()
            .ok_or_else(|| anyhow::anyhow!("manifest path has no parent directory"))?;
        if !parent.exists() {
            anyhow::bail!(
                "manifest parent directory does not exist: {}",
                parent.display()
            );
        }
        fs::canonicalize(parent)?.join(candidate.file_name().ok_or_else(|| {
            anyhow::anyhow!("manifest path must name a file: {}", candidate.display())
        })?)
    };
    if resolved == cwd || !resolved.starts_with(&cwd) {
        anyhow::bail!(
            "manifest path must remain inside cwd: {}",
            resolved.display()
        );
    }
    if resolved.exists() && fs::metadata(&resolved)?.is_dir() {
        anyhow::bail!("manifest path is a directory: {}", resolved.display());
    }
    Ok(resolved)
}

fn load_manifest(path: &Path) -> anyhow::Result<(BTreeMap<String, ManifestRecord>, bool)> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    let needs_separator = !bytes.is_empty() && !bytes.ends_with(b"\n");
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| anyhow::anyhow!("eval manifest is not UTF-8: {error}"))?;
    let lines = text.split('\n').collect::<Vec<_>>();
    let mut records = BTreeMap::new();
    for (index, raw_line) in lines.iter().enumerate() {
        let line = raw_line.trim_end_matches('\r').trim();
        if line.is_empty() {
            continue;
        }
        let final_nonempty_line = lines[index + 1..]
            .iter()
            .all(|candidate| candidate.trim_end_matches('\r').trim().is_empty());
        let record = match serde_json::from_str::<ManifestRecord>(line) {
            Ok(record) => record,
            Err(error) if final_nonempty_line && error.is_eof() => continue,
            Err(error) => anyhow::bail!("invalid eval manifest line {}: {error}", index + 1),
        };
        if record.version != MANIFEST_VERSION {
            anyhow::bail!(
                "unsupported eval manifest version {} on line {}",
                record.version,
                index + 1
            );
        }
        if record.name.is_empty()
            || record.fingerprint.is_empty()
            || record.result.name != record.name
        {
            anyhow::bail!("invalid eval manifest identity on line {}", index + 1);
        }
        if let Some(existing) = records.get(&record.fingerprint) {
            if existing != &record {
                anyhow::bail!(
                    "conflicting eval manifest records for fingerprint {}",
                    record.fingerprint
                );
            }
        } else {
            records.insert(record.fingerprint.clone(), record);
        }
    }
    Ok((records, needs_separator))
}

impl EvalReport {
    pub fn pass_rate(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.passed as f64 / self.total as f64
        }
    }
}

/// 批量跑:使用默认有界选项执行 case(确定性闸判 pass),聚合成功率 + 成本。
///
/// Keep the legacy entry point bounded too; callers that need a different
/// timeout/concurrency/invariant policy should use [`run_eval_with_options`].
pub async fn run_eval(cases: Vec<EvalCase>) -> anyhow::Result<EvalReport> {
    run_eval_with_options(cases, HarnessOptions::default()).await
}

const MAX_MARKER_BYTES: usize = 256;
const MAX_MARKER_MESSAGES: usize = 256;
const MAX_MARKER_MESSAGE_BYTES: usize = 16 * 1024;

/// Run cases with bounded concurrency, per-case timeout, stable input-order
/// aggregation, and deterministic invariant evidence.
pub async fn run_eval_with_options(
    cases: Vec<EvalCase>,
    options: HarnessOptions,
) -> anyhow::Result<EvalReport> {
    let mut names = BTreeSet::new();
    for case in &cases {
        if !names.insert(case.name.clone()) {
            anyhow::bail!("duplicate eval case name is not allowed: {}", case.name);
        }
    }

    let max_concurrency = options.max_concurrency.clamp(1, MAX_HARNESS_CONCURRENCY);
    let total = cases.len();
    let mut ordered: Vec<Option<CaseResult>> = (0..total).map(|_| None).collect();
    let mut fingerprints = vec![String::new(); total];
    let manifest = options
        .manifest_path
        .as_deref()
        .map(open_manifest)
        .transpose()?;
    let writer = manifest.as_ref().map(|session| Arc::clone(&session.writer));
    let mut pending_cases = Vec::with_capacity(total);
    let mut resumed = 0;
    for (index, case) in cases.into_iter().enumerate() {
        let fingerprint = case_execution_fingerprint(&case, &options);
        fingerprints[index] = fingerprint.clone();
        if let Some(record) = manifest
            .as_ref()
            .and_then(|session| session.records.get(&fingerprint))
        {
            if record.name != case.name {
                anyhow::bail!(
                    "eval manifest fingerprint {} names {}, expected {}",
                    fingerprint,
                    record.name,
                    case.name
                );
            }
            ordered[index] = Some(record.result.clone());
            resumed += 1;
        } else {
            pending_cases.push((index, case));
        }
    }
    let executed = pending_cases.len();
    let mut pending = pending_cases.into_iter();
    let mut active = tokio::task::JoinSet::new();

    // Refill on each completion rather than waiting for a whole wave. This
    // keeps the concurrency ceiling while avoiding a slow case idling all
    // available slots behind it; final report order remains input-stable.
    for _ in 0..max_concurrency {
        let Some((index, case)) = pending.next() else {
            break;
        };
        let case_options = options.clone();
        active.spawn(async move { (index, run_case_with_options(case, case_options).await) });
    }

    while let Some(joined) = active.join_next().await {
        let (index, result) =
            joined.map_err(|error| anyhow::anyhow!("eval case task failed: {error}"))?;
        if let Some(writer) = writer.as_ref() {
            writer.append(&ManifestRecord {
                version: MANIFEST_VERSION,
                name: result.name.clone(),
                fingerprint: fingerprints[index].clone(),
                result: result.clone(),
            })?;
        }
        ordered[index] = Some(result);
        if let Some((next_index, case)) = pending.next() {
            let case_options = options.clone();
            active.spawn(
                async move { (next_index, run_case_with_options(case, case_options).await) },
            );
        }
    }

    let results = ordered
        .into_iter()
        .map(|result| result.expect("every spawned eval case has a result"))
        .collect::<Vec<_>>();
    let passed = results.iter().filter(|result| result.approved).count();
    let total_tokens = results
        .iter()
        .fold(0usize, |total, result| total.saturating_add(result.tokens));
    let total = results.len();
    Ok(EvalReport {
        results,
        passed,
        total,
        total_tokens,
        resumed,
        executed,
    })
}

enum CaseExecution {
    Completed(Box<AgentState>),
    Incomplete {
        status: CaseStatus,
        state: Option<Box<AgentState>>,
    },
}

async fn run_case_with_options(case: EvalCase, options: HarnessOptions) -> CaseResult {
    let started = Instant::now();
    let name = case.name;
    let task = case.task;
    let checkpoint = Arc::new(MemoryCheckpointer::new());
    let run_checkpoint = Arc::clone(&checkpoint);
    let run = async move {
        let app = build_llm_agent(case.provider).map_err(|_| ())?;
        app.invoke_with(
            AgentState::new(task),
            &RunConfig::default(),
            Some(run_checkpoint.as_ref()),
            None,
        )
        .await
        .map_err(|_| ())
    };

    let outcome = if options.case_timeout.is_zero() {
        match run.await {
            Ok(state) => CaseExecution::Completed(Box::new(state)),
            Err(()) => CaseExecution::Incomplete {
                status: CaseStatus::Failed,
                state: latest_state(&checkpoint),
            },
        }
    } else {
        match tokio::time::timeout(options.case_timeout, run).await {
            Ok(Ok(state)) => CaseExecution::Completed(Box::new(state)),
            Ok(Err(())) => CaseExecution::Incomplete {
                status: CaseStatus::Failed,
                state: latest_state(&checkpoint),
            },
            Err(_) => CaseExecution::Incomplete {
                status: CaseStatus::TimedOut,
                state: latest_state(&checkpoint),
            },
        }
    };
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    match outcome {
        CaseExecution::Completed(state) => {
            let invariants =
                evaluate_invariants(&state, &options.invariants, options.max_invariants);
            let gate_passed = invariants.iter().all(|evidence| evidence.passed);
            let approved = state.approved && gate_passed;
            CaseResult {
                name,
                approved,
                steps: state.steps,
                tokens: state.total_tokens,
                duration_ms,
                status: CaseStatus::Completed,
                timed_out: false,
                evidence: invariants.clone(),
                invariants,
            }
        }
        CaseExecution::Incomplete { status, state } => {
            let timed_out = status == CaseStatus::TimedOut;
            let invariants = failed_invariants(
                &options.invariants,
                options.max_invariants,
                state.as_deref(),
            );
            CaseResult {
                name,
                approved: false,
                steps: state.as_ref().map_or(0, |state| state.steps),
                tokens: state.as_ref().map_or(0, |state| state.total_tokens),
                duration_ms,
                status,
                timed_out,
                evidence: invariants.clone(),
                invariants,
            }
        }
    }
}

fn latest_state(checkpoint: &MemoryCheckpointer<AgentState>) -> Option<Box<AgentState>> {
    checkpoint
        .latest()
        .map(|checkpoint| Box::new(checkpoint.state))
}

fn evaluate_invariants(
    state: &AgentState,
    invariants: &[Invariant],
    max_invariants: usize,
) -> Vec<InvariantEvidence> {
    invariants
        .iter()
        .take(max_invariants)
        .map(|invariant| match invariant {
            Invariant::Approved => InvariantEvidence {
                kind: InvariantKind::Approved,
                passed: state.approved,
                observed: usize::from(state.approved),
                limit: Some(1),
            },
            Invariant::MaxSteps(limit) => InvariantEvidence {
                kind: InvariantKind::Steps,
                passed: state.steps <= *limit,
                observed: state.steps,
                limit: Some(*limit),
            },
            Invariant::MaxTokens(limit) => InvariantEvidence {
                kind: InvariantKind::Tokens,
                passed: state.total_tokens <= *limit,
                observed: state.total_tokens,
                limit: Some(*limit),
            },
            Invariant::Marker(marker) => {
                let present = marker_present(state, marker);
                InvariantEvidence {
                    kind: InvariantKind::Marker,
                    passed: present,
                    observed: usize::from(present),
                    limit: Some(1),
                }
            }
        })
        .collect()
}

fn failed_invariants(
    invariants: &[Invariant],
    max_invariants: usize,
    state: Option<&AgentState>,
) -> Vec<InvariantEvidence> {
    invariants
        .iter()
        .take(max_invariants)
        .map(|invariant| match invariant {
            Invariant::Approved => InvariantEvidence {
                kind: InvariantKind::Approved,
                passed: false,
                observed: state.map_or(0, |state| usize::from(state.approved)),
                limit: Some(1),
            },
            Invariant::MaxSteps(limit) => InvariantEvidence {
                kind: InvariantKind::Steps,
                passed: false,
                observed: state.map_or(0, |state| state.steps),
                limit: Some(*limit),
            },
            Invariant::MaxTokens(limit) => InvariantEvidence {
                kind: InvariantKind::Tokens,
                passed: false,
                observed: state.map_or(0, |state| state.total_tokens),
                limit: Some(*limit),
            },
            Invariant::Marker(marker) => InvariantEvidence {
                kind: InvariantKind::Marker,
                passed: false,
                observed: state.map_or(0, |state| usize::from(marker_present(state, marker))),
                limit: Some(1),
            },
        })
        .collect()
}

fn marker_present(state: &AgentState, marker: &str) -> bool {
    let marker = marker.as_bytes();
    if marker.is_empty() || marker.len() > MAX_MARKER_BYTES {
        return false;
    }
    state
        .messages
        .iter()
        .chain(state.display_messages.iter())
        .take(MAX_MARKER_MESSAGES)
        .any(|message| {
            message
                .as_bytes()
                .get(..message.len().min(MAX_MARKER_MESSAGE_BYTES))
                .is_some_and(|prefix| prefix.windows(marker.len()).any(|window| window == marker))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use provider::{
        Completion, CompletionRequest, LlmProvider, Message, ScriptedProvider, ToolCall, Usage,
    };
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Default)]
    struct RefillProbe {
        slow_active: AtomicBool,
        overlap_observed: AtomicBool,
    }

    struct RefillProvider {
        case: usize,
        calls: AtomicUsize,
        probe: Arc<RefillProbe>,
        scripted: ScriptedProvider,
    }

    struct PartialTimeoutProvider {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl LlmProvider for PartialTimeoutProvider {
        async fn complete(
            &self,
            _request: &CompletionRequest,
        ) -> Result<Completion, provider::ProviderError> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Ok(Completion {
                    tool_calls: vec![tool_call("exit 1")],
                    usage: Usage {
                        prompt_tokens: 11,
                        completion_tokens: 2,
                    },
                    ..Default::default()
                });
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
            Ok(Completion::default())
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for RefillProvider {
        async fn complete(
            &self,
            request: &CompletionRequest,
        ) -> Result<Completion, provider::ProviderError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if self.case == 0 && call == 0 {
                self.probe.slow_active.store(true, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(120)).await;
                self.probe.slow_active.store(false, Ordering::SeqCst);
            } else {
                if self.case == 2 && call == 0 && self.probe.slow_active.load(Ordering::SeqCst) {
                    self.probe.overlap_observed.store(true, Ordering::SeqCst);
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            self.scripted.complete(request).await
        }
    }

    fn tool_call(cmd: &str) -> ToolCall {
        ToolCall {
            id: "1".to_string(),
            name: "run_shell".to_string(),
            arguments: json!({ "cmd": cmd }),
        }
    }

    fn pass_provider_for_manifest() -> Arc<ScriptedProvider> {
        Arc::new(ScriptedProvider::new(vec![
            Completion {
                tool_calls: vec![tool_call("exit 0")],
                usage: Usage {
                    prompt_tokens: 7,
                    completion_tokens: 2,
                },
                ..Default::default()
            },
            Completion {
                text: "done".to_string(),
                ..Default::default()
            },
        ]))
    }

    fn manifest_test_path() -> std::path::PathBuf {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
        let quality_dir = std::path::Path::new("target").join("quality");
        std::fs::create_dir_all(&quality_dir).expect("quality test directory");
        quality_dir.join(format!(
            "eval-manifest-test-{}-{}.jsonl",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn remove_manifest(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn eval_reports_pass_rate_and_cost() {
        // pass:exit 0(确定性通过)→ 收尾。
        let pass = Arc::new(ScriptedProvider::new(vec![
            Completion {
                tool_calls: vec![tool_call("exit 0")],
                usage: Usage {
                    prompt_tokens: 10,
                    completion_tokens: 0,
                },
                ..Default::default()
            },
            Completion {
                text: "done".to_string(),
                ..Default::default()
            },
        ]));
        // fail:一直 exit 1(无进展熔断)→ 不通过。
        let fail = Arc::new(ScriptedProvider::new(
            (0..8)
                .map(|_| Completion {
                    tool_calls: vec![tool_call("exit 1")],
                    ..Default::default()
                })
                .collect(),
        ));

        let report = run_eval(vec![
            EvalCase::new("pass", "make build pass", pass),
            EvalCase::new("fail", "impossible", fail),
        ])
        .await
        .unwrap();

        assert_eq!(report.total, 2);
        assert_eq!(report.passed, 1);
        assert!((report.pass_rate() - 0.5).abs() < 1e-9);
        assert!(report.total_tokens >= 10);
    }

    #[tokio::test]
    async fn bounded_harness_keeps_input_order_and_checks_invariants() {
        let report = run_eval_with_options(
            vec![
                EvalCase::new(
                    "second",
                    "second task",
                    Arc::new(ScriptedProvider::new(vec![
                        Completion {
                            tool_calls: vec![tool_call("exit 0")],
                            ..Default::default()
                        },
                        Completion {
                            text: "marker-second".to_string(),
                            ..Default::default()
                        },
                    ])),
                ),
                EvalCase::new(
                    "first",
                    "first task",
                    Arc::new(ScriptedProvider::new(vec![
                        Completion {
                            tool_calls: vec![tool_call("exit 0")],
                            ..Default::default()
                        },
                        Completion {
                            text: "marker-first".to_string(),
                            ..Default::default()
                        },
                    ])),
                ),
            ],
            HarnessOptions::default()
                .with_max_concurrency(2)
                .with_invariants([
                    Invariant::Approved,
                    Invariant::MaxSteps(4),
                    Invariant::MaxTokens(100),
                    Invariant::Marker("marker".to_string()),
                ]),
        )
        .await
        .unwrap();

        assert_eq!(report.results[0].name, "second");
        assert_eq!(report.results[1].name, "first");
        assert_eq!(report.passed, 2);
        assert!(report.results.iter().all(|result| {
            result.status == CaseStatus::Completed
                && !result.timed_out
                && result.invariants.len() == 4
                && result.invariants.iter().all(|evidence| evidence.passed)
        }));
    }

    #[tokio::test]
    async fn bounded_harness_refills_slots_on_completion() {
        let probe = Arc::new(RefillProbe::default());
        let cases = (0..3)
            .map(|case| {
                let scripted = ScriptedProvider::new(vec![Completion {
                    text: format!("case-{case}-done"),
                    ..Default::default()
                }]);
                EvalCase::new(
                    format!("case-{case}"),
                    "refill",
                    Arc::new(RefillProvider {
                        case,
                        calls: AtomicUsize::new(0),
                        probe: probe.clone(),
                        scripted,
                    }),
                )
            })
            .collect();

        let report = run_eval_with_options(
            cases,
            HarnessOptions::default()
                .with_max_concurrency(2)
                .with_case_timeout(Duration::from_secs(2)),
        )
        .await
        .unwrap();

        assert_eq!(report.total, 3);
        assert!(probe.overlap_observed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn bounded_harness_marks_slow_case_without_leaking_error_text() {
        let provider = Arc::new(
            ScriptedProvider::new(vec![Completion::default()])
                .with_delay(std::time::Duration::from_millis(100)),
        );
        let report = run_eval_with_options(
            vec![EvalCase::new("slow", "bounded", provider)],
            HarnessOptions::default().with_case_timeout(std::time::Duration::from_millis(5)),
        )
        .await
        .unwrap();

        assert_eq!(report.total, 1);
        assert_eq!(report.passed, 0);
        assert_eq!(report.results[0].status, CaseStatus::TimedOut);
        assert!(report.results[0].timed_out);
        assert!(report.results[0].duration_ms >= 5);
    }

    #[tokio::test]
    async fn timeout_retains_observed_checkpoint_cost() {
        let report = run_eval_with_options(
            vec![EvalCase::new(
                "partial-timeout",
                "retain completed work",
                Arc::new(PartialTimeoutProvider {
                    calls: AtomicUsize::new(0),
                }),
            )],
            HarnessOptions::default()
                .with_case_timeout(Duration::from_millis(100))
                .with_invariants([Invariant::MaxSteps(10), Invariant::MaxTokens(100)]),
        )
        .await
        .unwrap();

        let result = &report.results[0];
        assert_eq!(result.status, CaseStatus::TimedOut);
        assert!(result.steps > 0);
        assert_eq!(result.tokens, 13);
        assert_eq!(report.total_tokens, 13);
        assert_eq!(result.invariants[0].observed, result.steps);
        assert_eq!(result.invariants[1].observed, result.tokens);
        assert!(result.invariants.iter().all(|evidence| !evidence.passed));
    }

    #[tokio::test]
    async fn scripted_provider_records_only_bounded_request_shape() {
        let provider = ScriptedProvider::new(vec![Completion::default()]);
        provider
            .complete(&CompletionRequest {
                messages: vec![Message::user("secret-cookie-and-api-key")],
                tools: Vec::new(),
            })
            .await
            .unwrap();

        let records = provider.recorded_requests();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].message_count, 1);
        assert!(!format!("{records:?}").contains("secret-cookie-and-api-key"));
    }

    #[tokio::test]
    async fn manifest_resume_keeps_totals_and_input_order_without_duplicates() {
        let path = manifest_test_path();
        let first = run_eval_with_options(
            vec![
                EvalCase::new("first", "first task", pass_provider_for_manifest()),
                EvalCase::new("second", "second task", pass_provider_for_manifest()),
            ],
            HarnessOptions::default()
                .with_manifest_path(&path)
                .require_approved(),
        )
        .await
        .unwrap();
        assert_eq!(first.executed, 2);
        assert_eq!(first.resumed, 0);

        let second = run_eval_with_options(
            vec![
                EvalCase::new(
                    "first",
                    "first task",
                    Arc::new(ScriptedProvider::new(vec![])),
                ),
                EvalCase::new(
                    "second",
                    "second task",
                    Arc::new(ScriptedProvider::new(vec![])),
                ),
            ],
            HarnessOptions::default()
                .with_manifest_path(&path)
                .require_approved(),
        )
        .await
        .unwrap();
        assert_eq!(second.resumed, 2);
        assert_eq!(second.executed, 0);
        assert_eq!(second.total, 2);
        assert_eq!(second.passed, first.passed);
        assert_eq!(second.total_tokens, first.total_tokens);
        assert_eq!(
            second
                .results
                .iter()
                .map(|result| result.name.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        let manifest_text = std::fs::read_to_string(&path).unwrap();
        let lines = manifest_text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines
                .iter()
                .filter_map(|line| serde_json::from_str::<ManifestRecord>(line).ok())
                .map(|record| record.fingerprint)
                .collect::<BTreeSet<_>>()
                .len(),
            2
        );
        remove_manifest(&path);
    }

    #[tokio::test]
    async fn changed_invariant_policy_invalidates_manifest_record() {
        let path = manifest_test_path();
        let base_options = HarnessOptions::default()
            .with_manifest_path(&path)
            .require_approved();
        let first = run_eval_with_options(
            vec![EvalCase::new(
                "policy",
                "make the build pass",
                pass_provider_for_manifest(),
            )],
            base_options.clone(),
        )
        .await
        .unwrap();
        assert_eq!(first.resumed, 0);
        assert!(first.results[0].approved);

        let strict_options = base_options.clone().require_max_tokens(0);
        let strict = run_eval_with_options(
            vec![EvalCase::new(
                "policy",
                "make the build pass",
                pass_provider_for_manifest(),
            )],
            strict_options.clone(),
        )
        .await
        .unwrap();
        assert_eq!(strict.resumed, 0);
        assert_eq!(strict.executed, 1);
        assert!(!strict.results[0].approved);

        let reused = run_eval_with_options(
            vec![EvalCase::new(
                "policy",
                "make the build pass",
                Arc::new(ScriptedProvider::new(vec![])),
            )],
            strict_options,
        )
        .await
        .unwrap();
        assert_eq!(reused.resumed, 1);
        assert_eq!(reused.executed, 0);
        assert!(!reused.results[0].approved);
        remove_manifest(&path);
    }

    #[tokio::test]
    async fn changed_case_revision_invalidates_manifest_record() {
        let path = manifest_test_path();
        let options = HarnessOptions::default()
            .with_manifest_path(&path)
            .require_approved();
        run_eval_with_options(
            vec![EvalCase::new(
                "revision",
                "make the build pass",
                pass_provider_for_manifest(),
            )
            .with_revision("provider-v1")],
            options.clone(),
        )
        .await
        .unwrap();
        let changed = run_eval_with_options(
            vec![EvalCase::new(
                "revision",
                "make the build pass",
                Arc::new(ScriptedProvider::new(vec![])),
            )
            .with_revision("provider-v2")],
            options,
        )
        .await
        .unwrap();
        assert_eq!(changed.resumed, 0);
        assert_eq!(changed.executed, 1);
        remove_manifest(&path);
    }

    #[tokio::test]
    async fn manifest_ignores_only_a_torn_final_tail() {
        let path = manifest_test_path();
        run_eval_with_options(
            vec![EvalCase::new(
                "kept",
                "kept task",
                pass_provider_for_manifest(),
            )],
            HarnessOptions::default().with_manifest_path(&path),
        )
        .await
        .unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend_from_slice(b"{\"version\":1,\"name\":\"torn\"");
        std::fs::write(&path, bytes).unwrap();

        let report = run_eval_with_options(
            vec![EvalCase::new(
                "kept",
                "kept task",
                Arc::new(ScriptedProvider::new(vec![])),
            )],
            HarnessOptions::default().with_manifest_path(&path),
        )
        .await
        .unwrap();
        assert_eq!(report.resumed, 1);
        assert_eq!(report.executed, 0);
        remove_manifest(&path);
    }

    #[tokio::test]
    async fn manifest_middle_corruption_fails_closed() {
        let path = manifest_test_path();
        run_eval_with_options(
            vec![
                EvalCase::new("one", "one task", pass_provider_for_manifest()),
                EvalCase::new("two", "two task", pass_provider_for_manifest()),
            ],
            HarnessOptions::default().with_manifest_path(&path),
        )
        .await
        .unwrap();
        let records = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        std::fs::write(&path, format!("{}\nnot-json\n{}\n", records[0], records[1])).unwrap();
        let error = run_eval_with_options(
            vec![
                EvalCase::new("one", "one task", Arc::new(ScriptedProvider::new(vec![]))),
                EvalCase::new("two", "two task", Arc::new(ScriptedProvider::new(vec![]))),
            ],
            HarnessOptions::default().with_manifest_path(&path),
        )
        .await
        .expect_err("middle corruption must stop resume");
        assert!(error.to_string().contains("manifest line 2"));
        remove_manifest(&path);
    }

    #[tokio::test]
    async fn manifest_conflicting_duplicate_fingerprint_fails_closed() {
        let path = manifest_test_path();
        run_eval_with_options(
            vec![EvalCase::new(
                "conflict",
                "conflict task",
                pass_provider_for_manifest(),
            )],
            HarnessOptions::default().with_manifest_path(&path),
        )
        .await
        .unwrap();
        let original = std::fs::read_to_string(&path).unwrap();
        let mut modified: serde_json::Value = serde_json::from_str(original.trim()).unwrap();
        let duration = modified["result"]["duration_ms"].as_u64().unwrap();
        modified["result"]["duration_ms"] = json!(duration.saturating_add(1));
        let mut content = original;
        content.push_str(&serde_json::to_string(&modified).unwrap());
        content.push('\n');
        std::fs::write(&path, content).unwrap();
        let error = run_eval_with_options(
            vec![EvalCase::new(
                "conflict",
                "conflict task",
                Arc::new(ScriptedProvider::new(vec![])),
            )],
            HarnessOptions::default().with_manifest_path(&path),
        )
        .await
        .expect_err("conflicting duplicate must stop resume");
        assert!(error
            .to_string()
            .contains("conflicting eval manifest records"));
        remove_manifest(&path);
    }

    #[tokio::test]
    async fn stale_fingerprint_is_not_reused() {
        let path = manifest_test_path();
        run_eval_with_options(
            vec![EvalCase::new(
                "same-name",
                "old task",
                pass_provider_for_manifest(),
            )],
            HarnessOptions::default().with_manifest_path(&path),
        )
        .await
        .unwrap();
        let provider = Arc::new(ScriptedProvider::new(vec![Completion {
            text: "executed-new-task".to_string(),
            ..Default::default()
        }]));
        let report = run_eval_with_options(
            vec![EvalCase::new("same-name", "new task", provider.clone())],
            HarnessOptions::default().with_manifest_path(&path),
        )
        .await
        .unwrap();
        assert_eq!(report.resumed, 0);
        assert_eq!(report.executed, 1);
        assert!(provider.request_count() > 0);
        remove_manifest(&path);
    }

    #[tokio::test]
    async fn duplicate_name_is_rejected_before_execution() {
        let provider = Arc::new(ScriptedProvider::new(vec![Completion::default()]));
        let error = run_eval_with_options(
            vec![
                EvalCase::new("duplicate", "one", provider.clone()),
                EvalCase::new("duplicate", "two", provider),
            ],
            HarnessOptions::default(),
        )
        .await
        .expect_err("duplicate names must be explicit");
        assert!(error.to_string().contains("duplicate eval case name"));
    }

    #[tokio::test]
    async fn manifest_never_contains_task_body_or_marker_text() {
        let path = manifest_test_path();
        let secret = "sensitive task body api-key=do-not-persist";
        run_eval_with_options(
            vec![EvalCase::new(
                "secret-case",
                secret,
                pass_provider_for_manifest(),
            )],
            HarnessOptions::default()
                .with_manifest_path(&path)
                .require_marker("secret-marker-not-persisted"),
        )
        .await
        .unwrap();
        let manifest = std::fs::read_to_string(&path).unwrap();
        assert!(!manifest.contains(secret));
        assert!(!manifest.contains("secret-marker-not-persisted"));
        assert!(manifest.contains("secret-case"));
        let record: ManifestRecord = serde_json::from_str(manifest.trim()).unwrap();
        assert_eq!(record.fingerprint.len(), 64);
        remove_manifest(&path);
    }

    #[test]
    fn harness_concurrency_is_hard_bounded() {
        assert_eq!(
            HarnessOptions::default()
                .with_max_concurrency(usize::MAX)
                .max_concurrency,
            MAX_HARNESS_CONCURRENCY
        );
    }
}
