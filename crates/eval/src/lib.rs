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
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use agent::{build_llm_agent, AgentState};
use langgraph::{MemoryCheckpointer, RunConfig};
use provider::LlmProvider;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

/// Schema version for machine-readable external evaluation artifacts.
///
/// This is intentionally separate from the append-only harness manifest
/// version: callers can evolve resumable local execution without changing the
/// result contract consumed by benchmark runners.
pub const MACHINE_EVAL_SCHEMA_VERSION: u32 = 1;

/// Serializable description of one externally evaluated task.
///
/// `EvalCase` deliberately owns a live provider and is therefore not suitable
/// for exchange with an external runner. `CaseSpecV1` is the provider-free
/// counterpart used in machine-readable inputs and results.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseSpecV1 {
    pub schema_version: u32,
    pub case_id: String,
    pub task: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

impl CaseSpecV1 {
    pub fn new(case_id: impl Into<String>, task: impl Into<String>) -> Self {
        Self {
            schema_version: MACHINE_EVAL_SCHEMA_VERSION,
            case_id: case_id.into(),
            task: task.into(),
            tags: Vec::new(),
        }
    }

    pub fn with_tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tags = tags.into_iter().map(Into::into).collect();
        self
    }
}

/// Outcome reported by a verifier that is independent from the agent.
///
/// `NotRun` is deliberate rather than an optional value: an agent's internal
/// approval is never evidence of external success.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalVerificationStatus {
    #[default]
    NotRun,
    Passed,
    Failed,
    Error,
}

/// Bounded, serializable result emitted by an external verifier.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalVerification {
    pub schema_version: u32,
    pub status: ExternalVerificationStatus,
    /// Stable identifier for the verifier, for example `hidden-tests-v1`.
    pub verifier: String,
    /// Revision or workspace fingerprint the verifier observed, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// Human- and machine-readable bounded summary. Implementations must not
    /// place secrets or unbounded command output here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub duration_ms: u64,
}

impl ExternalVerification {
    pub fn not_run(verifier: impl Into<String>) -> Self {
        Self::new(ExternalVerificationStatus::NotRun, verifier)
    }

    pub fn passed(verifier: impl Into<String>) -> Self {
        Self::new(ExternalVerificationStatus::Passed, verifier)
    }

    pub fn failed(verifier: impl Into<String>) -> Self {
        Self::new(ExternalVerificationStatus::Failed, verifier)
    }

    pub fn error(verifier: impl Into<String>) -> Self {
        Self::new(ExternalVerificationStatus::Error, verifier)
    }

    pub fn is_success(&self) -> bool {
        self.status == ExternalVerificationStatus::Passed
    }

    fn new(status: ExternalVerificationStatus, verifier: impl Into<String>) -> Self {
        Self {
            schema_version: MACHINE_EVAL_SCHEMA_VERSION,
            status,
            verifier: verifier.into(),
            revision: None,
            summary: None,
            duration_ms: 0,
        }
    }
}

/// Versioned machine result that keeps the agent's self-assessment separate
/// from independently verified success.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseResultV1 {
    pub schema_version: u32,
    pub case: CaseSpecV1,
    /// The existing agent-internal deterministic gate. This is diagnostic only
    /// and never implies benchmark success.
    pub agent_approved: bool,
    pub external_verification: ExternalVerification,
    pub steps: usize,
    pub tokens: usize,
    pub duration_ms: u64,
    pub status: CaseStatus,
    #[serde(default)]
    pub timed_out: bool,
}

impl CaseResultV1 {
    pub fn from_case_result(
        case: CaseSpecV1,
        result: &CaseResult,
        external_verification: ExternalVerification,
    ) -> Self {
        Self {
            schema_version: MACHINE_EVAL_SCHEMA_VERSION,
            case,
            agent_approved: result.approved,
            external_verification,
            steps: result.steps,
            tokens: result.tokens,
            duration_ms: result.duration_ms,
            status: result.status.clone(),
            timed_out: result.timed_out,
        }
    }

    /// The only success predicate for external benchmarks.
    pub fn externally_verified_success(&self) -> bool {
        self.external_verification.is_success()
    }
}

/// Serializable aggregate for one externally scored experiment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentManifestV1 {
    pub schema_version: u32,
    pub experiment_id: String,
    pub results: Vec<CaseResultV1>,
}

impl ExperimentManifestV1 {
    pub fn new(experiment_id: impl Into<String>, results: Vec<CaseResultV1>) -> Self {
        Self {
            schema_version: MACHINE_EVAL_SCHEMA_VERSION,
            experiment_id: experiment_id.into(),
            results,
        }
    }

    pub fn externally_verified_passed(&self) -> usize {
        self.results
            .iter()
            .filter(|result| result.externally_verified_success())
            .count()
    }
}

/// A verifier command for one external benchmark case.  It is always started
/// as an argv vector, never through a shell.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalVerifierV1 {
    pub name: String,
    pub program: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
}

/// One task, its isolated worktree, and the independent verifier that scores
/// it.  `workspace` must resolve inside the corpus root passed to the runner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalEvalCaseV1 {
    pub case: CaseSpecV1,
    pub workspace: PathBuf,
    pub verifier: ExternalVerifierV1,
}

/// Versioned JSON input accepted by `ridgecode-eval external`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalEvalSuiteV1 {
    pub schema_version: u32,
    pub experiment_id: String,
    pub cases: Vec<ExternalEvalCaseV1>,
}

impl ExternalEvalSuiteV1 {
    pub fn new(experiment_id: impl Into<String>, cases: Vec<ExternalEvalCaseV1>) -> Self {
        Self {
            schema_version: MACHINE_EVAL_SCHEMA_VERSION,
            experiment_id: experiment_id.into(),
            cases,
        }
    }
}

/// Runtime controls for an external benchmark invocation.
#[derive(Clone, Debug)]
pub struct ExternalEvalOptions {
    pub corpus_root: PathBuf,
    pub ridgecode_path: PathBuf,
    pub experiment_id: String,
    pub max_turns: usize,
    pub timeout: Duration,
    pub budget_tokens: Option<usize>,
    pub read_only: bool,
    pub require_api_key: bool,
}

