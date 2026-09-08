use crate::dispatch_budget::MAX_DISPATCH_ATTEMPTS;
use langgraph::{GraphState, RunConfig};
use provider::{Message, ToolCall, ToolEffect, Usage};
use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;

/// 回合上限 —— **防跑飞的后备护栏**,非正常终止手段。真正的停机主力是:`approved`(目标达成)、
/// 无进展检测(`stalled`,连 3 轮同输出即停)、连错熔断(`circuit_broken`,连 5 轮报错即停)。
/// 抬到 2000:让**真实长任务能跑完**,而非被腰斩(用户诉求)。命中此上限**不是硬杀**——经 `wrapup`
/// 软中止,让模型总结进度 + 规划后续供用户参考(见 `verify_route_llm`)。预算护栏默认关(`budget_tokens=0`),
/// 卡死由 stall/circuit 早停兜底,故 2000 只有**持续有进展**的长任务才会触达。
/// 注意:上限一抬,引擎超步上限须随之派生(见 [`agent_run_config`]),否则先撞引擎默认 100 超步的 `StepLimit`。
pub const MAX_STEPS: usize = 2000;

/// 一个运行最多允许多少个并行 dispatch wave。每 wave 仍可含 2–3 个 sub-agent；
/// 这是防循环重试的运行级护栏，不是“一次运行只能派一批”。
pub const MAX_DISPATCH_BATCHES: usize = 8;

/// 本 agent 的运行参数:引擎超步上限据 [`MAX_STEPS`] **派生**(每 step ≈ 2 超步 reason+act,
/// 加收尾余量 verify+wrapup)。使「跑多久」真正由 MAX_STEPS 决定,不被引擎默认 100 超步提前腰斩。
pub fn agent_run_config() -> RunConfig {
    RunConfig {
        max_supersteps: MAX_STEPS * 2 + 50,
    }
}

/// 一条任务清单项(像 Claude Code 的 TodoWrite):`status` ∈ `pending` / `in_progress` / `completed`。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Todo {
    pub content: String,
    pub status: String,
}

/// A requirement is a first-class completion target rather than an implicit
/// sentence somewhere in the chat history.  `Satisfied` is meaningful only
/// when the requirement names evidence from the current workspace revision.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementStatus {
    #[default]
    Unknown,
    Satisfied,
    Failed,
    Blocked,
    Waived,
}

impl RequirementStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Satisfied => "satisfied",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
            Self::Waived => "waived",
        }
    }
}

/// One explicitly named acceptance requirement in [`TaskContract`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirement {
    pub id: String,
    pub description: String,
    #[serde(default)]
    pub status: RequirementStatus,
    #[serde(default)]
    pub evidence_call_ids: Vec<String>,
}

/// Structured, bounded task intent.  It is optional during the migration so
/// older checkpoints and simple one-shot questions retain their behaviour.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskContract {
    pub objective: String,
    #[serde(default)]
    pub requirements: Vec<Requirement>,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub deliverables: Vec<String>,
    #[serde(default)]
    pub non_goals: Vec<String>,
}

/// An immutable, bounded reference to an actual completed tool call.  The
/// reducer stamps the revision, so a model cannot claim a later revision by
/// fabricating a number in its tool arguments.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub call_id: String,
    pub tool: String,
    pub succeeded: bool,
    #[serde(default)]
    pub workspace_revision: usize,
    pub summary: String,
}

/// Stable outcome category for a tool call.  The text observation remains for
/// humans and legacy providers, while state transitions consume this enum.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolResultStatus {
    #[default]
    Success,
    Error,
    Blocked,
    Running,
}

impl ToolResultStatus {
    pub fn is_success(self) -> bool {
        self == Self::Success
    }

    pub fn is_error(self) -> bool {
        matches!(self, Self::Error | Self::Blocked)
    }

    /// A verifier may proceed only after a successful, settled tool result.
    /// `blocked` and `running` are operationally distinct, but neither is
    /// evidence that the requested work completed.
    pub fn blocks_completion(self) -> bool {
        !self.is_success()
    }
}

