use crate::brain::*;
use crate::context::*;
use crate::exec::*;
use crate::guard::*;
use crate::knowledge::*;
use crate::mcp_tools::*;
use crate::observe::*;
use crate::state::*;
use langgraph::{CompiledGraph, GraphError, StateGraph};
use provider::{CompletionRequest, LlmProvider, Message, Role};
use std::convert::Infallible;
use std::sync::Arc;

// halt_reason 已移至 orchestrate;此处再导出,保持 `crate::graph::halt_reason` 路径不变
//(signals.rs 经 `use crate::graph::*` 依赖它,不在本次改动范围内)。
pub(crate) use crate::orchestrate::halt_reason;

/// 流式 token 总线:REPL 每回合把一个 sender 塞进来,reason 节点边收 provider 的增量文本
/// 边往里发,REPL 侧就能**逐字显示**(像 Claude Code)。`None` = 该回合不流式。
pub type TokenBus = Arc<std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<String>>>>;

/// 一个「永不流式」的空总线(测试 / 非交互装配用)。
pub fn null_token_bus() -> TokenBus {
    Arc::new(std::sync::Mutex::new(None))
}

/// 用**真实 LLM provider** 装配 agent 图(不接 MCP)。见 [`build_llm_agent_with`]。
pub fn build_llm_agent(
    provider: Arc<dyn LlmProvider>,
) -> Result<CompiledGraph<AgentState>, GraphError> {
    build_llm_agent_with(provider, McpTools::empty())
}

/// 装配 agent 图,并把 MCP 工具并入(确定性 verify,无独立模型复核,一律放行)。
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
    let system = Arc::new(build_system_prompt(&skills));

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
            // 流式:provider 每吐一段文本就发进总线,REPL 侧逐字显示。无总线 sender 则等同整段。
            let on_token = move |t: String| {
                if let Some(tx) = bus.lock().unwrap().as_ref() {
                    let _ = tx.send(t);
                }
            };
            tracing::debug!(step = s.steps + 1, msgs = req.messages.len(), "llm request");
            let completion = provider.complete_streaming(&req, &on_token).await?;
            let tokens = completion.usage.total() as usize; // 成本记账
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
                    Patch::AddTokens(tokens),
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
                    Patch::AddTokens(tokens),
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
                        match client.call_tool(raw, call.arguments.clone()).await {
                            Ok(t) => t,
                            Err(e) => format!("mcp error: {e}"),
                        }
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
                    // Durable State 回填(事实驱动):在 obs 被移动前算好。
                    let durable = durable_updates(call, &obs);
                    let mut patches = vec![
                        Patch::Message(format!("act: {} -> {}", call.name, obs)),
                        // 工具结果按 role=tool 正确回灌(匹配 tool_call_id)。
                        Patch::PushHistory(Message::tool_result(call.id.clone(), obs.clone())),
                        Patch::SetStall(stall),
                        Patch::SetErrStreak(err_streak),
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
                        return Ok::<_, provider::ProviderError>(Patch::Batch(vec![
                            Patch::Approved(false),
                            Patch::Issues(vec!["build/tests not passing".to_string()]),
                            Patch::Message(
                                "verify: FAIL (deterministic) -> back to reason".to_string(),
                            ),
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

    g.set_entry("reason");
    g.add_conditional_edge("reason", reason_route);
    g.add_edge("act", "reason");
    g.add_conditional_edge("verify", verify_route);

    g.compile()
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
        let out = app
            .invoke(AgentState::new("make tests pass"))
            .await
            .unwrap();

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
}