impl ExternalEvalOptions {
    pub fn new(
        corpus_root: impl Into<PathBuf>,
        ridgecode_path: impl Into<PathBuf>,
        experiment_id: impl Into<String>,
    ) -> Self {
        Self {
            corpus_root: corpus_root.into(),
            ridgecode_path: ridgecode_path.into(),
            experiment_id: experiment_id.into(),
            max_turns: 80,
            timeout: Duration::from_secs(20 * 60),
            budget_tokens: None,
            read_only: false,
            require_api_key: true,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_max_turns(mut self, max_turns: usize) -> Self {
        self.max_turns = max_turns.max(1);
        self
    }

    pub fn with_budget_tokens(mut self, budget_tokens: Option<usize>) -> Self {
        self.budget_tokens = budget_tokens;
        self
    }

    pub fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    pub fn require_api_key(mut self, require_api_key: bool) -> Self {
        self.require_api_key = require_api_key;
        self
    }
}

/// Minimal fields consumed from a SWE-bench dataset row. Extra upstream
/// dataset fields are intentionally ignored so a downloaded JSONL can be used
/// directly without coupling the Rust harness to a Python dataset release.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SweBenchInstanceV1 {
    pub instance_id: String,
    pub problem_statement: String,
}

/// Official SWE-bench prediction record. This type deliberately contains no
/// RidgeCode approval or verifier fields: only the official SWE harness may
/// turn a patch into a resolved/unresolved score.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SweBenchPredictionV1 {
    pub instance_id: String,
    pub model_name_or_path: String,
    pub model_patch: String,
}

impl SweBenchPredictionV1 {
    pub fn new(
        instance_id: impl Into<String>,
        model_name_or_path: impl Into<String>,
        model_patch: impl Into<String>,
    ) -> anyhow::Result<Self> {
        let instance_id = instance_id.into();
        if !is_safe_swebench_instance_id(&instance_id) {
            anyhow::bail!("SWE-bench instance id must be a single safe path component");
        }
        let model_name_or_path = model_name_or_path.into();
        if model_name_or_path.trim().is_empty() {
            anyhow::bail!("SWE-bench model name must not be empty");
        }
        Ok(Self {
            instance_id,
            model_name_or_path,
            model_patch: model_patch.into(),
        })
    }
}

/// Controls for producing SWE-bench prediction JSONL from pre-provisioned
/// worktrees. Scoring remains outside RidgeCode in the official harness.
#[derive(Clone, Debug)]
pub struct SweBenchExportOptions {
    pub workspaces_root: PathBuf,
    pub ridgecode_path: PathBuf,
    pub model_name_or_path: String,
    pub max_turns: usize,
    pub timeout: Duration,
    pub budget_tokens: Option<usize>,
    pub require_api_key: bool,
}

impl SweBenchExportOptions {
    pub fn new(
        workspaces_root: impl Into<PathBuf>,
        ridgecode_path: impl Into<PathBuf>,
        model_name_or_path: impl Into<String>,
    ) -> Self {
        Self {
            workspaces_root: workspaces_root.into(),
            ridgecode_path: ridgecode_path.into(),
            model_name_or_path: model_name_or_path.into(),
            max_turns: 80,
            timeout: Duration::from_secs(20 * 60),
            budget_tokens: None,
            require_api_key: true,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_max_turns(mut self, max_turns: usize) -> Self {
        self.max_turns = max_turns.max(1);
        self
    }

    pub fn with_budget_tokens(mut self, budget_tokens: Option<usize>) -> Self {
        self.budget_tokens = budget_tokens;
        self
    }

    pub fn require_api_key(mut self, require_api_key: bool) -> Self {
        self.require_api_key = require_api_key;
        self
    }
}

/// Score derived exclusively from official SWE-bench `report.json` files.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SweBenchScoreV1 {
    pub schema_version: u32,
    pub total: usize,
    pub resolved: usize,
    pub unresolved: usize,
    /// Fraction in `[0.0, 1.0]`, derived from official `resolved` booleans.
    pub resolution_rate: f64,
}

/// Difference between two official scorecards. Positive values favour the
/// candidate; this structure contains no model-generated assessment.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SweBenchComparisonV1 {
    pub schema_version: u32,
    pub baseline: SweBenchScoreV1,
    pub candidate: SweBenchScoreV1,
    pub resolved_delta: isize,
    pub resolution_rate_delta: f64,
}

/// Read official harness reports below one run/model root and construct a
/// fail-closed scorecard. The expected shape is `{instance_id: {resolved:
/// bool}}`, matching SWE-bench's per-instance `report.json` artifact.
pub fn score_swebench_reports(reports_root: &Path) -> anyhow::Result<SweBenchScoreV1> {
    let reports_root = reports_root
        .canonicalize()
        .map_err(|error| anyhow::anyhow!("cannot resolve SWE-bench reports root: {error}"))?;
    if !reports_root.is_dir() {
        anyhow::bail!("SWE-bench reports root is not a directory");
    }
    let mut resolved = BTreeMap::new();
    collect_swebench_reports(&reports_root, &mut resolved)?;
    if resolved.is_empty() {
        anyhow::bail!("no official SWE-bench report.json files found");
    }
    let total = resolved.len();
    let resolved_count = resolved.values().filter(|resolved| **resolved).count();
    Ok(SweBenchScoreV1 {
        schema_version: MACHINE_EVAL_SCHEMA_VERSION,
        total,
        resolved: resolved_count,
        unresolved: total - resolved_count,
        resolution_rate: resolved_count as f64 / total as f64,
    })
}

pub fn compare_swebench_scores(
    baseline: SweBenchScoreV1,
    candidate: SweBenchScoreV1,
) -> anyhow::Result<SweBenchComparisonV1> {
    if baseline.schema_version != MACHINE_EVAL_SCHEMA_VERSION
        || candidate.schema_version != MACHINE_EVAL_SCHEMA_VERSION
    {
        anyhow::bail!("unsupported SWE-bench score schema version");
    }
    if baseline.total == 0 || candidate.total == 0 {
        anyhow::bail!("cannot compare empty SWE-bench scorecards");
    }
    Ok(SweBenchComparisonV1 {
        schema_version: MACHINE_EVAL_SCHEMA_VERSION,
        resolved_delta: candidate.resolved as isize - baseline.resolved as isize,
        resolution_rate_delta: candidate.resolution_rate - baseline.resolution_rate,
        baseline,
        candidate,
    })
}

