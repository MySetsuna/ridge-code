use crate::brain::*;
use crate::context::*;
use crate::exec::*;
use crate::guard::*;
use crate::knowledge::*;
use crate::mcp_tools::*;
use crate::observe::*;
use crate::state::*;
use langgraph::{CompiledGraph, GraphError, StateGraph};
use provider::{CompletionRequest, LlmProvider, Message, Role, StreamChunk};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

// halt_reason 已移至 orchestrate;此处再导出,保持 `crate::graph::halt_reason` 路径不变
//(signals.rs 经 `use crate::graph::*` 依赖它,不在本次改动范围内)。
pub(crate) use crate::orchestrate::halt_reason;

/// 流式 token 总线:REPL 每回合把一个 sender 塞进来,reason 节点边收 provider 的增量
/// 边往里发,REPL 侧就能**逐字显示**(像 Claude Code)。载 [`StreamChunk`] 以**分道**回答/思考
/// (回答恒显、思考灰显)。`None` = 该回合不流式。
pub type TokenBus = Arc<std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<StreamChunk>>>>;

/// 一个「永不流式」的空总线(测试 / 非交互装配用)。
pub fn null_token_bus() -> TokenBus {
    Arc::new(std::sync::Mutex::new(None))
}

const DEFAULT_TOOL_TIMEOUT_SECS: u64 = 180;

fn graph_trace(stage: &str) {
    let Some(path) = std::env::var_os("RIDGE_TUI_TRACE") else {
        return;
    };
    if let Some(parent) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        let _ = writeln!(file, "graph.{stage}");
    }
}

fn mcp_tool_timeout() -> Duration {
    std::env::var("RIDGE_TOOL_TIMEOUT")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_TOOL_TIMEOUT_SECS))
}

async fn call_mcp_with_timeout(
    client: &mcp::McpClient,
    tool: &str,
    arguments: serde_json::Value,
    timeout: Duration,
) -> String {
    match tokio::time::timeout(timeout, client.call_tool(tool, arguments)).await {
        Ok(Ok(text)) => text,
        Ok(Err(error)) => format!("mcp error: {error}"),
        Err(_) => format!("mcp error: timed out after {}ms", timeout.as_millis()),
    }
}

/// 用**真实 LLM provider** 装配 agent 图(不接 MCP)。见 [`build_llm_agent_with`]。
pub fn build_llm_agent(
    provider: Arc<dyn LlmProvider>,
) -> Result<CompiledGraph<AgentState>, GraphError> {
    build_llm_agent_with(provider, McpTools::empty())
}

/// 装配 agent 图,并把 MCP 工具并入(确定性 verify,无独立模型复核,一律放行)。
/// Build an agent graph that cannot expose side-effecting tools.
pub fn build_llm_agent_read_only(
    provider: Arc<dyn LlmProvider>,
) -> Result<CompiledGraph<AgentState>, GraphError> {
    build_core(
        provider,
        McpTools::empty(),
        None,
        Arc::new(AutoApprove),
        Vec::new(),
        null_token_bus(),
        Arc::new(Agents::default()),
        true,
    )
}

pub fn build_llm_agent_with(
    provider: Arc<dyn LlmProvider>,
    mcp: McpTools,
) -> Result<CompiledGraph<AgentState>, GraphError> {
    build_core(
        provider,
        mcp,
        None,
        Arc::new(AutoApprove),
        Vec::new(),
        null_token_bus(),
        Arc::new(Agents::default()),
        false,
    )
}

/// 带**独立模型 checker** 的装配(M4,maker≠checker 的强形式):
/// 确定性 verify 通过后,再让一个**独立的** reviewer 模型复核有没有作弊(如删/跳过测试);
/// reviewer 打回则 approved=false、带 issue 回 reason。用**不同的** provider,别让写代码的模型自审。
pub fn build_llm_agent_reviewed(
    provider: Arc<dyn LlmProvider>,
    mcp: McpTools,
    reviewer: Arc<dyn LlmProvider>,
) -> Result<CompiledGraph<AgentState>, GraphError> {
    build_core(
        provider,
        mcp,
        Some(reviewer),
        Arc::new(AutoApprove),
        Vec::new(),
        null_token_bus(),
        Arc::new(Agents::default()),
        false,
    )
}

