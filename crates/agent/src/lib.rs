//! # agent —— 跑在 [`langgraph`] 引擎上的最小编码 agent
//!
//! 把「loop engineering」的核心结构落成一张图:
//!
//! ```text
//!   START ─▶ reason ──(action=finish 或 到达回合上限)──▶ verify ──(approved / 到顶)──▶ END
//!             ▲   │                                          │
//!             │   └────────(其它 action)────▶ act ──────────┘(未过 → 回 reason)
//!             └──────────────────(reflection loop)───────────
//! ```
//!
//! 三个原则(见 `docs/REPORT-langgraph-rust.md`):
//! - **maker ≠ checker**:`reason`/`act` 生成,`verify` 独立判定,不让生成者给自己打分。
//! - **确定性验证**:`verify` 只认工具输出里的客观信号(测试是否 passed),不认模型自述。
//! - **停机是设计的一半**:硬回合上限 `MAX_STEPS` + approved 闸门,双保险防跑飞。
//!
//! `Brain` 是接真实 LLM 的接缝;这里给一个离线 `ScriptedBrain`,零联网即可跑通闭环。

use std::convert::Infallible;
use std::sync::Arc;

use langgraph::{CompiledGraph, GraphError, GraphState, StateGraph, END};
use provider::{CompletionRequest, LlmProvider, Message, Role, ToolCall, ToolSpec};

/// 到达此回合数强制收尾 —— 成本 / 防死循环护栏。
pub const MAX_STEPS: usize = 8;

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
    /// token 预算(0 = 不限)。超了就熔断停机。
    pub budget_tokens: usize,
    /// 连续「无进展」轮数(工具输出与上一轮相同)。到 [`MAX_STALL`] 就熔断。
    pub stall: usize,
}

impl AgentState {
    pub fn new(task: impl Into<String>) -> Self {
        Self {
            task: task.into(),
            ..Default::default()
        }
    }

    /// 设 token 预算(loop engineering 的经济护栏之一)。
    pub fn with_budget(mut self, tokens: usize) -> Self {
        self.budget_tokens = tokens;
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
    SetStall(usize),
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
            Patch::SetStall(n) => self.stall = n,
            Patch::BumpStep => self.steps += 1,
            Patch::Batch(v) => v.into_iter().for_each(|p| self.apply(p)),
        }
    }
}

/// 连续无进展多少轮就熔断(no-progress detection)。
pub const MAX_STALL: usize = 3;

/// 超预算?(0 预算 = 不限)
fn over_budget(s: &AgentState) -> bool {
    s.budget_tokens > 0 && s.total_tokens >= s.budget_tokens
}

/// 陷入僵局?(连续 MAX_STALL 轮工具输出没变)
fn stalled(s: &AgentState) -> bool {
    s.stall >= MAX_STALL
}

/// 验证器的确定性判据:工具输出里出现客观成功信号(shell `exit 0` 或测试 `passed`)。
fn tool_output_ok(o: &str) -> bool {
    o.contains("exit 0") || (o.contains("passed") && !o.contains("failed"))
}

/// 多层独立退出:到回合上限 / 超预算 / 无进展任一命中,循环都该停(loop engineering:停机是设计的一半)。
fn must_stop(s: &AgentState) -> bool {
    s.steps >= MAX_STEPS || over_budget(s) || stalled(s)
}

/// reason 之后的路由(scripted / llm 两条路径共用):finish 或需停机 → verify,否则 → act。
fn reason_route(s: &AgentState) -> Vec<String> {
    if must_stop(s) {
        return vec!["verify".to_string()];
    }
    match s.last_action.as_deref() {
        Some("finish") => vec!["verify".to_string()],
        _ => vec!["act".to_string()],
    }
}

/// verify 之后的路由(共用):通过或需停机 → END,否则回 reason。
fn verify_route(s: &AgentState) -> Vec<String> {
    if s.approved || must_stop(s) {
        vec![END.to_string()]
    } else {
        vec!["reason".to_string()]
    }
}

/// 决策大脑(maker 的一半):看状态,给出下一步动作名或 `"finish"`。
/// 这是接真实 LLM provider 的接缝 —— 换实现即可,图不用动。
pub trait Brain: Send + Sync + 'static {
    fn decide(&self, state: &AgentState) -> String;
}

/// 离线脚本大脑:按工具反馈推进(写代码 → 修复 → 完成),用于零联网跑通闭环 / 测试。
pub struct ScriptedBrain;

impl Brain for ScriptedBrain {
    fn decide(&self, s: &AgentState) -> String {
        match s.tool_output.as_deref() {
            None => "write_code".to_string(),
            Some(o) if o.contains("failed") => "fix".to_string(),
            Some(o) if o.contains("passed") => "finish".to_string(),
            _ => "finish".to_string(),
        }
    }
}