/// Versioned, bounded machine-readable projection of an executed tool call.
/// It is deliberately separate from UI text so verification and recovery do
/// not have to infer status from prose.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResultV1 {
    pub schema_version: u8,
    pub call_id: String,
    pub tool: String,
    pub effect: ToolEffect,
    pub status: ToolResultStatus,
    pub exit_code: Option<i32>,
    pub retryable: bool,
    #[serde(default)]
    pub changed_paths: Vec<String>,
    /// Rich ACI facts for bounded read/search tools. Optional for checkpoint
    /// compatibility with pre-rich-result state.
    #[serde(default)]
    pub output_truncated: bool,
    #[serde(default)]
    pub read_offset: Option<usize>,
    #[serde(default)]
    pub read_limit: Option<usize>,
    #[serde(default)]
    pub match_count: Option<usize>,
    #[serde(default)]
    pub workspace_revision: usize,
    pub summary: String,
}

/// A requested requirement state transition emitted by `requirement_update`.
/// Validation happens against the current ledger before the reducer applies it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequirementUpdate {
    pub id: String,
    pub status: RequirementStatus,
    pub evidence_call_ids: Vec<String>,
}

/// agent 的共享状态。`messages` 是事件轨迹(reducer 追加),其余字段覆盖。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AgentState {
    pub task: String,
    /// Optional structured contract emitted by the `contract_write` tool.
    /// Keeping this optional is a checkpoint-compatible migration path.
    #[serde(default)]
    pub task_contract: Option<TaskContract>,
    /// Monotonic revision for successful edit effects. Evidence is stamped at
    /// this value so a later edit invalidates older completion proof.
    #[serde(default)]
    pub workspace_revision: usize,
    /// Bounded ledger of real tool results. This never accepts model prose as
    /// evidence and is the source used to validate requirement transitions.
    #[serde(default)]
    pub evidence_ledger: Vec<EvidenceRef>,
    /// Most recent typed tool result and bounded history for context/recovery.
    #[serde(default)]
    pub last_tool_result: Option<ToolResultV1>,
    #[serde(default)]
    pub recent_tool_results: Vec<ToolResultV1>,
    pub messages: Vec<String>,
    /// Presentation-only event stream. Unlike `messages`, tool observations
    /// keep their original text so the TUI can offer a complete audit view;
    /// model context and reviewer input continue using the bounded stream.
    pub display_messages: Vec<String>,
    pub last_action: Option<String>,
    pub tool_output: Option<String>,
    pub approved: bool,
    pub steps: usize,
    /// Per-run cap on model reasoning turns. `0` retains [`MAX_STEPS`] so
    /// checkpoints produced before this field existed remain long-task safe.
    #[serde(default)]
    pub reasoning_step_limit: usize,
    pub issues: Vec<String>,
    /// 由 reason 节点(真实 LLM 路径)产出、待 act 节点执行的结构化工具调用。
    ///
    /// This is retained as a compatibility mirror of the queue head for
    /// older checkpoints and presentation code. Runtime control flow must use
    /// [`Self::next_pending_call`] / [`Self::has_pending_calls`] so a provider
    /// response containing more than one call is never silently discarded.
    #[serde(default)]
    pub pending_call: Option<ToolCall>,
    /// Ordered tool-call queue returned by the most recent model completion.
    /// `act` executes exactly one entry per graph turn and dequeues it only
    /// after recording that entry's observation.
    #[serde(default)]
    pub pending_calls: Vec<ToolCall>,
    /// 累计消耗的 token(成本记账)。
    pub total_tokens: usize,
    /// provider 回传的输入 token 累计，用于 TUI 成本分栏。
    pub input_tokens: usize,
    /// provider 回传的输出 token 累计，用于 TUI 成本分栏。
    pub output_tokens: usize,
    /// token 预算(0 = 不限)。超了就熔断停机。
    pub budget_tokens: usize,
    /// 连续「无进展」轮数(工具输出与上一轮相同)。到 [`MAX_STALL`] 就熔断。
    pub stall: usize,
    /// 连续**工具/provider 报错**轮数(与 `stall` 正交:stall 认「输出相同」,本字段认「输出为错误」,
    /// 故报错内容**每轮不同**时 stall 不触发、由本字段兜底)。到 [`MAX_ERR_STREAK`] 熔断,防无人值守烧预算。
    pub err_streak: usize,
    /// Consecutive policy denials in this run. Unlike `stall`, this is based
    /// on structured status so changing prose or switching explore tools does
    /// not evade the loop guard. A successful edit or verification clears it.
    #[serde(default)]
    pub policy_blocked_streak: usize,
    /// 连续**没有新增证据的纯侦察**轮数。新的定位路径或不同的工具结果会清零；重复读取/搜索
    /// 才累加。成功写改(`write_file`/`edit_file`/`apply_edits`)也清零。到 [`MAX_EXPLORE`] 软暂停，
    /// 防「无休止只查不改 → 撞 step_cap → 再开一轮又从侦察重来」，但不误伤复杂定位。
    pub explore_streak: usize,
    pub explore_handoff: bool,
    pub explore_action_used: bool,
    /// Number of batch-dispatch waves consumed in this run. A failed/denied
    /// attempt still consumes one wave so a model cannot loop on the same
    /// collaboration request forever.
    #[serde(
        default,
        alias = "dispatch_agents_used",
        deserialize_with = "deserialize_dispatch_batches"
    )]
    pub dispatch_batches_used: usize,
    /// Number of provider attempts consumed by dispatches in this agent run.
    /// The graph restores this from checkpoints so retries/fallbacks cannot
    /// reset the cumulative per-run ceiling.
    #[serde(default, deserialize_with = "deserialize_dispatch_attempts")]
    pub dispatch_attempts_used: usize,
    /// Session fact: the optional CodeGraph tool was unavailable, so the next
    /// reasoning turn must use the built-in bounded search/read tools.
    #[serde(default)]
    pub codegraph_unavailable: bool,
    /// **模型面向**的多轮对话历史(system 之外的部分):user / assistant(可带 tool_calls)/ tool 结果。
    /// 这是发给 provider 的真身;REPL 跨轮携带它实现多轮上下文。
    pub history: Vec<Message>,
    /// 当前任务清单(模型经 `todo_write` 维护),REPL 渲染成 `[x]/[~]/[ ]` 给用户看进度。
    pub todos: Vec<Todo>,
    /// **Durable State(持久化事实)**:本次任务已成功改动的文件路径。用 `BTreeSet` 保证**有序稳态**
    /// —— 编进 prompt 事实块时字节稳定,不抖动、利 Claude 缓存。体量 O(去重文件数),不随步数膨胀。
    pub modified_files: BTreeSet<String>,
    /// **Durable State**:上一次工具调用的核心错误摘要(去噪后首行)。事实块据它「重锚定」模型注意力,
    /// 免其在被压缩的模糊历史里遗忘卡在哪。成功时清空。
    pub last_error: Option<String>,
    /// Recently read file paths. Compact drops tool noise; this keeps the
    /// already-located edit target in the fact block.
    #[serde(default)]
    pub last_read_paths: Vec<String>,
    /// Parked `run_shell` job ids. A live job blocks successful completion.
    #[serde(default)]
    pub live_shell_jobs: Vec<String>,
    /// Effect of the last completed tool call. Verify and handoff consume the
    /// same capability fact instead of re-guessing dynamic tool names.
    #[serde(default)]
    pub last_tool_effect: ToolEffect,
    /// **信号复利**:run 启动时从 `.ridge/signals` 载入的「继承信号」有界注入块(上个会话留下的未决发现/
    /// 摩擦/待办)。run 中不变,由 CLI 在建 state 时经 [`load_signal_block`] 注入;无则 `None`。
    pub signal_block: Option<String>,
}