/// 带**权限门**的装配:有副作用的工具执行前过 [`Approver`](REPL 用它做 y/n 确认)。
pub fn build_llm_agent_gated(
    provider: Arc<dyn LlmProvider>,
    mcp: McpTools,
    approver: Arc<dyn Approver>,
) -> Result<CompiledGraph<AgentState>, GraphError> {
    build_core(
        provider,
        mcp,
        None,
        approver,
        Vec::new(),
        null_token_bus(),
        Arc::new(Agents::default()),
        false,
    )
}

/// **全装配**(模块化框架):MCP 工具 + 权限门 + 声明式 Skills + 流式 token 总线。CLI 用它。
/// `token_bus` 传 [`null_token_bus`] 即不流式;REPL 传真实总线以逐字显示。
#[allow(clippy::too_many_arguments)]
pub fn build_llm_agent_full(
    provider: Arc<dyn LlmProvider>,
    mcp: McpTools,
    approver: Arc<dyn Approver>,
    skills: Vec<Skill>,
    token_bus: TokenBus,
    agents: Arc<Agents>,
    read_only: bool,
) -> Result<CompiledGraph<AgentState>, GraphError> {
    build_core(
        provider, mcp, None, approver, skills, token_bus, agents, read_only,
    )
}

/// reason 把 内置 + MCP 工具一起 offer 给 LLM,act 按 `<server>__<tool>` 命名空间路由到对应
/// MCP 客户端(否则走内置工具),执行前过权限门,verify 认确定性信号(可选再挂独立模型 reviewer);
/// system prompt 注入 Skills(领域知识)。
#[allow(clippy::too_many_arguments)]
fn build_core(
    provider: Arc<dyn LlmProvider>,
    mcp: McpTools,
    reviewer: Option<Arc<dyn LlmProvider>>,
    approver: Arc<dyn Approver>,
    skills: Vec<Skill>,
    token_bus: TokenBus,
    agents: Arc<Agents>,
    read_only: bool,
) -> Result<CompiledGraph<AgentState>, GraphError> {
    graph_trace("build.begin");
    let mut g = StateGraph::<AgentState>::new();
    // 只读模式:不 offer 副作用工具、也不 offer MCP(副作用未知)—— 从源头断写。
    let mut specs = builtin_tool_specs();
    if read_only {
        specs.retain(|s| !is_mutating_tool(&s.name));
    }
    if let Some(d) = dispatch_spec(&agents) {
        specs.push(d); // dispatch_agent 安全(子 agent 恒只读),只读模式也可派
    }
    if !read_only {
        specs.extend(mcp.specs);
    }
    let router = Arc::new(mcp.router);
    graph_trace("specs.ready");
    let system = Arc::new(build_system_prompt_with_mode(&skills, read_only));
    graph_trace("system.ready");

    let provider_c = provider.clone();
    let system_c = system.clone();
    g.add_node("reason", move |s: AgentState| {
        let provider = provider_c.clone();
        let tools = specs.clone();
        let system = system_c.clone();
        let bus = token_bus.clone();
        async move {
            let req = CompletionRequest {
                messages: to_messages(&system, &s),
                tools,
            };
            // 流式:provider 每吐一段(回答/思考)就发进总线,REPL 侧分道逐字显示。无 sender 则等同整段。
            let on_token = move |chunk: StreamChunk| {
                if let Some(tx) = bus.lock().unwrap().as_ref() {
                    let _ = tx.send(chunk);
                }
            };
            tracing::debug!(step = s.steps + 1, msgs = req.messages.len(), "llm request");
            let completion = provider.complete_streaming(&req, &on_token).await?;
            let usage = completion.usage.clone();
            let tokens = usage.total() as usize; // 成本记账
            tracing::debug!(
                step = s.steps + 1,
                tokens,
                tool_calls = completion.tool_calls.len(),
                "llm response"
            );
            let asst_text = completion.text.clone();
            let patch = if let Some(call) = completion.tool_calls.into_iter().next() {
                // maker 想用工具 → 记 assistant(带 tool_calls)进 history,交给 act 执行。
                let hist = Message::assistant(asst_text).with_tool_calls(vec![call.clone()]);
                Patch::Batch(vec![
                    Patch::BumpStep,
                    Patch::AddUsage(usage.clone()),
                    Patch::Message(format!(
                        "reason#{}: tool_call {} {}",
                        s.steps + 1,
                        call.name,
                        call.arguments
                    )),
                    Patch::PushHistory(hist),
                    Patch::PendingCall(Some(call)),
                    Patch::Action(Some("tool".to_string())),
                ])
            } else {
                // 模型给了最终文本,没有工具调用 → 收尾。
                Patch::Batch(vec![
                    Patch::BumpStep,
                    Patch::AddUsage(usage),
                    Patch::Message(format!("reason#{}: (final) {}", s.steps + 1, asst_text)),
                    Patch::PushHistory(Message::assistant(asst_text)),
                    Patch::PendingCall(None),
                    Patch::Action(Some("finish".to_string())),
                ])
            };
            Ok::<_, provider::ProviderError>(patch)
        }
    });

    // web_search 依赖:真实抓取器 + 网络环境缓存(整会话懒探测一次)。
    let fetch: Arc<dyn provider::search::WebFetch> =
        Arc::new(provider::search::ReqwestFetch::new());
    let net: Arc<std::sync::OnceLock<provider::search::NetEnv>> =
        Arc::new(std::sync::OnceLock::new());

    let router_c = router.clone();
    let approver_c = approver.clone();
    let fetch_c = fetch.clone();
    let net_c = net.clone();
    let agents_c = agents.clone();
    let provider_act = provider.clone(); // 主 provider:sub-agent 未指定档案时的回落
    g.add_node("act", move |s: AgentState| {
        let router = router_c.clone();
        let approver = approver_c.clone();
        let fetch = fetch_c.clone();
        let net = net_c.clone();
        let agents = agents_c.clone();
        let main_provider = provider_act.clone();
        async move {
            let patch = match &s.pending_call {
                Some(call) => {
                    // 只读模式深度防御:副作用工具即使被幻觉调到也硬拒(与 offering 过滤双保险)。
                    let obs = if let Some(m) = read_only_block(read_only, &call.name) {
                        m
                    } else if needs_approval(&call.name)
                        && !approver.approve(&call.name, &preview_call(call))
                    {
                        format!("permission denied by user: {}", call.name)
                    } else if call.name == "dispatch_agent" {
                        dispatch_obs(&agents, &main_provider, call).await
                    } else if call.name == "web_search" {
                        web_search_obs(fetch.as_ref(), &net, call).await
                    } else if call.name == "fetch_url" {
                        fetch_url_obs(fetch.as_ref(), call).await
                    } else if let Some((client, raw)) = router.get(&call.name) {
                        // 命名空间命中 → 路由到 MCP 服务器。
                        call_mcp_with_timeout(
                            client,
                            raw,
                            call.arguments.clone(),
                            mcp_tool_timeout(),
                        )
                        .await
                    } else {
                        execute_tool_call(call)
                    };
                    // 上下文卫生(根因):巨型工具输出入 history 前确定性截断(head+tail 预览),
                    // 止住单条巨输出撑爆上下文;零丢数据,文件可 read_file 区间重取。所有工具路径汇流此接缝。
                    let obs = bound_observation(obs);
                    // 无进展检测:工具输出与上一轮相同则 stall+1,否则清零。
                    let stall = if s.tool_output.as_deref() == Some(obs.as_str()) {
                        s.stall + 1
                    } else {
                        0
                    };
                    // 熔断计数:本轮观察为错误则 err_streak+1,成功则清零(与 stall 正交,兜「错误每轮不同」)。
                    let err_streak = if is_error_observation(&obs) {
                        s.err_streak + 1
                    } else {
                        0
                    };
                    // 纯侦察计数:read/search 等每轮+1;成功落盘改写清零;其余(run_shell/todo/…)保持。
                    // 修「输出每轮不同 → stall 永不触发 → 只查不改烧到 step_cap」的根因。
                    let explore_streak =
                        if is_land_edit_tool(&call.name) && !is_error_observation(&obs) {
                            0
                        } else if is_explore_tool(&call.name) {
                            s.explore_streak + 1
                        } else {
                            s.explore_streak
                        };
                    // Durable State 回填(事实驱动):在 obs 被移动前算好。
                    let durable = durable_updates(call, &obs);
                    let mut patches = vec![
                        Patch::Message(format!("act: {} -> {}", call.name, obs)),
                        // 工具结果按 role=tool 正确回灌(匹配 tool_call_id)。
                        Patch::PushHistory(Message::tool_result(call.id.clone(), obs.clone())),
                        Patch::SetStall(stall),
                        Patch::SetErrStreak(err_streak),
                        Patch::SetExploreStreak(explore_streak),
                        Patch::ToolOutput(Some(obs)),
                        Patch::PendingCall(None),
                    ];
                    // todo_write:把清单写进状态(REPL 会渲染 [x]/[~]/[ ])。
                    if call.name == "todo_write" {
                        patches.push(Patch::SetTodos(parse_todos(call)));
                    }
                    patches.extend(durable); // 记已改文件 / 上次报错
                    Patch::Batch(patches)
                }
                None => Patch::Message("act: no pending tool_call".to_string()),
            };
            Ok::<_, Infallible>(patch)
        }
    });

    match reviewer {
        // 有独立 reviewer:确定性通过后再让它复核作弊。
        Some(rv) => {
            let rv_c = rv.clone();
            g.add_node("verify", move |s: AgentState| {
                let reviewer = rv_c.clone();
                async move {
                    let det_ok = verify_ok(&s);
                    if !det_ok {
                        let reason = verify_failure_reason(&s);
                        return Ok::<_, provider::ProviderError>(Patch::Batch(vec![
                            Patch::Approved(false),
                            Patch::Issues(vec![reason.to_string()]),
                            Patch::Message(format!(
                                "verify: FAIL (deterministic: {reason}) -> back to reason"
                            )),
                        ]));
                    }
                    // 独立模型复核:给它轨迹,问是否合法达成(没作弊)。
                    let verdict = reviewer.complete(&review_request(&s)).await?;
                    let approved =
                        verdict.text.contains("APPROVE") && !verdict.text.contains("REJECT");
                    let patch = if approved {
                        Patch::Batch(vec![
                            Patch::Approved(true),
                            Patch::Message(
                                "verify: PASS (deterministic + 独立 reviewer)".to_string(),
                            ),
                        ])
                    } else {
                        Patch::Batch(vec![
                            Patch::Approved(false),
                            Patch::Issues(vec![format!("reviewer 打回: {}", verdict.text)]),
                            Patch::Message(format!("verify: reviewer REJECT -> {}", verdict.text)),
                        ])
                    };
                    Ok(patch)
                }
            });
        }
        // 无 reviewer:纯确定性 verify。
        None => {
            g.add_node("verify", verify_node);
        }
    }

    // 收束回合(#2 / 软中止):运行到达安全上限(回合上限)或卡死(无进展/连错)时,不再哑然腰斩,
    // 而是**软暂停**——让模型产一段面向用户的交接:已完成 / 还剩什么 / 建议的后续步骤,供用户参考续跑。
    // 一次非流式补全、不 offer 工具(逼其只出文本)、无出边 → 隐式 END,天然不成环、不会二次熔断。
    let provider_wrap = provider.clone();
    let system_wrap = system.clone();
    g.add_node("wrapup", move |s: AgentState| {
        let provider = provider_wrap.clone();
        let system = system_wrap.clone();
        async move {
            let reason = halt_reason(&s);
            let mut messages = to_messages(&system, &s);
            messages.push(Message::new(
                Role::System,
                format!(
                    "本轮到此**软暂停**(原因:{}。不是失败,且已不能再调用工具)。请用**用户的语言**\
                     写一段供用户参考的交接说明:①目前已完成/已改动了什么;②还剩什么没做;\
                     ③若要继续,给出具体、可直接照做的后续步骤或计划。直接说给用户听,\
                     别提「护栏/节点/超步」等内部机制。",
                    reason.as_str()
                ),
            ));
            let req = CompletionRequest {
                messages,
                tools: vec![],
            };
            // 交接说明生成失败也不阻断收尾(给个占位文本),halt_reason 仍据终态判定不变。
            let (text, usage) = match provider.complete(&req).await {
                Ok(c) => (c.text, c.usage),
                Err(e) => (
                    format!("(交接说明生成失败:{e})"),
                    provider::Usage::default(),
                ),
            };
            Ok::<_, Infallible>(Patch::Batch(vec![
                Patch::AddUsage(usage),
                // 以 `(final)` 约定渲染成模型终答(🤖 白);⏸ 标记「软暂停」+ 原因,供用户一眼分辨非正常完成。
                Patch::Message(format!("(final) ⏸ [{}] {}", reason.as_str(), text)),
                Patch::PushHistory(Message::assistant(text)),
            ]))
        }
    });

    g.set_entry("reason");
    g.add_conditional_edge("reason", reason_route);
    g.add_edge("act", "reason");
    // LLM 路径专用 verify 路由:熔断 → wrapup(收束回合)→ 隐式 END。
    g.add_conditional_edge("verify", verify_route_llm);

    graph_trace("compile.begin");
    let compiled = g.compile();
    graph_trace("compile.end");
    compiled
}

