use langgraph::{GraphState, RunConfig};
use provider::{Message, ToolCall, Usage};
use std::collections::BTreeSet;

/// 回合上限 —— **防跑飞的后备护栏**,非正常终止手段。真正的停机主力是:`approved`(目标达成)、
/// 无进展检测(`stalled`,连 3 轮同输出即停)、连错熔断(`circuit_broken`,连 5 轮报错即停)。
/// 抬到 2000:让**真实长任务能跑完**,而非被腰斩(用户诉求)。命中此上限**不是硬杀**——经 `wrapup`
/// 软中止,让模型总结进度 + 规划后续供用户参考(见 `verify_route_llm`)。预算护栏默认关(`budget_tokens=0`),
/// 卡死由 stall/circuit 早停兜底,故 2000 只有**持续有进展**的长任务才会触达。
/// 注意:上限一抬,引擎超步上限须随之派生(见 [`agent_run_config`]),否则先撞引擎默认 100 超步的 `StepLimit`。
pub const MAX_STEPS: usize = 2000;

/// 本 agent 的运行参数:引擎超步上限据 [`MAX_STEPS`] **派生**(每 step ≈ 2 超步 reason+act,
/// 加收尾余量 verify+wrapup)。使「跑多久」真正由 MAX_STEPS 决定,不被引擎默认 100 超步提前腰斩。
pub fn agent_run_config() -> RunConfig {
    RunConfig {
        max_supersteps: MAX_STEPS * 2 + 50,
    }
}

/// 一条任务清单项(像 Claude Code 的 TodoWrite):`status` ∈ `pending` / `in_progress` / `completed`。
#[derive(Clone, Debug, PartialEq)]
pub struct Todo {
    pub content: String,
    pub status: String,
}

/// agent 的共享状态。`messages` 是事件轨迹(reducer 追加),其余字段覆盖。
#[derive(Clone, Debug, Default)]
pub struct AgentState {
    pub task: String,
    pub messages: Vec<String>,
    pub last_action: Option<String>,
    pub tool_output: Option<String>,
    pub approved: bool,
    pub steps: usize,
    pub issues: Vec<String>,
    /// 由 reason 节点(真实 LLM 路径)产出、待 act 节点执行的结构化工具调用。
    pub pending_call: Option<ToolCall>,
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
    /// 连续**纯侦察**轮数(read_file/search/web_search/fetch_url/dispatch_agent,输出每轮不同故 stall 不触发)。
    /// 成功写改(`write_file`/`edit_file`/`apply_edits`)或模型收尾时清零。到 [`MAX_EXPLORE`] 软暂停,
    /// 防「无休止只查不改 → 撞 step_cap → 再开一轮又从侦察重来」。
    pub explore_streak: usize,
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
}

/// 节点产出的增量更新(delta)。`Batch` 让一个节点一次改多个字段。
#[derive(Debug)]
pub enum Patch {
    Message(String),
    Action(Option<String>),
    ToolOutput(Option<String>),
    Approved(bool),
    Issues(Vec<String>),
    PendingCall(Option<ToolCall>),
    AddTokens(usize),
    AddUsage(Usage),
    SetStall(usize),
    SetErrStreak(usize),
    SetExploreStreak(usize),
    PushHistory(Message),
    SetTodos(Vec<Todo>),
    RecordModified(String),
    SetLastError(Option<String>),
    BumpStep,
    Batch(Vec<Patch>),
}

impl GraphState for AgentState {
    type Update = Patch;
    fn apply(&mut self, u: Patch) {
        match u {
            Patch::Message(m) => self.messages.push(m), // append reducer
            Patch::Action(a) => self.last_action = a,
            Patch::ToolOutput(o) => self.tool_output = o,
            Patch::Approved(b) => self.approved = b,
            Patch::Issues(v) => self.issues = v,
            Patch::PendingCall(c) => self.pending_call = c,
            Patch::AddTokens(n) => self.total_tokens += n,
            Patch::AddUsage(usage) => {
                self.input_tokens += usage.prompt_tokens as usize;
                self.output_tokens += usage.completion_tokens as usize;
                self.total_tokens += usage.total() as usize;
            }
            Patch::SetStall(n) => self.stall = n,
            Patch::SetErrStreak(n) => self.err_streak = n,
            Patch::SetExploreStreak(n) => self.explore_streak = n,
            Patch::PushHistory(m) => self.history.push(m),
            Patch::SetTodos(t) => self.todos = t,
            Patch::RecordModified(p) => {
                self.modified_files.insert(p);
            }
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

/// 连续纯侦察多少轮就软暂停(explore thrash)。低于此数仅在 durable 事实块里轻 nudge;
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
        "read_file" | "search" | "web_search" | "fetch_url" | "todo_write" | "signal_write"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::*;

    /// 只读工具(read_file / search / web_search / fetch_url)不走权限门;有副作用的走。
    #[test]
    fn readonly_tools_skip_approval() {
        assert!(!needs_approval("read_file"));
        assert!(!needs_approval("search"));
        assert!(!needs_approval("web_search"));
        assert!(!needs_approval("fetch_url"));
        assert!(needs_approval("edit_file"));
        assert!(needs_approval("write_file"));
        assert!(needs_approval("run_shell"));
    }
}