impl AgentState {
    pub fn new(task: impl Into<String>) -> Self {
        let task = task.into();
        Self {
            history: vec![Message::user(task.clone())],
            task,
            ..Default::default()
        }
    }

    /// 设 token 预算(loop engineering 的经济护栏之一)。
    pub fn with_budget(mut self, tokens: usize) -> Self {
        self.budget_tokens = tokens;
        self
    }

    /// Restrict this run's model reasoning turns without changing the global
    /// long-task default used by interactive sessions.
    pub fn with_reasoning_limit(mut self, limit: usize) -> Self {
        self.reasoning_step_limit = limit.clamp(1, MAX_STEPS);
        self
    }

    pub fn reasoning_limit(&self) -> usize {
        if self.reasoning_step_limit == 0 {
            MAX_STEPS
        } else {
            self.reasoning_step_limit.min(MAX_STEPS)
        }
    }

    /// Number of dispatch waves consumed, including the pre-counter boolean
    /// field used by checkpoints written during the migration to wave budgets.
    pub fn dispatch_wave_count(&self) -> usize {
        self.dispatch_batches_used
    }

    /// 用已有对话历史续跑(REPL 多轮携带上下文)。
    pub fn with_history(mut self, history: Vec<Message>) -> Self {
        self.history = history;
        self
    }