const MAX_SWEBENCH_REPORTS: usize = 10_000;

fn collect_swebench_reports(
    directory: &Path,
    resolved: &mut BTreeMap<String, bool>,
) -> anyhow::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            collect_swebench_reports(&path, resolved)?;
            continue;
        }
        if !file_type.is_file() || entry.file_name() != "report.json" {
            continue;
        }
        let source = fs::read_to_string(&path)
            .map_err(|error| anyhow::anyhow!("cannot read official SWE-bench report: {error}"))?;
        let value: serde_json::Value = serde_json::from_str(&source)
            .map_err(|error| anyhow::anyhow!("invalid official SWE-bench report JSON: {error}"))?;
        let object = value
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("official SWE-bench report must be an object"))?;
        if object.len() != 1 {
            anyhow::bail!("official SWE-bench report must contain exactly one instance result");
        }
        let (instance_id, result) = object.iter().next().expect("checked one report entry");
        if !is_safe_swebench_instance_id(instance_id) {
            anyhow::bail!("official SWE-bench report contains unsafe instance id");
        }
        let resolved_value = result
            .get("resolved")
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| anyhow::anyhow!("official SWE-bench report lacks boolean resolved"))?;
        if resolved
            .insert(instance_id.clone(), resolved_value)
            .is_some()
        {
            anyhow::bail!("duplicate official SWE-bench instance result");
        }
        if resolved.len() > MAX_SWEBENCH_REPORTS {
            anyhow::bail!("too many official SWE-bench reports");
        }
    }
    Ok(())
}

fn is_safe_swebench_instance_id(instance_id: &str) -> bool {
    !instance_id.is_empty()
        && instance_id.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
        && !instance_id.starts_with('.')
        && instance_id.contains("__")
}

const MAX_EXTERNAL_OUTPUT_BYTES: usize = 64 * 1024;

/// Run isolated RidgeCode tasks and score them only with their independent
/// verifier.  A machine-run `approved` bit is retained as diagnostic evidence
/// but never determines the externally verified pass count.
pub async fn run_external_eval(
    cases: Vec<ExternalEvalCaseV1>,
    options: ExternalEvalOptions,
) -> anyhow::Result<ExperimentManifestV1> {
    if cases.is_empty() {
        anyhow::bail!("external eval requires at least one case");
    }
    if options.experiment_id.trim().is_empty() {
        anyhow::bail!("external eval experiment id must not be empty");
    }
    let corpus_root = options
        .corpus_root
        .canonicalize()
        .map_err(|error| anyhow::anyhow!("cannot resolve corpus root: {error}"))?;
    let ridgecode_path = options
        .ridgecode_path
        .canonicalize()
        .map_err(|error| anyhow::anyhow!("cannot resolve ridgecode executable: {error}"))?;
    if !ridgecode_path.is_file() {
        anyhow::bail!("ridgecode executable is not a file");
    }

    let mut results = Vec::with_capacity(cases.len());
    for case in cases {
        let workspace = contained_path(
            &corpus_root,
            &root_relative_path(&corpus_root, &case.workspace),
            "case workspace",
        )?;
        let verifier_program = verifier_program(&corpus_root, &case.verifier.program)?;
        results.push(
            run_external_case(case, workspace, verifier_program, &ridgecode_path, &options).await,
        );
    }
    Ok(ExperimentManifestV1::new(options.experiment_id, results))
}

/// Run pre-provisioned SWE-bench worktrees and export only the prediction
/// contract consumed by the official SWE-bench evaluator. This function does
/// not evaluate tests and must never be interpreted as a resolved-rate score.
pub async fn run_swebench_export(
    instances: Vec<SweBenchInstanceV1>,
    options: SweBenchExportOptions,
) -> anyhow::Result<Vec<SweBenchPredictionV1>> {
    if instances.is_empty() {
        anyhow::bail!("SWE-bench export requires at least one instance");
    }
    if options.model_name_or_path.trim().is_empty() {
        anyhow::bail!("SWE-bench model name must not be empty");
    }
    let workspaces_root = options
        .workspaces_root
        .canonicalize()
        .map_err(|error| anyhow::anyhow!("cannot resolve SWE-bench workspaces root: {error}"))?;
    let ridgecode_path = options
        .ridgecode_path
        .canonicalize()
        .map_err(|error| anyhow::anyhow!("cannot resolve ridgecode executable: {error}"))?;
    if !ridgecode_path.is_file() {
        anyhow::bail!("ridgecode executable is not a file");
    }

    let mut predictions = Vec::with_capacity(instances.len());
    for instance in instances {
        if !is_safe_swebench_instance_id(&instance.instance_id) {
            anyhow::bail!("SWE-bench instance id must be a single safe path component");
        }
        if instance.problem_statement.trim().is_empty() {
            anyhow::bail!("SWE-bench problem statement must not be empty");
        }
        let workspace = contained_path(
            &workspaces_root,
            &workspaces_root.join(&instance.instance_id),
            "SWE-bench workspace",
        )?;
        let task = CaseSpecV1::new(&instance.instance_id, &instance.problem_statement);
        let completed = run_swebench_agent(&task, &workspace, &ridgecode_path, &options).await;
        let model_patch = if completed {
            capture_git_diff(&workspace, options.timeout).await?
        } else {
            String::new()
        };
        predictions.push(SweBenchPredictionV1::new(
            instance.instance_id,
            &options.model_name_or_path,
            model_patch,
        )?);
    }
    Ok(predictions)
}