/// 工具执行器(act 节点用)。真实场景后面换成 MCP / shell / 编译器。
pub type Tool = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// 便捷构造:脚本大脑。
pub fn scripted() -> Arc<dyn Brain> {
    Arc::new(ScriptedBrain)
}

/// 便捷构造:默认离线工具 —— 模拟「写代码先挂、修复后通过」的客观验证信号。
pub fn default_tool() -> Tool {
    Arc::new(|action: &str| match action {
        "write_code" => "tests: 1 failed".to_string(),
        "fix" => "tests: passed".to_string(),
        other => format!("unknown tool `{other}`"),
    })
}

/// 便捷构造:**真实** shell 工具(M1 物理闭环)—— 把 action 当命令跑,返回退出码 + 输出。
/// 这是 act 节点触碰真实世界的接缝。⚠ 无沙箱,只喂受控命令。
pub fn shell_tool() -> Tool {
    Arc::new(|action: &str| match tools::run_shell(action) {
        Ok(r) => format!("exit {}: {}{}", r.code, r.stdout.trim(), r.stderr.trim()),
        Err(e) => format!("shell error: {e}"),
    })
}

/// 把 agent 装配成一张编译好的 langgraph 图。
pub fn build_agent(
    brain: Arc<dyn Brain>,
    tool: Tool,
) -> Result<CompiledGraph<AgentState>, GraphError> {
    let mut g = StateGraph::<AgentState>::new();

    // reason:推进一个回合,问大脑要下一步动作。
    let brain_c = brain.clone();
    g.add_node("reason", move |s: AgentState| {
        let brain = brain_c.clone();
        async move {
            let action = brain.decide(&s);
            let msg = format!("reason#{}: -> {action}", s.steps + 1);
            Ok::<_, Infallible>(Patch::Batch(vec![
                Patch::BumpStep,
                Patch::Message(msg),
                Patch::Action(Some(action)),
            ]))
        }
    });

    // act:执行工具,把客观输出写回状态。
    let tool_c = tool.clone();
    g.add_node("act", move |s: AgentState| {
        let tool = tool_c.clone();
        async move {
            let action = s.last_action.clone().unwrap_or_default();
            let out = (tool.as_ref())(&action);
            Ok::<_, Infallible>(Patch::Batch(vec![
                Patch::Message(format!("act: {action} -> {out}")),
                Patch::ToolOutput(Some(out)),
            ]))
        }
    });

    g.add_node("verify", verify_node);

    g.set_entry("reason");
    g.add_conditional_edge("reason", reason_route);
    g.add_edge("act", "reason"); // 反思环:工具跑完回 reason 复盘
    g.add_conditional_edge("verify", verify_route);

    g.compile()
}

/// verify 节点(scripted / llm 两条路径共用):独立 checker,只认 [`tool_output_ok`] 的确定性信号。
async fn verify_node(s: AgentState) -> Result<Patch, Infallible> {
    let ok = s.tool_output.as_deref().is_some_and(tool_output_ok);
    let patch = if ok {
        Patch::Batch(vec![
            Patch::Approved(true),
            Patch::Message("verify: PASS (deterministic gate)".to_string()),
        ])
    } else {
        Patch::Batch(vec![
            Patch::Approved(false),
            Patch::Issues(vec!["build/tests not passing".to_string()]),
            Patch::Message("verify: FAIL -> back to reason".to_string()),
        ])
    };
    Ok(patch)
}

/// 内置工具的规格(喂给 LLM 让它按 schema 出结构化 tool_call)。
pub fn builtin_tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "run_shell".to_string(),
            description: "运行一条 shell 命令,返回退出码与输出".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"cmd":{"type":"string"}},"required":["cmd"]}),
        },
        ToolSpec {
            name: "write_file".to_string(),
            description: "把内容整文件写入路径(覆盖)".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"},"contents":{"type":"string"}},"required":["path","contents"]}),
        },
        ToolSpec {
            name: "read_file".to_string(),
            description: "读取一个文件的全文".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}),
        },
    ]
}

/// 执行一个结构化工具调用,返回给模型看的观察结果(observation)。用真实的 `tools` crate 干活。
pub fn execute_tool_call(call: &ToolCall) -> String {
    let arg = |k: &str| call.arguments.get(k).and_then(|v| v.as_str()).unwrap_or("");
    match call.name.as_str() {
        "run_shell" => match tools::run_shell(arg("cmd")) {
            Ok(r) => format!("exit {}: {}{}", r.code, r.stdout.trim(), r.stderr.trim()),
            Err(e) => format!("shell error: {e}"),
        },
        "write_file" => {
            let contents = arg("contents");
            match tools::write_file(arg("path"), contents) {
                Ok(()) => format!("wrote {} bytes to {}", contents.len(), arg("path")),
                Err(e) => format!("write error: {e}"),
            }
        }
        "read_file" => match tools::read_file(arg("path")) {
            Ok(c) => c,
            Err(e) => format!("read error: {e}"),
        },
        other => format!("unknown tool `{other}`"),
    }
}