    /// 注入继承信号块(信号复利:上个会话的未决发现)。CLI 建 state 时调 [`load_signal_block`] 取之。
    pub fn with_signals(mut self, block: Option<String>) -> Self {
        self.signal_block = block;
        self
    }

    /// Whether this run still has model-requested tool work. The legacy
    /// single-call field is included so checkpoints written before the queue
    /// migration resume safely instead of skipping their pending action.
    pub fn has_pending_calls(&self) -> bool {
        !self.pending_calls.is_empty() || self.pending_call.is_some()
    }

    /// The next call to execute, preserving provider return order.
    pub fn next_pending_call(&self) -> Option<&ToolCall> {
        self.pending_calls.first().or(self.pending_call.as_ref())
    }

    fn set_pending_calls(&mut self, calls: Vec<ToolCall>) {
        self.pending_call = calls.first().cloned();
        self.pending_calls = calls;
    }

    fn dequeue_pending_call(&mut self) {
        if self.pending_calls.is_empty() {
            self.pending_call = None;
            return;
        }
        self.pending_calls.remove(0);
        self.pending_call = self.pending_calls.first().cloned();
    }

    pub fn evidence_is_current_success(&self, call_id: &str) -> bool {
        self.evidence_ledger.iter().any(|evidence| {
            evidence.call_id == call_id
                && evidence.succeeded
                && evidence.workspace_revision == self.workspace_revision
        })
    }

    /// A contract can opt into the strict requirement gate only after the
    /// model has explicitly created it. Every requirement must be satisfied
    /// with current-revision, successful tool evidence.
    pub fn contract_completion_blocked(&self) -> bool {
        let Some(contract) = &self.task_contract else {
            return false;
        };
        contract.requirements.iter().any(|requirement| {
            requirement.status != RequirementStatus::Satisfied
                || requirement.evidence_call_ids.is_empty()
                || requirement
                    .evidence_call_ids
                    .iter()
                    .any(|call_id| !self.evidence_is_current_success(call_id))
        })
    }

    pub fn validate_requirement_updates(
        &self,
        updates: &[RequirementUpdate],
    ) -> Result<(), String> {
        let contract = self
            .task_contract
            .as_ref()
            .ok_or_else(|| "no task contract has been recorded".to_string())?;
        if updates.is_empty() {
            return Err("at least one requirement update is required".to_string());
        }
        for update in updates {
            if !contract
                .requirements
                .iter()
                .any(|requirement| requirement.id == update.id)
            {
                return Err(format!("unknown requirement id `{}`", update.id));
            }
            if update.status == RequirementStatus::Satisfied
                && (update.evidence_call_ids.is_empty()
                    || update
                        .evidence_call_ids
                        .iter()
                        .any(|id| !self.evidence_is_current_success(id)))
            {
                return Err(format!(
                    "requirement `{}` needs current successful tool evidence",
                    update.id
                ));
            }
        }
        Ok(())
    }
}

fn deserialize_dispatch_batches<'de, D>(deserializer: D) -> Result<usize, D::Error>
where
    D: Deserializer<'de>,
{
    struct DispatchBatchVisitor;

    impl<'de> Visitor<'de> for DispatchBatchVisitor {
        type Value = usize;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a dispatch wave count or legacy boolean")
        }

        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            usize::try_from(value).map_err(|_| E::custom("dispatch wave count overflows usize"))
        }

        fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            if value < 0 {
                Err(E::custom("dispatch wave count cannot be negative"))
            } else {
                self.visit_u64(value as u64)
            }
        }

        fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(usize::from(value))
        }
    }

    deserializer.deserialize_any(DispatchBatchVisitor)
}