/// Atomically write official SWE-bench prediction JSONL under `root`. Existing
/// output is refused so a new run cannot silently overwrite an artifact that
/// the official harness may cache by run id.
pub fn write_swebench_predictions(
    root: &Path,
    output: &Path,
    predictions: &[SweBenchPredictionV1],
) -> anyhow::Result<PathBuf> {
    if predictions.is_empty() {
        anyhow::bail!("SWE-bench predictions must not be empty");
    }
    let root = root
        .canonicalize()
        .map_err(|error| anyhow::anyhow!("cannot resolve SWE-bench output root: {error}"))?;
    let output = root_relative_path(&root, output);
    let parent = output
        .parent()
        .ok_or_else(|| anyhow::anyhow!("SWE-bench predictions need a parent directory"))?
        .canonicalize()
        .map_err(|error| {
            anyhow::anyhow!("cannot resolve SWE-bench prediction directory: {error}")
        })?;
    if !parent.starts_with(&root) {
        anyhow::bail!("SWE-bench prediction path is outside workspaces root");
    }
    if output.exists() {
        anyhow::bail!("refusing to overwrite existing SWE-bench predictions");
    }
    let temp = parent.join(format!(
        ".ridgecode-swebench-{}-{}.tmp",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let write_result = (|| -> anyhow::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        for prediction in predictions {
            serde_json::to_writer(&mut file, prediction)?;
            file.write_all(b"\n")?;
        }
        file.sync_all()?;
        fs::rename(&temp, &output)?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    write_result?;
    Ok(output)
}

async fn run_swebench_agent(
    task: &CaseSpecV1,
    workspace: &Path,
    ridgecode_path: &Path,
    options: &SweBenchExportOptions,
) -> bool {
    let Ok(task_file) = write_external_task(task) else {
        return false;
    };
    let mut agent = Command::new(ridgecode_path);
    agent
        .current_dir(workspace)
        .arg("run")
        .arg("--task-file")
        .arg(&task_file)
        .arg("--jsonl")
        .arg("--no-persist")
        .arg("--isolate-runtime")
        .arg("--max-turns")
        .arg(options.max_turns.to_string())
        .arg("--timeout")
        .arg(format_duration_seconds(options.timeout));
    if options.require_api_key {
        agent.arg("--require-api-key");
    }
    if let Some(budget_tokens) = options.budget_tokens {
        agent.arg("--budget-tokens").arg(budget_tokens.to_string());
    }
    let output = run_bounded_command(agent, options.timeout).await;
    let _ = fs::remove_file(&task_file);
    matches!(output, Ok(output) if !output.timed_out && parse_machine_run(&output.stdout).is_ok())
}

async fn capture_git_diff(workspace: &Path, timeout: Duration) -> anyhow::Result<String> {
    let mut git = Command::new("git");
    git.current_dir(workspace)
        .arg("diff")
        .arg("--binary")
        .arg("--no-ext-diff");
    let output = run_bounded_command(git, timeout)
        .await
        .map_err(|_| anyhow::anyhow!("cannot capture SWE-bench git diff"))?;
    if output.timed_out {
        anyhow::bail!("SWE-bench git diff timed out");
    }
    if !output.status.success() {
        anyhow::bail!("SWE-bench workspace is not a usable git repository");
    }
    String::from_utf8(output.stdout).map_err(|_| anyhow::anyhow!("SWE-bench git diff is not UTF-8"))
}

fn root_relative_path(root: &Path, candidate: &Path) -> PathBuf {
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        root.join(candidate)
    }
}

async fn run_external_case(
    case: ExternalEvalCaseV1,
    workspace: PathBuf,
    verifier_program: PathBuf,
    ridgecode_path: &Path,
    options: &ExternalEvalOptions,
) -> CaseResultV1 {
    let task_file = match write_external_task(&case.case) {
        Ok(path) => path,
        Err(error) => {
            return failed_external_case(
                case.case,
                CaseStatus::Failed,
                false,
                0,
                0,
                0,
                ExternalVerification::error(&case.verifier.name),
            )
            .with_verifier_summary(format!("task-file-error={}", stable_error_kind(&error)));
        }
    };
    let started = Instant::now();
    let mut agent = Command::new(ridgecode_path);
    agent
        .current_dir(&workspace)
        .arg("run")
        .arg("--task-file")
        .arg(&task_file)
        .arg("--jsonl")
        .arg("--no-persist")
        .arg("--isolate-runtime")
        .arg("--max-turns")
        .arg(options.max_turns.to_string())
        .arg("--timeout")
        .arg(format_duration_seconds(options.timeout));
    if options.read_only {
        agent.arg("--read-only");
    }
    if options.require_api_key {
        agent.arg("--require-api-key");
    }
    if let Some(budget_tokens) = options.budget_tokens {
        agent.arg("--budget-tokens").arg(budget_tokens.to_string());
    }
    let agent_output = run_bounded_command(agent, options.timeout).await;
    let _ = fs::remove_file(&task_file);
    let process_duration_ms = started.elapsed().as_millis() as u64;
    let machine = match agent_output {
        Ok(output) if output.timed_out => {
            return failed_external_case(
                case.case,
                CaseStatus::TimedOut,
                false,
                0,
                0,
                process_duration_ms,
                ExternalVerification::not_run(&case.verifier.name),
            )
            .with_verifier_summary("agent-timeout".to_string());
        }
        Ok(output) => match parse_machine_run(&output.stdout) {
            Ok(machine) => machine,
            Err(_) => {
                return failed_external_case(
                    case.case,
                    CaseStatus::Failed,
                    false,
                    0,
                    0,
                    process_duration_ms,
                    ExternalVerification::not_run(&case.verifier.name),
                )
                .with_verifier_summary("machine-result-invalid".to_string());
            }
        },
        Err(_) => {
            return failed_external_case(
                case.case,
                CaseStatus::Failed,
                false,
                0,
                0,
                process_duration_ms,
                ExternalVerification::not_run(&case.verifier.name),
            )
            .with_verifier_summary("agent-start-error".to_string());
        }
    };

    let verifier_started = Instant::now();
    let mut verifier = Command::new(verifier_program);
    verifier.current_dir(&workspace).args(&case.verifier.args);
    let external_verification = match run_bounded_command(verifier, options.timeout).await {
        Ok(output) if output.timed_out => verification_with_summary(
            ExternalVerification::error(&case.verifier.name),
            verifier_started.elapsed(),
            "verifier-timeout",
        ),
        Ok(output) if output.status.success() => verification_with_summary(
            ExternalVerification::passed(&case.verifier.name),
            verifier_started.elapsed(),
            "exit=0",
        ),
        Ok(output) => verification_with_summary(
            ExternalVerification::failed(&case.verifier.name),
            verifier_started.elapsed(),
            &format!("exit={}", output.status.code().unwrap_or(-1)),
        ),
        Err(_) => verification_with_summary(
            ExternalVerification::error(&case.verifier.name),
            verifier_started.elapsed(),
            "verifier-start-error",
        ),
    };
    CaseResultV1 {
        schema_version: MACHINE_EVAL_SCHEMA_VERSION,
        case: case.case,
        agent_approved: machine.approved,
        external_verification,
        steps: machine.steps,
        tokens: machine.total_tokens,
        duration_ms: machine.elapsed_ms.max(process_duration_ms),
        status: CaseStatus::Completed,
        timed_out: false,
    }
}

fn contained_path(root: &Path, candidate: &Path, label: &str) -> anyhow::Result<PathBuf> {
    let candidate = candidate
        .canonicalize()
        .map_err(|error| anyhow::anyhow!("cannot resolve {label}: {error}"))?;
    if !candidate.starts_with(root) {
        anyhow::bail!("{label} is outside corpus root");
    }
    if !candidate.is_dir() {
        anyhow::bail!("{label} is not a directory");
    }
    Ok(candidate)
}

fn verifier_program(root: &Path, program: &Path) -> anyhow::Result<PathBuf> {
    if program.is_absolute() || program.components().count() > 1 {
        let program = root_relative_path(root, program)
            .canonicalize()
            .map_err(|error| anyhow::anyhow!("cannot resolve verifier program: {error}"))?;
        if !program.starts_with(root) {
            anyhow::bail!("verifier program is outside corpus root");
        }
        if !program.is_file() {
            anyhow::bail!("verifier program is not a file");
        }
        Ok(program)
    } else if program.as_os_str().is_empty() {
        anyhow::bail!("verifier program must not be empty");
    } else {
        // A bare program such as `cargo` is resolved by PATH.  It is still
        // argv-only; paths with separators must be contained in the corpus.
        Ok(program.to_path_buf())
    }
}

fn write_external_task(case: &CaseSpecV1) -> anyhow::Result<PathBuf> {
    let id = case
        .case_id
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
        .take(48)
        .collect::<String>();
    let path = std::env::temp_dir().join(format!(
        "ridgecode-external-eval-{}-{}-{}.txt",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        if id.is_empty() { "case" } else { &id }
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    file.write_all(case.task.as_bytes())?;
    file.sync_all()?;
    Ok(path)
}

fn format_duration_seconds(duration: Duration) -> String {
    format!("{}s", duration.as_secs().max(1))
}

#[derive(Debug, Deserialize)]
struct MachineRunFinishV1 {
    event: String,
    #[serde(default)]
    approved: bool,
    #[serde(default)]
    steps: usize,
    #[serde(default)]
    total_tokens: usize,
    #[serde(default)]
    tokens: usize,
    #[serde(default)]
    elapsed_ms: u64,
}

fn parse_machine_run(stdout: &[u8]) -> anyhow::Result<MachineRunFinishV1> {
    let stdout =
        std::str::from_utf8(stdout).map_err(|_| anyhow::anyhow!("machine output invalid"))?;
    stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<MachineRunFinishV1>(line).ok())
        .rev()
        .find(|record| record.event == "run_finished")
        .map(|mut record| {
            if record.total_tokens == 0 {
                record.total_tokens = record.tokens;
            }
            record
        })
        .ok_or_else(|| anyhow::anyhow!("machine output lacks run_finished"))
}

struct BoundedCommandOutput {
    status: std::process::ExitStatus,
    timed_out: bool,
    stdout: Vec<u8>,
}

async fn run_bounded_command(
    mut command: Command,
    timeout: Duration,
) -> anyhow::Result<BoundedCommandOutput> {
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("child stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("child stderr unavailable"))?;
    let stdout_task = tokio::spawn(read_bounded(stdout));
    let stderr_task = tokio::spawn(read_bounded(stderr));
    let (status, timed_out) = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(result) => (result?, false),
        Err(_) => {
            let _ = child.kill().await;
            (child.wait().await?, true)
        }
    };
    let stdout = stdout_task
        .await
        .map_err(|_| anyhow::anyhow!("stdout reader failed"))??;
    let _ = stderr_task
        .await
        .map_err(|_| anyhow::anyhow!("stderr reader failed"))??;
    Ok(BoundedCommandOutput {
        status,
        timed_out,
        stdout,
    })
}

async fn read_bounded<R: AsyncRead + Unpin>(mut reader: R) -> std::io::Result<Vec<u8>> {
    let mut retained = Vec::with_capacity(MAX_EXTERNAL_OUTPUT_BYTES);
    let mut buffer = [0u8; 4096];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            return Ok(retained);
        }
        let remaining = MAX_EXTERNAL_OUTPUT_BYTES.saturating_sub(retained.len());
        retained.extend_from_slice(&buffer[..read.min(remaining)]);
    }
}

fn verification_with_summary(
    mut verification: ExternalVerification,
    elapsed: Duration,
    summary: &str,
) -> ExternalVerification {
    verification.duration_ms = elapsed.as_millis() as u64;
    verification.summary = Some(summary.to_string());
    verification
}

fn failed_external_case(
    case: CaseSpecV1,
    status: CaseStatus,
    agent_approved: bool,
    steps: usize,
    tokens: usize,
    duration_ms: u64,
    external_verification: ExternalVerification,
) -> CaseResultV1 {
    CaseResultV1 {
        schema_version: MACHINE_EVAL_SCHEMA_VERSION,
        case,
        agent_approved,
        external_verification,
        steps,
        tokens,
        duration_ms,
        status: status.clone(),
        timed_out: status == CaseStatus::TimedOut,
    }
}

trait CaseResultExternalSummary {
    fn with_verifier_summary(self, summary: String) -> Self;
}

impl CaseResultExternalSummary for CaseResultV1 {
    fn with_verifier_summary(mut self, summary: String) -> Self {
        self.external_verification.summary = Some(summary);
        self
    }
}

fn stable_error_kind(error: &anyhow::Error) -> &'static str {
    if error.downcast_ref::<std::io::Error>().is_some() {
        "io"
    } else {
        "other"
    }
}

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
    /// Require at least this many durable file mutations.
    ModifiedFilesAtLeast(usize),
    /// Require a named tool call to appear in the completed trace.
    ToolUsed(String),
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

    pub fn modified_files_at_least(limit: usize) -> Self {
        Self::ModifiedFilesAtLeast(limit)
    }

    pub fn tool_used(name: impl Into<String>) -> Self {
        Self::ToolUsed(name.into())
    }
}