/// 给独立 reviewer 的复核请求:system 定角色 + user 附上 agent 的轨迹。
fn review_request(s: &AgentState) -> CompletionRequest {
    CompletionRequest {
        messages: vec![
            Message::new(
                Role::System,
                "You are an independent code reviewer. Given the agent's task and trajectory, \
                 decide whether it achieved the goal LEGITIMATELY or CHEATED (e.g. deleted/skipped \
                 failing tests, faked output). Reply exactly 'APPROVE' or 'REJECT: <reason>'.",
            ),
            Message::new(
                Role::User,
                format!("Task: {}\n\nTrajectory:\n{}", s.task, s.messages.join("\n")),
            ),
        ],
        tools: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;
    use langgraph::RunConfig;
    use mcp::McpClient;
    use provider::ToolCall;

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
    /// 从**已接近上限**起跑,只需几步即触达 —— 快、且不依赖 `MAX_STEPS` 的具体值(现为 2000)。
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

        let start = AgentState {
            steps: MAX_STEPS - 2,
            ..AgentState::new("impossible")
        };
        let out = app
            .invoke_with(start, &RunConfig::default(), None, None)
            .await
            .unwrap();

        assert!(!out.approved, "must not fake success");
        assert_eq!(out.steps, MAX_STEPS, "hard cap stops the runaway loop");
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

    /// P0b:一次工具调用后,模型面向的 history 里应出现 role=tool 结果(匹配 tool_call_id)+ 带 tool_calls 的 assistant。
    #[tokio::test]
    async fn history_carries_role_tool_after_tool_call() {
        use provider::{Completion, Role, ScriptedProvider};
        let scripted = ScriptedProvider::new(vec![
            Completion {
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: "run_shell".to_string(),
                    arguments: serde_json::json!({"cmd": "exit 0"}),
                }],
                ..Default::default()
            },
            Completion {
                text: "done".to_string(),
                ..Default::default()
            },
        ]);
        let app = build_llm_agent(Arc::new(scripted)).unwrap();
        let out = app.invoke(AgentState::new("build")).await.unwrap();

        // history = [user(build), assistant(tool_calls), tool_result(call_1), assistant(done)]
        assert!(out
            .history
            .iter()
            .any(|m| m.role == Role::Tool && m.tool_call_id.as_deref() == Some("call_1")));
        assert!(out
            .history
            .iter()
            .any(|m| m.role == Role::Assistant && !m.tool_calls.is_empty()));
        // to_messages 会在最前面加 system;history 首条是 user 任务。
        assert_eq!(out.history.first().map(|m| &m.role), Some(&Role::User));
    }

    /// M2 端到端:LLM 发一个**命名空间**工具调用 → act 路由到 MCP 服务器 → verify 认其结果 → approved。
    /// 用离线 FnTransport 站位真实 MCP 服务器,零联网。
    #[tokio::test]
    async fn llm_agent_routes_tool_call_to_mcp_server() {
        use mcp::FnTransport;
        use provider::{Completion, ScriptedProvider};

        // 假 MCP 服务器:有一个 check 工具,调用返回成功信号 "tests: passed"。
        let transport = FnTransport(|method: &str, _p: &serde_json::Value| match method {
            "initialize" => Ok(serde_json::json!({})),
            "tools/list" => Ok(serde_json::json!({"tools": [
                {"name": "check", "description": "run project checks", "inputSchema": {"type": "object"}}
            ]})),
            "tools/call" => {
                Ok(serde_json::json!({"content": [{"type": "text", "text": "tests: passed"}]}))
            }
            m => Err(mcp::McpError::BadResponse(m.to_string())),
        });
        let client = Arc::new(McpClient::new("ci", Box::new(transport)));
        let mcp_tools = resolve_mcp(vec![client]).await;

        // LLM:第 1 轮调命名空间工具 ci__check;第 2 轮收尾。
        let scripted = ScriptedProvider::new(vec![
            Completion {
                tool_calls: vec![ToolCall {
                    id: "1".to_string(),
                    name: "ci__check".to_string(),
                    arguments: serde_json::json!({}),
                }],
                ..Default::default()
            },
            Completion {
                text: "done".to_string(),
                ..Default::default()
            },
        ]);
        let app = build_llm_agent_with(Arc::new(scripted), mcp_tools).unwrap();
        let out = app.invoke(AgentState::new("run ci")).await.unwrap();

        assert!(out.approved, "MCP 工具返回 passed 应满足确定性闸");
        assert!(out.messages.iter().any(|m| m.contains("ci__check")));
        assert_eq!(out.tool_output.as_deref(), Some("tests: passed"));
    }

    #[tokio::test]
    async fn hanging_mcp_call_becomes_bounded_error_observation() {
        struct HangingTransport;
        #[async_trait::async_trait]
        impl mcp::McpTransport for HangingTransport {
            async fn request(
                &self,
                method: &str,
                _params: serde_json::Value,
            ) -> Result<serde_json::Value, mcp::McpError> {
                if method == "tools/call" {
                    std::future::pending::<Result<serde_json::Value, mcp::McpError>>().await
                } else {
                    Ok(serde_json::json!({}))
                }
            }
        }

        let client = mcp::McpClient::new("hang", Box::new(HangingTransport));
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            call_mcp_with_timeout(
                &client,
                "never_returns",
                serde_json::json!({}),
                Duration::from_millis(20),
            ),
        )
        .await
        .expect("wrapper must return before the outer test timeout");

        assert!(result.starts_with("mcp error: timed out after 20ms"));
        assert!(is_error_observation(&result));
    }

    // maker:跑 exit 0(确定性通过)然后收尾。
    fn maker_passes_then_finishes() -> provider::ScriptedProvider {
        use provider::{Completion, ScriptedProvider};
        ScriptedProvider::new(vec![
            Completion {
                tool_calls: vec![ToolCall {
                    id: "1".to_string(),
                    name: "run_shell".to_string(),
                    arguments: serde_json::json!({"cmd": "exit 0"}),
                }],
                ..Default::default()
            },
            Completion {
                text: "done".to_string(),
                ..Default::default()
            },
        ])
    }

    /// M4:确定性闸通过,但**独立 reviewer** 发现作弊 → 最终不批准。
    #[tokio::test]
    async fn independent_reviewer_rejects_cheating() {
        use provider::{Completion, ScriptedProvider};
        let reviewer = ScriptedProvider::new(
            (0..8)
                .map(|_| Completion {
                    text: "REJECT: agent deleted the failing test".to_string(),
                    ..Default::default()
                })
                .collect(),
        );
        let app = build_llm_agent_reviewed(
            Arc::new(maker_passes_then_finishes()),
            McpTools::empty(),
            Arc::new(reviewer),
        )
        .unwrap();
        // 近上限起跑:reviewer 每轮打回 → 本会一路重试到步上限;seed 令其两步即触达软中止,
        // 快且不改断言本意(reviewer REJECT → 终不批准)。
        let start = AgentState {
            steps: MAX_STEPS - 2,
            ..AgentState::new("make tests pass")
        };
        let out = app.invoke(start).await.unwrap();

        assert!(!out.approved, "独立 reviewer 应拦下作弊,即使确定性闸已过");
        assert!(out.messages.iter().any(|m| m.contains("reviewer REJECT")));
    }

    /// M4:确定性闸通过 + 独立 reviewer 认可 → 批准。
    #[tokio::test]
    async fn independent_reviewer_approves_legit_work() {
        use provider::{Completion, ScriptedProvider};
        let reviewer = ScriptedProvider::new(vec![Completion {
            text: "APPROVE".to_string(),
            ..Default::default()
        }]);
        let app = build_llm_agent_reviewed(
            Arc::new(maker_passes_then_finishes()),
            McpTools::empty(),
            Arc::new(reviewer),
        )
        .unwrap();
        let out = app
            .invoke(AgentState::new("make tests pass"))
            .await
            .unwrap();

        assert!(out.approved);
        assert!(out.messages.iter().any(|m| m.contains("独立 reviewer")));
        assert_eq!(out.steps, 2);
    }

    /// 回归(误报根治):模型读到**内容含 "error"/"failed" 字样但操作成功**的输出后 finish →
    /// 应**接受完成**(approved),不再被松散子串误判失败、踢进「否决→回 reason→再收尾」无尽环。
    #[tokio::test]
    async fn finish_accepted_despite_scary_substrings_in_content() {
        use provider::{Completion, ScriptedProvider};
        let mut path = std::env::temp_dir();
        path.push("ridge_scary_substrings.txt");
        std::fs::write(&path, "build log: 0 errors, 0 failed — all good").unwrap();

        let scripted = ScriptedProvider::new(vec![
            Completion {
                tool_calls: vec![ToolCall {
                    id: "1".to_string(),
                    name: "read_file".to_string(),
                    arguments: serde_json::json!({"path": path.to_str().unwrap()}),
                }],
                ..Default::default()
            },
            Completion {
                text: "日志显示 0 报错,收工".to_string(),
                ..Default::default()
            },
        ]);
        let app = build_llm_agent(Arc::new(scripted)).unwrap();
        let out = app.invoke(AgentState::new("看下构建日志")).await.unwrap();

        assert!(
            out.approved,
            "内容含 error/failed 字样但操作成功,finish 应被接受,不该误否"
        );
        assert_eq!(out.steps, 2, "read_file -> finish,不空转");
        std::fs::remove_file(&path).ok();
    }

    /// 通用性:开放式任务(工具输出无 exit0/passed 也无失败信号)+ 模型 finish → 接受完成,不空转到上限。
    /// (修复 MCP 信息类任务空转烧 token 的问题。)
    #[tokio::test]
    async fn open_ended_finish_accepted_when_no_failure_signal() {
        use provider::{Completion, ScriptedProvider};
        let mut path = std::env::temp_dir();
        path.push("ridge_open_ended.txt");
        std::fs::write(&path, "neutral content, no success or failure signal").unwrap();

        let scripted = ScriptedProvider::new(vec![
            Completion {
                tool_calls: vec![ToolCall {
                    id: "1".to_string(),
                    name: "read_file".to_string(),
                    arguments: serde_json::json!({"path": path.to_str().unwrap()}),
                }],
                ..Default::default()
            },
            Completion {
                text: "here is the content".to_string(),
                ..Default::default()
            },
        ]);
        let app = build_llm_agent(Arc::new(scripted)).unwrap();
        let out = app.invoke(AgentState::new("read the file")).await.unwrap();

        assert!(out.approved, "模型 finish 且无失败信号 → 接受,不该空转");
        assert_eq!(out.steps, 2);
        std::fs::remove_file(&path).ok();
    }

    /// #2 收束回合:护栏熔断(超预算)后不哑然 END,而是先经 `wrapup` 让模型产一段
    /// 面向用户的收束陈述(带停机原因标记),再结束。用离线 ScriptedProvider 模拟「反复调工具、
    /// 从不完成」直到超预算,零联网。
    #[tokio::test]
    async fn guardrail_halt_runs_wrapup_summary() {
        use provider::{Completion, ScriptedProvider, Usage};
        // 每次 reason 都调一个**必失败**的工具(读不存在的文件 → 非成功信号 → verify 不放行),
        // 且每步计 100 token;预算 150 → 第 2 次 reason 即超预算熔断。第 3 条(complete)是收束陈述。
        let tool_call = || Completion {
            tool_calls: vec![ToolCall {
                id: "t".to_string(),
                name: "read_file".to_string(),
                arguments: serde_json::json!({"path": "no/such/xyzzy-does-not-exist.txt"}),
            }],
            usage: Usage {
                prompt_tokens: 100,
                completion_tokens: 0,
            },
            ..Default::default()
        };
        let scripted = ScriptedProvider::new(vec![
            tool_call(),
            tool_call(),
            // wrapup 的 provider.complete(非流式)取到这条:模型的收束陈述。
            Completion {
                text: "已完成读取尝试;还差有效文件路径;建议确认路径后重试。[WRAPUP_MARK]"
                    .to_string(),
                ..Default::default()
            },
        ]);
        let app = build_llm_agent(Arc::new(scripted)).unwrap();
        let out = app
            .invoke(AgentState::new("读取某文件").with_budget(150))
            .await
            .unwrap();

        assert!(!out.approved, "护栏熔断不应伪装成成功");
        assert_eq!(halt_reason(&out), HaltReason::Budget, "停机原因应为超预算");
        // 收束陈述已产出并作为 (final) 终答呈现,且带停机原因标记。
        let wrapup = out
            .messages
            .iter()
            .find(|m| m.contains("[WRAPUP_MARK]"))
            .expect("应有一条收束陈述");
        assert!(
            wrapup.contains("(final)"),
            "收束陈述应以 (final) 终答样式呈现"
        );
        assert!(
            wrapup.contains("budget_exceeded"),
            "收束陈述应前置停机原因标记"
        );
        // 收束陈述也进了模型历史(供续轮/审计)。
        assert!(out
            .history
            .iter()
            .any(|m| m.content.contains("[WRAPUP_MARK]")));
    }

    /// 回归:模型「调一次失败工具 → 直接 finish」时,不得回 reason 原地空转再收尾(旧 bug:无尽收尾环,
    /// act 不跑 → stall/err_streak 冻结,唯一出口 step_cap=2000,白烧 token)。新路由:否决的 finish 一次
    /// wrapup 即 END。用离线 provider,零联网、确定性。
    #[tokio::test]
    async fn rejected_finish_does_not_spin_and_wraps_up_once() {
        use provider::{Completion, ScriptedProvider};
        let scripted = ScriptedProvider::new(vec![
            // 第 1 轮:调 read_file 读不存在的文件 → 观察含 "read error:"(失败信号)。
            Completion {
                tool_calls: vec![ToolCall {
                    id: "1".to_string(),
                    name: "read_file".to_string(),
                    arguments: serde_json::json!({"path": "no/such/xyzzy-nope.txt"}),
                }],
                ..Default::default()
            },
            // 第 2 轮:模型不再调工具、直接给终答(finish)。verify 因上一步失败信号否决之。
            Completion {
                text: "我看了下,应该好了".to_string(),
                ..Default::default()
            },
            // wrapup 的 provider.complete 取这条:诚实交接。旧 bug 下永远走不到 wrapup(在 reason↔verify 空转)。
            Completion {
                text: "已尝试读取但文件不存在;建议确认路径后重试。[WRAP]".to_string(),
                ..Default::default()
            },
        ]);
        let app = build_llm_agent(Arc::new(scripted)).unwrap();
        let out = app.invoke(AgentState::new("读个文件")).await.unwrap();

        assert!(!out.approved, "有失败信号不应伪装成功");
        assert_eq!(
            out.steps, 2,
            "read_file -> finish 即收 wrapup,不得空转至 step_cap"
        );
        assert_eq!(
            halt_reason(&out),
            HaltReason::Unverified,
            "非护栏熔断、模型自判完成却未验证通过 → unverified"
        );
        assert!(
            out.messages
                .iter()
                .any(|m| m.contains("[WRAP]") && m.contains("(final)")),
            "应有且仅经一次 wrapup 收束陈述作为 (final) 终答"
        );
    }

    /// #2 路由(纯函数):通过 → END;护栏熔断且未过 → wrapup;未过但可继续 → reason。
    #[test]
    fn verify_route_llm_sends_guardrail_halt_to_wrapup() {
        use langgraph::END;
        let approved = AgentState {
            approved: true,
            ..Default::default()
        };
        assert_eq!(verify_route_llm(&approved), vec![END.to_string()]);
        let over_budget = AgentState {
            budget_tokens: 100,
            total_tokens: 100,
            ..Default::default()
        };
        assert_eq!(verify_route_llm(&over_budget), vec!["wrapup".to_string()]);
        // 模型已自判 finish 却未通过验证(非 must_stop)→ 收 wrapup,**不**回 reason 空转成环。
        let rejected_finish = AgentState {
            last_action: Some("finish".to_string()),
            ..Default::default()
        };
        assert_eq!(
            verify_route_llm(&rejected_finish),
            vec!["wrapup".to_string()],
            "否决的 finish 须走 wrapup,不得回 reason(否则无尽收尾环)"
        );
        assert_eq!(
            verify_route_llm(&AgentState::default()),
            vec!["reason".to_string()]
        );
    }
}