fn deserialize_dispatch_attempts<'de, D>(deserializer: D) -> Result<usize, D::Error>
where
    D: Deserializer<'de>,
{
    usize::deserialize(deserializer).map(|value| value.min(MAX_DISPATCH_ATTEMPTS))
}

/// 节点产出的增量更新(delta)。`Batch` 让一个节点一次改多个字段。
#[derive(Debug)]
pub enum Patch {
    Message(String),
    DisplayMessage(String),
    Action(Option<String>),
    ToolOutput(Option<String>),
    Approved(bool),
    Issues(Vec<String>),
    PendingCall(Option<ToolCall>),
    /// Replace the full ordered queue returned by a completion.
    PendingCalls(Vec<ToolCall>),
    /// Consume the queue head after its tool result has been persisted.
    DequeuePendingCall,
    AddTokens(usize),
    AddUsage(Usage),
    SetStall(usize),
    SetErrStreak(usize),
    SetPolicyBlockedStreak(usize),
    SetExploreStreak(usize),
    SetExploreHandoff(bool),
    SetExploreActionUsed(bool),
    SetDispatchBatches(usize),
    SetDispatchAttempts(usize),
    SetCodegraphUnavailable(bool),
    PushHistory(Message),
    SetTodos(Vec<Todo>),
    SetTaskContract(TaskContract),
    UpdateRequirements(Vec<RequirementUpdate>),
    AdvanceWorkspaceRevision,
    RecordEvidence(EvidenceRef),
    RecordToolResult(ToolResultV1),
    RecordModified(String),
    RecordRead(String),
    AddLiveShellJob(String),
    RemoveLiveShellJob(String),
    SetLastToolEffect(ToolEffect),
    SetLastError(Option<String>),
    BumpStep,
    Batch(Vec<Patch>),
}