/// Stable, non-sensitive category for one invariant observation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvariantKind {
    Approved,
    Steps,
    Tokens,
    Marker,
    ModifiedFiles,
    Tool,
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

    pub fn require_modified_files(self, minimum: usize) -> Self {
        self.with_invariant(Invariant::ModifiedFilesAtLeast(minimum))
    }

    pub fn require_tool(self, tool: impl Into<String>) -> Self {
        self.with_invariant(Invariant::ToolUsed(tool.into()))
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
    /// Durable facts retained for long-task diagnostics and manifest review.
    #[serde(default)]
    pub modified_files: Vec<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub explore_handoffs: usize,
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
            Invariant::ModifiedFilesAtLeast(limit) => {
                hasher.update([4]);
                hasher.update((*limit as u64).to_le_bytes());
            }
            Invariant::ToolUsed(tool) => {
                hasher.update([5]);
                update_text(&mut hasher, tool);
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
            let facts = trace_facts(&state);
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
                modified_files: facts.modified_files,
                tools: facts.tools,
                explore_handoffs: facts.explore_handoffs,
            }
        }
        CaseExecution::Incomplete { status, state } => {
            let timed_out = status == CaseStatus::TimedOut;
            let invariants = failed_invariants(
                &options.invariants,
                options.max_invariants,
                state.as_deref(),
            );
            let facts = state.as_deref().map(trace_facts).unwrap_or_default();
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
                modified_files: facts.modified_files,
                tools: facts.tools,
                explore_handoffs: facts.explore_handoffs,
            }
        }
    }
}