/// 把当前状态铺成给 provider 的消息序列(system + task + 轨迹)。
fn to_messages(s: &AgentState) -> Vec<Message> {
    let mut msgs = vec![
        Message::new(
            Role::System,
            "You are a coding agent. Use the provided tools to make the build/tests pass, then stop.",
        ),
        Message::new(Role::User, s.task.clone()),
    ];
    msgs.extend(
        s.messages
            .iter()
            .map(|m| Message::new(Role::Assistant, m.clone())),
    );
    msgs
}

/// 用**真实 LLM provider** 装配 agent 图:reason 节点问 provider 要结构化 tool_call,
/// act 节点用 [`execute_tool_call`] 调真实工具,verify 认确定性信号。这是通往「媲美 Claude Code」的主路径。
pub fn build_llm_agent(
    provider: Arc<dyn LlmProvider>,
) -> Result<CompiledGraph<AgentState>, GraphError> {
    let mut g = StateGraph::<AgentState>::new();
    let specs = builtin_tool_specs();

    let provider_c = provider.clone();
    g.add_node("reason", move |s: AgentState| {
        let provider = provider_c.clone();
        let tools = specs.clone();
        async move {
            let req = CompletionRequest {
                messages: to_messages(&s),
                tools,
            };
            let completion = provider.complete(&req).await?;
            let tokens = completion.usage.total() as usize; // 成本记账
            let patch = if let Some(call) = completion.tool_calls.into_iter().next() {
                // maker 想用工具 → 交给 act 执行。
                Patch::Batch(vec![
                    Patch::BumpStep,
                    Patch::AddTokens(tokens),
                    Patch::Message(format!(
                        "reason#{}: tool_call {} {}",
                        s.steps + 1,
                        call.name,
                        call.arguments
                    )),
                    Patch::PendingCall(Some(call)),
                    Patch::Action(Some("tool".to_string())),
                ])
            } else {
                // 模型给了最终文本,没有工具调用 → 收尾。
                Patch::Batch(vec![
                    Patch::BumpStep,
                    Patch::AddTokens(tokens),
                    Patch::Message(format!(
                        "reason#{}: (final) {}",
                        s.steps + 1,
                        completion.text
                    )),
                    Patch::PendingCall(None),
                    Patch::Action(Some("finish".to_string())),
                ])
            };
            Ok::<_, provider::ProviderError>(patch)
        }
    });

    g.add_node("act", |s: AgentState| async move {
        let patch = match &s.pending_call {
            Some(call) => {
                let obs = execute_tool_call(call);
                // 无进展检测:工具输出与上一轮相同则 stall+1,否则清零。
                let stall = if s.tool_output.as_deref() == Some(obs.as_str()) {
                    s.stall + 1
                } else {
                    0
                };
                Patch::Batch(vec![
                    Patch::Message(format!("act: {} -> {}", call.name, obs)),
                    Patch::SetStall(stall),
                    Patch::ToolOutput(Some(obs)),
                    Patch::PendingCall(None),
                ])
            }
            None => Patch::Message("act: no pending tool_call".to_string()),
        };
        Ok::<_, Infallible>(patch)
    });

    g.add_node("verify", verify_node);

    g.set_entry("reason");
    g.add_conditional_edge("reason", reason_route);
    g.add_edge("act", "reason");
    g.add_conditional_edge("verify", verify_route);

    g.compile()
}

#[cfg(test)]
mod tests {
    use super::*;
    use langgraph::RunConfig;

    #[tokio::test]
    async fn happy_path_converges_and_gets_approved() {
        let app = build_agent(scripted(), default_tool()).unwrap();
        let out = app
            .invoke(AgentState::new("make tests pass"))
            .await
            .unwrap();
        assert!(out.approved, "checker should approve once tests pass");
        assert_eq!(out.steps, 3, "write_code -> fix -> finish");
        assert!(out.messages.iter().any(|m| m.contains("verify: PASS")));
    }

    /// 大脑永不收工 + 工具永远失败:循环必须在回合上限处停机,而不是烧到天荒地老。
    #[tokio::test]
    async fn broken_loop_terminates_at_cap() {
        struct NeverDone;
        impl Brain for NeverDone {
            fn decide(&self, _s: &AgentState) -> String {
                "retry".to_string()
            }
        }
        let tool: Tool = Arc::new(|_a: &str| "tests: 1 failed".to_string());
        let app = build_agent(Arc::new(NeverDone), tool).unwrap();

        let out = app
            .invoke_with(
                AgentState::new("impossible"),
                &RunConfig::default(),
                None,
                None,
            )
            .await
            .unwrap();

        assert!(!out.approved, "must not fake success");
        assert_eq!(out.steps, MAX_STEPS, "hard cap stops the runaway loop");
    }