impl GraphState for AgentState {
    type Update = Patch;
    fn apply(&mut self, u: Patch) {
        match u {
            Patch::Message(m) => {
                self.messages.push(m.clone()); // bounded execution/event stream
                self.display_messages.push(m); // complete presentation stream
            }
            Patch::DisplayMessage(m) => {
                if let Some(last) = self.display_messages.last_mut() {
                    *last = m;
                } else {
                    self.display_messages.push(m);
                }
            }
            Patch::Action(a) => self.last_action = a,
            Patch::ToolOutput(o) => self.tool_output = o,
            Patch::Approved(b) => self.approved = b,
            Patch::Issues(v) => self.issues = v,
            Patch::PendingCall(c) => self.set_pending_calls(c.into_iter().collect()),
            Patch::PendingCalls(calls) => self.set_pending_calls(calls),
            Patch::DequeuePendingCall => self.dequeue_pending_call(),
            Patch::AddTokens(n) => self.total_tokens += n,
            Patch::AddUsage(usage) => {
                self.input_tokens += usage.prompt_tokens as usize;
                self.output_tokens += usage.completion_tokens as usize;
                self.total_tokens += usage.total() as usize;
            }
            Patch::SetStall(n) => self.stall = n,
            Patch::SetErrStreak(n) => self.err_streak = n,
            Patch::SetPolicyBlockedStreak(n) => self.policy_blocked_streak = n,
            Patch::SetExploreStreak(n) => self.explore_streak = n,
            Patch::SetExploreHandoff(value) => self.explore_handoff = value,
            Patch::SetExploreActionUsed(value) => self.explore_action_used = value,
            Patch::SetDispatchBatches(n) => self.dispatch_batches_used = n,
            Patch::SetDispatchAttempts(n) => {
                self.dispatch_attempts_used = n.min(MAX_DISPATCH_ATTEMPTS)
            }
            Patch::SetCodegraphUnavailable(value) => self.codegraph_unavailable = value,
            Patch::PushHistory(m) => self.history.push(m),
            Patch::SetTodos(t) => self.todos = t,
            Patch::SetTaskContract(contract) => self.task_contract = Some(contract),
            Patch::UpdateRequirements(updates) => {
                let Some(contract) = self.task_contract.as_mut() else {
                    return;
                };
                for update in updates {
                    if let Some(requirement) = contract
                        .requirements
                        .iter_mut()
                        .find(|requirement| requirement.id == update.id)
                    {
                        requirement.status = update.status;
                        requirement.evidence_call_ids = update.evidence_call_ids;
                    }
                }
            }
            Patch::AdvanceWorkspaceRevision => {
                self.workspace_revision = self.workspace_revision.saturating_add(1)
            }
            Patch::RecordEvidence(mut evidence) => {
                const MAX_EVIDENCE: usize = 128;
                evidence.workspace_revision = self.workspace_revision;
                self.evidence_ledger
                    .retain(|existing| existing.call_id != evidence.call_id);
                self.evidence_ledger.push(evidence);
                if self.evidence_ledger.len() > MAX_EVIDENCE {
                    let excess = self.evidence_ledger.len() - MAX_EVIDENCE;
                    self.evidence_ledger.drain(..excess);
                }
            }
            Patch::RecordToolResult(mut result) => {
                const MAX_TOOL_RESULTS: usize = 64;
                const MAX_ID_CHARS: usize = 128;
                const MAX_TOOL_CHARS: usize = 128;
                const MAX_SUMMARY_CHARS: usize = 512;
                const MAX_PATHS: usize = 32;
                result.call_id = result.call_id.chars().take(MAX_ID_CHARS).collect();
                result.tool = result.tool.chars().take(MAX_TOOL_CHARS).collect();
                result.summary = result.summary.chars().take(MAX_SUMMARY_CHARS).collect();
                result.changed_paths.truncate(MAX_PATHS);
                result.workspace_revision = self.workspace_revision;
                self.recent_tool_results
                    .retain(|existing| existing.call_id != result.call_id);
                self.recent_tool_results.push(result.clone());
                if self.recent_tool_results.len() > MAX_TOOL_RESULTS {
                    let excess = self.recent_tool_results.len() - MAX_TOOL_RESULTS;
                    self.recent_tool_results.drain(..excess);
                }
                self.last_tool_result = Some(result);
            }
            Patch::RecordModified(p) => {
                self.modified_files.insert(p);
            }
            Patch::RecordRead(path) => {
                self.last_read_paths.retain(|existing| existing != &path);
                self.last_read_paths.push(path);
                const MAX_READ_PATHS: usize = 8;
                if self.last_read_paths.len() > MAX_READ_PATHS {
                    let drop = self.last_read_paths.len() - MAX_READ_PATHS;
                    self.last_read_paths.drain(..drop);
                }
            }
            Patch::AddLiveShellJob(id) => {
                if !self.live_shell_jobs.iter().any(|existing| existing == &id) {
                    self.live_shell_jobs.push(id);
                }
            }
            Patch::RemoveLiveShellJob(id) => {
                self.live_shell_jobs.retain(|existing| existing != &id);
            }
            Patch::SetLastToolEffect(effect) => self.last_tool_effect = effect,
            Patch::SetLastError(e) => self.last_error = e,
            Patch::BumpStep => self.steps += 1,
            Patch::Batch(v) => v.into_iter().for_each(|p| self.apply(p)),
        }
    }
}

/// 连续无进展多少轮就熔断(no-progress detection)。
pub const MAX_STALL: usize = 3;

/// 连续工具/provider 报错多少轮就熔断(circuit breaker,防无人值守 `--every` 循环持续失败烧预算)。
pub const MAX_ERR_STREAK: usize = 5;

/// Repeating policy-rejected actions cannot make progress. Keep this small so
/// a model gets a chance to choose a permitted alternative without burning a
/// long-task budget when it keeps changing superficial arguments.
pub const MAX_POLICY_BLOCKED_STREAK: usize = 3;

/// 连续无新增证据的纯侦察多少轮就软暂停(explore thrash)。低于此数仅在 durable 事实块里轻 nudge;
/// 达此数 → `must_stop`/`no_progress`,逼模型先交接已定位的问题再开新轮,而非空烧到 `MAX_STEPS`。
pub const MAX_EXPLORE: usize = 12;

/// 连续纯侦察达此数起,在 durable 事实块注入「定位后立即动手」提醒(仍不硬停)。
pub const EXPLORE_NUDGE_AFTER: usize = 5;