#[derive(Default)]
struct TraceFacts {
    modified_files: Vec<String>,
    tools: Vec<String>,
    explore_handoffs: usize,
}

fn trace_facts(state: &AgentState) -> TraceFacts {
    let mut tools = BTreeSet::new();
    for message in &state.history {
        for call in &message.tool_calls {
            tools.insert(call.name.clone());
        }
    }
    TraceFacts {
        modified_files: state.modified_files.iter().cloned().collect(),
        tools: tools.into_iter().collect(),
        explore_handoffs: state
            .messages
            .iter()
            .filter(|message| message.starts_with("control: exploration guard triggered"))
            .count(),
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
            Invariant::ModifiedFilesAtLeast(limit) => InvariantEvidence {
                kind: InvariantKind::ModifiedFiles,
                passed: state.modified_files.len() >= *limit,
                observed: state.modified_files.len(),
                limit: Some(*limit),
            },
            Invariant::ToolUsed(tool) => {
                let present = tool_used(state, tool);
                InvariantEvidence {
                    kind: InvariantKind::Tool,
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
            Invariant::ModifiedFilesAtLeast(limit) => InvariantEvidence {
                kind: InvariantKind::ModifiedFiles,
                passed: false,
                observed: state.map_or(0, |state| state.modified_files.len()),
                limit: Some(*limit),
            },
            Invariant::ToolUsed(tool) => InvariantEvidence {
                kind: InvariantKind::Tool,
                passed: false,
                observed: state.map_or(0, |state| usize::from(tool_used(state, tool))),
                limit: Some(1),
            },
        })
        .collect()
}

fn tool_used(state: &AgentState, tool: &str) -> bool {
    !tool.trim().is_empty()
        && state
            .history
            .iter()
            .flat_map(|message| &message.tool_calls)
            .any(|call| call.name == tool)
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
    use std::time::{SystemTime, UNIX_EPOCH};

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

    fn approved_case_result() -> CaseResult {
        CaseResult {
            name: "internally-approved".to_string(),
            approved: true,
            steps: 3,
            tokens: 42,
            duration_ms: 7,
            status: CaseStatus::Completed,
            timed_out: false,
            invariants: Vec::new(),
            evidence: Vec::new(),
            modified_files: Vec::new(),
            tools: Vec::new(),
            explore_handoffs: 0,
        }
    }

    #[test]
    fn internal_approval_is_not_external_success_until_verifier_passes() {
        let spec = CaseSpecV1::new("external-gate", "make the hidden checks pass");
        let internal = approved_case_result();

        let not_run = CaseResultV1::from_case_result(
            spec.clone(),
            &internal,
            ExternalVerification::not_run("hidden-tests-v1"),
        );
        assert!(not_run.agent_approved);
        assert!(!not_run.externally_verified_success());

        let failed = CaseResultV1::from_case_result(
            spec.clone(),
            &internal,
            ExternalVerification::failed("hidden-tests-v1"),
        );
        assert!(failed.agent_approved);
        assert!(!failed.externally_verified_success());

        let passed = CaseResultV1::from_case_result(
            spec,
            &internal,
            ExternalVerification::passed("hidden-tests-v1"),
        );
        assert!(passed.agent_approved);
        assert!(passed.externally_verified_success());
    }

    #[test]
    fn external_result_schema_round_trips_and_manifest_counts_only_verifier_passes() {
        let internal = approved_case_result();
        let not_run = CaseResultV1::from_case_result(
            CaseSpecV1::new("not-run", "task"),
            &internal,
            ExternalVerification::not_run("hidden-tests-v1"),
        );
        let passed = CaseResultV1::from_case_result(
            CaseSpecV1::new("passed", "task"),
            &internal,
            ExternalVerification::passed("hidden-tests-v1"),
        );
        let manifest = ExperimentManifestV1::new("external-foundation", vec![not_run, passed]);

        let json = serde_json::to_string(&manifest).expect("serialize machine manifest");
        let decoded: ExperimentManifestV1 =
            serde_json::from_str(&json).expect("deserialize machine manifest");

        assert_eq!(decoded.schema_version, MACHINE_EVAL_SCHEMA_VERSION);
        assert_eq!(decoded.externally_verified_passed(), 1);
        assert_eq!(
            decoded.results[0].external_verification.status,
            ExternalVerificationStatus::NotRun
        );
        assert_eq!(
            decoded.results[1].external_verification.status,
            ExternalVerificationStatus::Passed
        );
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
    async fn long_exploration_harness_reaches_edit_and_reports_facts() {
        let path = format!("target/quality/ridge-long-task-{}.txt", std::process::id());
        std::fs::create_dir_all("target/quality").expect("quality directory");
        std::fs::write(&path, "old\n").expect("fixture file");

        let mut steps = (0..12)
            .map(|index| Completion {
                tool_calls: vec![ToolCall {
                    id: format!("read-{index}"),
                    name: "read_file".into(),
                    arguments: json!({"path": path.clone(), "limit": 4}),
                }],
                ..Default::default()
            })
            .collect::<Vec<_>>();
        steps.push(Completion {
            tool_calls: vec![ToolCall {
                id: "edit".into(),
                name: "edit_file".into(),
                arguments: json!({
                    "path": path.clone(),
                    "old_string": "old",
                    "new_string": "new"
                }),
            }],
            ..Default::default()
        });
        steps.push(Completion {
            text: "done".into(),
            ..Default::default()
        });

        let report = run_eval_with_options(
            vec![EvalCase::new(
                "long-edit-handoff",
                format!("edit {path} then verify"),
                Arc::new(ScriptedProvider::new(steps)),
            )],
            HarnessOptions::default().with_invariants([
                Invariant::Approved,
                Invariant::ModifiedFilesAtLeast(1),
                Invariant::ToolUsed("edit_file".into()),
                Invariant::MaxSteps(40),
            ]),
        )
        .await
        .expect("long-task harness");

        let _ = std::fs::remove_file(&path);
        let result = &report.results[0];
        assert!(
            result.approved,
            "long task must enter edit phase: {result:?}"
        );
        assert_eq!(result.explore_handoffs, 1);
        assert!(result.modified_files.iter().any(|file| file == &path));
        assert!(result.tools.iter().any(|tool| tool == "edit_file"));
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

    #[test]
    fn swebench_prediction_uses_the_official_three_field_contract() {
        let prediction = SweBenchPredictionV1::new(
            "sympy__sympy-20590",
            "ridgecode/glm-5.3",
            "diff --git a/x b/x\n",
        )
        .expect("a normal SWE-bench id is valid");
        let value = serde_json::to_value(&prediction).unwrap();
        assert_eq!(value["instance_id"], "sympy__sympy-20590");
        assert_eq!(value["model_name_or_path"], "ridgecode/glm-5.3");
        assert_eq!(value["model_patch"], "diff --git a/x b/x\n");
        assert_eq!(value.as_object().unwrap().len(), 3);
        assert!(SweBenchPredictionV1::new("../escape", "ridgecode", "").is_err());
    }

    #[tokio::test]
    async fn swebench_export_captures_git_patch_and_writes_official_jsonl() {
        let root = external_eval_test_root();
        let instance_id = "owner__repo-1";
        let workspace = root.join(instance_id);
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("answer.txt"), "STATUS=TODO\n").unwrap();
        git_in(&workspace, ["init", "-q"]);
        git_in(&workspace, ["add", "answer.txt"]);
        let agent = write_external_eval_script(&root, "fake-ridgecode", swebench_agent_body());
        let predictions = run_swebench_export(
            vec![SweBenchInstanceV1 {
                instance_id: instance_id.to_string(),
                problem_statement: "Set the answer status to DONE.".to_string(),
            }],
            SweBenchExportOptions::new(&root, agent, "ridgecode/fixture")
                .with_timeout(Duration::from_secs(5)),
        )
        .await
        .expect("fixture SWE-bench export should succeed");

        assert_eq!(predictions.len(), 1);
        assert!(predictions[0].model_patch.contains("STATUS=DONE"));
        assert_isolated_runner_args(&workspace.join("agent-args.txt"));
        let output =
            write_swebench_predictions(&root, Path::new("predictions.jsonl"), &predictions)
                .expect("prediction output must remain inside root");
        let line = std::fs::read_to_string(&output).unwrap();
        let decoded: SweBenchPredictionV1 = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(decoded, predictions[0]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn swebench_prediction_writer_rejects_output_outside_root() {
        let root = external_eval_test_root();
        let prediction = SweBenchPredictionV1::new("owner__repo-1", "ridgecode", "").unwrap();
        let error = write_swebench_predictions(
            &root,
            &root.join("..").join("escaped.jsonl"),
            &[prediction],
        )
        .expect_err("prediction output must be contained");
        assert!(error.to_string().contains("outside workspaces root"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn swebench_score_uses_only_official_resolved_flags_and_rejects_duplicates() {
        let root = external_eval_test_root();
        let first = root.join("model").join("owner__repo-1");
        let second = root.join("model").join("owner__repo-2");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(
            first.join("report.json"),
            r#"{"owner__repo-1":{"resolved":true,"extra":"ignored"}}"#,
        )
        .unwrap();
        std::fs::write(
            second.join("report.json"),
            r#"{"owner__repo-2":{"resolved":false}}"#,
        )
        .unwrap();
        let score = score_swebench_reports(&root).expect("official reports should score");
        assert_eq!((score.total, score.resolved, score.unresolved), (2, 1, 1));
        assert_eq!(score.resolution_rate, 0.5);

        let duplicate = root.join("duplicate");
        std::fs::create_dir_all(&duplicate).unwrap();
        std::fs::write(
            duplicate.join("report.json"),
            r#"{"owner__repo-1":{"resolved":true}}"#,
        )
        .unwrap();
        assert!(score_swebench_reports(&root).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn external_eval_scores_the_verifier_not_agent_approval() {
        let root = external_eval_test_root();
        let workspace = root.join("case");
        std::fs::create_dir_all(&workspace).unwrap();
        let agent = write_external_eval_script(
            &root,
            "fake-ridgecode",
            r#"echo {"event":"run_finished","approved":false,"steps":2,"tokens":3,"elapsed_ms":4,"outcome":"unverified"}"#,
        );
        let verifier = write_external_eval_script(&root, "verifier", "exit 0");
        let case = ExternalEvalCaseV1 {
            case: CaseSpecV1::new("verifier-wins", "do a bounded task"),
            workspace,
            verifier: ExternalVerifierV1 {
                name: "fixture-verifier".to_string(),
                program: verifier,
                args: Vec::new(),
            },
        };

        let report = run_external_eval(
            vec![case],
            ExternalEvalOptions::new(&root, agent, "external-eval-test")
                .with_timeout(Duration::from_secs(5)),
        )
        .await
        .expect("fixture commands should run");

        assert_eq!(report.results.len(), 1);
        assert!(!report.results[0].agent_approved);
        assert!(
            report.results[0].externally_verified_success(),
            "unexpected external result: {:?}",
            report.results[0]
        );
        assert_eq!(report.externally_verified_passed(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn external_eval_forces_isolated_machine_runtime() {
        let root = external_eval_test_root();
        let workspace = root.join("case");
        std::fs::create_dir_all(&workspace).unwrap();
        let agent = write_external_eval_script(&root, "fake-ridgecode", isolated_agent_body());
        let verifier = write_external_eval_script(&root, "verifier", isolated_args_verifier_body());
        let case = ExternalEvalCaseV1 {
            case: CaseSpecV1::new("isolated-runtime", "do a bounded task"),
            workspace: workspace.clone(),
            verifier: ExternalVerifierV1 {
                name: "isolated-args".to_string(),
                program: verifier,
                args: Vec::new(),
            },
        };
        let report = run_external_eval(
            vec![case],
            ExternalEvalOptions::new(&root, agent, "isolated-runtime-test")
                .with_timeout(Duration::from_secs(5)),
        )
        .await
        .expect("fixture commands should run");
        assert_eq!(report.externally_verified_passed(), 1, "{report:?}");
        assert_isolated_runner_args(&workspace.join("agent-args.txt"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn external_eval_rejects_case_workspace_outside_corpus() {
        let root = external_eval_test_root();
        let outside = std::env::temp_dir().join(format!(
            "ridgecode-external-eval-outside-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&outside).unwrap();
        let agent = write_external_eval_script(&root, "fake-ridgecode", "exit 0");
        let verifier = write_external_eval_script(&root, "verifier", "exit 0");
        let error = run_external_eval(
            vec![ExternalEvalCaseV1 {
                case: CaseSpecV1::new("outside", "must not execute"),
                workspace: outside.clone(),
                verifier: ExternalVerifierV1 {
                    name: "fixture-verifier".to_string(),
                    program: verifier,
                    args: Vec::new(),
                },
            }],
            ExternalEvalOptions::new(&root, agent, "external-eval-test"),
        )
        .await
        .expect_err("case roots must be contained");
        assert!(error.to_string().contains("outside corpus root"));
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    fn external_eval_test_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "ridgecode-external-eval-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_external_eval_script(root: &Path, stem: &str, body: &str) -> PathBuf {
        #[cfg(windows)]
        let path = root.join(format!("{stem}.cmd"));
        #[cfg(not(windows))]
        let path = root.join(stem);
        #[cfg(windows)]
        std::fs::write(&path, format!("@echo off\r\n{body}\r\n")).unwrap();
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&path, permissions).unwrap();
        }
        path
    }

    #[cfg(windows)]
    fn swebench_agent_body() -> &'static str {
        r#"echo %* > agent-args.txt & echo STATUS=DONE> answer.txt & echo {"event":"run_finished","approved":false,"steps":1,"tokens":3,"elapsed_ms":4}"#
    }

    #[cfg(not(windows))]
    fn swebench_agent_body() -> &'static str {
        r#"printf '%s\n' "$@" > agent-args.txt; printf 'STATUS=DONE\n' > answer.txt; echo '{"event":"run_finished","approved":false,"steps":1,"tokens":3,"elapsed_ms":4}'"#
    }

    #[cfg(windows)]
    fn isolated_agent_body() -> &'static str {
        r#"echo %* > agent-args.txt & echo {"event":"run_finished","approved":false,"steps":1,"tokens":3,"elapsed_ms":4}"#
    }

    #[cfg(not(windows))]
    fn isolated_agent_body() -> &'static str {
        r#"printf '%s\n' "$@" > agent-args.txt; echo '{"event":"run_finished","approved":false,"steps":1,"tokens":3,"elapsed_ms":4}'"#
    }

    #[cfg(windows)]
    fn isolated_args_verifier_body() -> &'static str {
        r#"findstr /C:"--isolate-runtime" agent-args.txt >nul || exit /b 1 & findstr /C:"--no-persist" agent-args.txt >nul"#
    }

    #[cfg(not(windows))]
    fn isolated_args_verifier_body() -> &'static str {
        r#"grep -F -- '--isolate-runtime' agent-args.txt >/dev/null && grep -F -- '--no-persist' agent-args.txt >/dev/null"#
    }

    fn assert_isolated_runner_args(path: &Path) {
        let args = std::fs::read_to_string(path).expect("fake runner should record arguments");
        assert!(args.contains("--isolate-runtime"), "runner args: {args}");
        assert!(args.contains("--no-persist"), "runner args: {args}");
    }

    fn git_in<const N: usize>(workspace: &Path, args: [&str; N]) {
        let status = std::process::Command::new("git")
            .current_dir(workspace)
            .args(args)
            .status()
            .expect("git must be installed for this repository's SWE-bench adapter test");
        assert!(status.success(), "git command failed");
    }
}