    /// P0 物理闭环:shell 工具把真实退出码带回来(0 vs 非 0),不再是脚本假信号。
    #[test]
    fn shell_tool_reflects_real_exit_code() {
        let tool = shell_tool();
        assert!((tool.as_ref())("exit 0").starts_with("exit 0:"));
        assert!((tool.as_ref())("exit 7").starts_with("exit 7:"));
    }

    /// P1:结构化 tool_call → 真实文件写入(物理副作用可验证)。
    #[test]
    fn execute_tool_call_writes_real_file() {
        let mut path = std::env::temp_dir();
        path.push("ridge_llm_toolcall.txt");
        let call = ToolCall {
            id: "x".to_string(),
            name: "write_file".to_string(),
            arguments: serde_json::json!({"path": path.to_str().unwrap(), "contents": "physical closure"}),
        };
        let obs = execute_tool_call(&call);
        assert!(obs.contains("wrote"), "{obs}");
        assert_eq!(tools::read_file(&path).unwrap(), "physical closure");
        let _ = std::fs::remove_file(&path);
    }

    /// P1 端到端:provider 吐结构化 tool_call → act 调**真实** shell → verify 认真实 `exit 0` → approved。
    /// 用离线 ScriptedProvider 站位真实 LLM,零联网、确定性。
    #[tokio::test]
    async fn llm_agent_drives_real_tools_to_approved() {
        use provider::{Completion, ScriptedProvider};
        let scripted = ScriptedProvider::new(vec![
            // 第 1 轮:决定跑构建(真实 shell,exit 0 代表构建通过)。
            Completion {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "1".to_string(),
                    name: "run_shell".to_string(),
                    arguments: serde_json::json!({"cmd": "exit 0"}),
                }],
                ..Default::default()
            },
            // 第 2 轮:没有工具调用 → 收尾。
            Completion {
                text: "build is green, done".to_string(),
                tool_calls: vec![],
                ..Default::default()
            },
        ]);
        let app = build_llm_agent(Arc::new(scripted)).unwrap();
        let out = app
            .invoke(AgentState::new("make the build pass"))
            .await
            .unwrap();

        assert!(
            out.approved,
            "real exit 0 should satisfy the deterministic gate"
        );
        assert_eq!(out.steps, 2, "run_shell -> finish");
        assert!(out.messages.iter().any(|m| m.contains("run_shell")));
    }

    // 一个「永不收工、每轮都调同一个失败命令」的 provider 步骤,带可配的 token 用量。
    fn stuck_step(tokens: u32) -> provider::Completion {
        provider::Completion {
            tool_calls: vec![ToolCall {
                id: "1".to_string(),
                name: "run_shell".to_string(),
                arguments: serde_json::json!({"cmd": "exit 1"}),
            }],
            usage: provider::Usage {
                prompt_tokens: tokens,
                completion_tokens: 0,
            },
            ..Default::default()
        }
    }

    /// 成本护栏:每轮烧 token,预算耗尽即熔断,不跑到回合上限。
    #[tokio::test]
    async fn budget_breaker_stops_before_cap() {
        use provider::ScriptedProvider;
        let provider = ScriptedProvider::new((0..8).map(|_| stuck_step(100)).collect::<Vec<_>>());
        let app = build_llm_agent(Arc::new(provider)).unwrap();
        let out = app
            .invoke(AgentState::new("loop").with_budget(250))
            .await
            .unwrap();

        assert!(!out.approved, "must not fake success");
        assert!(out.total_tokens >= 250, "hit budget: {}", out.total_tokens);
        assert!(
            out.steps < MAX_STEPS,
            "budget熔断应早于回合上限: steps={}",
            out.steps
        );
    }

    /// 无进展检测:工具输出连续 MAX_STALL 轮不变即熔断,不跑到回合上限。
    #[tokio::test]
    async fn no_progress_detection_stops_before_cap() {
        use provider::ScriptedProvider;
        let provider = ScriptedProvider::new((0..8).map(|_| stuck_step(0)).collect::<Vec<_>>());
        let app = build_llm_agent(Arc::new(provider)).unwrap();
        let out = app.invoke(AgentState::new("stuck")).await.unwrap();

        assert!(!out.approved);
        assert!(out.stall >= MAX_STALL, "stall={}", out.stall);
        assert!(
            out.steps < MAX_STEPS,
            "no-progress熔断应早于回合上限: steps={}",
            out.steps
        );
    }
}