/// 权限门:执行**有副作用的**工具(shell / 写文件 / MCP)前征询批准(human-in-the-loop)。
/// REPL 用 stdin y/n;测试用 [`AutoApprove`] / [`AutoDeny`]。`read_file` 等只读工具不走它。
pub trait Approver: Send + Sync {
    fn approve(&self, action: &str, detail: &str) -> bool;
}

/// 一律放行(默认;非交互 / 一次性任务用)。
pub struct AutoApprove;
impl Approver for AutoApprove {
    fn approve(&self, _action: &str, _detail: &str) -> bool {
        true
    }
}

/// 一律拒绝(测试用)。
pub struct AutoDeny;
impl Approver for AutoDeny {
    fn approve(&self, _action: &str, _detail: &str) -> bool {
        false
    }
}

/// 只读工具不需要批准(read_file / search 只读本地;web_search / fetch_url 只读公共网页;
/// todo_write 只更新内部清单,无外部副作用)。
pub(crate) fn needs_approval(tool: &str) -> bool {
    !matches!(
        tool,
        "read_file"
            | "search"
            | "web_search"
            | "fetch_url"
            | "todo_write"
            | "contract_write"
            | "requirement_update"
            | "signal_write"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::*;

    #[test]
    fn legacy_dispatch_boolean_checkpoint_migrates_to_one_wave() {
        let mut value = serde_json::to_value(AgentState::new("resume")).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("dispatch_batches_used");
        object.insert("dispatch_agents_used".into(), serde_json::Value::Bool(true));

        let restored: AgentState = serde_json::from_value(value).unwrap();
        assert_eq!(restored.dispatch_wave_count(), 1);
    }

    #[test]
    fn dispatch_attempt_checkpoint_defaults_and_reducer_stays_bounded() {
        let mut value = serde_json::to_value(AgentState::new("resume")).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("dispatch_attempts_used");
        let restored: AgentState = serde_json::from_value(value).unwrap();
        assert_eq!(restored.dispatch_attempts_used, 0);

        let mut state = AgentState::new("run");
        state.apply(Patch::SetDispatchAttempts(usize::MAX));
        assert_eq!(state.dispatch_attempts_used, MAX_DISPATCH_ATTEMPTS);
    }

    /// 只读工具(read_file / search / web_search / fetch_url)不走权限门;有副作用的走。
    #[test]
    fn readonly_tools_skip_approval() {
        assert!(!needs_approval("read_file"));
        assert!(!needs_approval("search"));
        assert!(!needs_approval("web_search"));
        assert!(!needs_approval("fetch_url"));
        assert!(!needs_approval("contract_write"));
        assert!(!needs_approval("requirement_update"));
        assert!(needs_approval("edit_file"));
        assert!(needs_approval("write_file"));
        assert!(needs_approval("run_shell"));
    }

    #[test]
    fn pending_call_queue_preserves_order_and_legacy_head() {
        let first = ToolCall {
            id: "first".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "first"}),
        };
        let second = ToolCall {
            id: "second".into(),
            name: "run_shell".into(),
            arguments: serde_json::json!({"cmd": "exit 0"}),
        };
        let mut state = AgentState::new("run both");
        state.apply(Patch::PendingCalls(vec![first, second]));
        assert_eq!(
            state.next_pending_call().map(|call| call.id.as_str()),
            Some("first")
        );
        assert_eq!(
            state.pending_call.as_ref().map(|call| call.id.as_str()),
            Some("first")
        );

        state.apply(Patch::DequeuePendingCall);
        assert_eq!(
            state.next_pending_call().map(|call| call.id.as_str()),
            Some("second")
        );
        assert_eq!(
            state.pending_call.as_ref().map(|call| call.id.as_str()),
            Some("second")
        );

        state.apply(Patch::DequeuePendingCall);
        assert!(!state.has_pending_calls());
        assert!(state.pending_call.is_none());
    }

    #[test]
    fn contract_requires_real_current_revision_evidence() {
        let contract = TaskContract {
            objective: "change and verify".into(),
            requirements: vec![Requirement {
                id: "R1".into(),
                description: "target test passes".into(),
                status: RequirementStatus::Unknown,
                evidence_call_ids: Vec::new(),
            }],
            constraints: Vec::new(),
            deliverables: Vec::new(),
            non_goals: Vec::new(),
        };
        let mut state = AgentState::new("change and verify");
        state.apply(Patch::SetTaskContract(contract));
        assert!(state.contract_completion_blocked());

        state.apply(Patch::RecordEvidence(EvidenceRef {
            call_id: "test-1".into(),
            tool: "run_shell".into(),
            succeeded: true,
            workspace_revision: 999,
            summary: "exit 0".into(),
        }));
        let update = RequirementUpdate {
            id: "R1".into(),
            status: RequirementStatus::Satisfied,
            evidence_call_ids: vec!["test-1".into()],
        };
        assert!(state
            .validate_requirement_updates(std::slice::from_ref(&update))
            .is_ok());
        state.apply(Patch::UpdateRequirements(vec![update]));
        assert!(!state.contract_completion_blocked());

        state.apply(Patch::AdvanceWorkspaceRevision);
        assert!(
            state.contract_completion_blocked(),
            "a later edit must invalidate old completion evidence"
        );
        assert!(state
            .validate_requirement_updates(&[RequirementUpdate {
                id: "R1".into(),
                status: RequirementStatus::Satisfied,
                evidence_call_ids: vec!["missing".into()],
            }])
            .is_err());
    }

    #[test]
    fn typed_tool_results_are_revision_stamped_deduplicated_and_bounded() {
        let mut state = AgentState::new("record results");
        state.apply(Patch::AdvanceWorkspaceRevision);
        let base = ToolResultV1 {
            schema_version: 1,
            call_id: "same".into(),
            tool: "write_file".into(),
            effect: ToolEffect::Edit,
            status: ToolResultStatus::Success,
            exit_code: Some(0),
            retryable: false,
            changed_paths: (0..40).map(|i| format!("src/{i}.rs")).collect(),
            output_truncated: false,
            read_offset: None,
            read_limit: None,
            match_count: None,
            workspace_revision: 0,
            summary: "x".repeat(600),
        };
        state.apply(Patch::RecordToolResult(base));
        let first = state.last_tool_result.as_ref().expect("recorded result");
        assert_eq!(first.workspace_revision, 1);
        assert_eq!(first.changed_paths.len(), 32);
        assert_eq!(first.summary.chars().count(), 512);

        for i in 0..70 {
            state.apply(Patch::RecordToolResult(ToolResultV1 {
                call_id: format!("call-{i}"),
                tool: "read_file".into(),
                effect: ToolEffect::Explore,
                status: ToolResultStatus::Success,
                exit_code: None,
                retryable: false,
                changed_paths: Vec::new(),
                output_truncated: false,
                read_offset: None,
                read_limit: None,
                match_count: None,
                workspace_revision: 0,
                summary: "ok".into(),
                schema_version: 1,
            }));
        }
        assert_eq!(state.recent_tool_results.len(), 64);
        assert_eq!(
            state
                .last_tool_result
                .as_ref()
                .map(|result| result.call_id.as_str()),
            Some("call-69")
        );

        state.apply(Patch::RecordToolResult(ToolResultV1 {
            call_id: "call-69".into(),
            tool: "search".into(),
            effect: ToolEffect::Explore,
            status: ToolResultStatus::Blocked,
            exit_code: None,
            retryable: false,
            changed_paths: Vec::new(),
            output_truncated: false,
            read_offset: None,
            read_limit: None,
            match_count: None,
            workspace_revision: 0,
            summary: "blocked".into(),
            schema_version: 1,
        }));
        assert_eq!(state.recent_tool_results.len(), 64);
        assert_eq!(
            state.last_tool_result.as_ref().map(|result| result.status),
            Some(ToolResultStatus::Blocked)
        );
    }

    #[test]
    fn machine_reasoning_limit_is_clamped_and_checkpoint_safe() {
        let state = AgentState::new("bounded").with_reasoning_limit(1);
        assert_eq!(state.reasoning_limit(), 1);
        let mut legacy = serde_json::to_value(AgentState::new("legacy")).unwrap();
        legacy
            .as_object_mut()
            .unwrap()
            .remove("reasoning_step_limit");
        assert_eq!(
            serde_json::from_value::<AgentState>(legacy)
                .unwrap()
                .reasoning_limit(),
            MAX_STEPS,
            "old checkpoints retain the long-task default"
        );
    }
}
