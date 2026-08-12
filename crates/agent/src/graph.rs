use crate::brain::{
    act_route, build_system_prompt_with_mode, explore_handoff_patch, is_explore_tool,
    is_land_edit_tool, reason_route, verify_failure_reason, verify_node, verify_ok,
    verify_route_llm,
};
use crate::context::{bound_observation, to_messages};
use crate::exec::{
    builtin_tool_specs, durable_updates, execute_tool_call, is_error_observation, parse_todos,
};
use crate::guard::{is_mutating_tool, read_only_block};
use crate::knowledge::{
    dispatch_batch_obs, dispatch_batch_spec, dispatch_obs, dispatch_spec, Agents, Skill,
};
use crate::mcp_tools::McpTools;
use crate::observe::{fetch_url_obs, preview_call, web_search_obs};
use crate::state::{
    needs_approval, AgentState, Approver, AutoApprove, Patch, MAX_DISPATCH_BATCHES,
};
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
    let specs = build_tool_specs(&mcp, &agents, read_only);
    let system = Arc::new(build_system_prompt_with_mode(&skills, read_only));
    let mcp = Arc::new(mcp);
    let mut graph = StateGraph::<AgentState>::new();

    add_reason_node(
        &mut graph,
        provider.clone(),
        system.clone(),
        specs,
        token_bus,
    );
    graph.add_node("explore_handoff", |state: AgentState| async move {
        Ok::<_, Infallible>(explore_handoff_patch(&state))
    });
    let fetch: Arc<dyn provider::search::WebFetch> =
        Arc::new(provider::search::ReqwestFetch::new());
    let net = Arc::new(std::sync::OnceLock::new());
    add_act_node(
        &mut graph,
        ActContext {
            mcp,
            approver,
            fetch,
            net,
            agents,
            main_provider: provider.clone(),
            read_only,
        },
    );
    add_verify_node(&mut graph, reviewer);
    add_wrapup_node(&mut graph, provider, system);

    graph.set_entry("reason");
    graph.add_conditional_edge("reason", reason_route);
    graph.add_edge("explore_handoff", "reason");
    graph.add_conditional_edge("act", act_route);
    graph.add_conditional_edge("verify", verify_route_llm);
    graph_trace("compile.begin");
    let compiled = graph.compile();
    graph_trace("compile.end");
    compiled
}

fn build_tool_specs(mcp: &McpTools, agents: &Agents, read_only: bool) -> Vec<provider::ToolSpec> {
    let mut specs = builtin_tool_specs();
    if read_only {
        specs.retain(|spec| !is_mutating_tool(&spec.name));
    }
    if let Some(dispatch) = dispatch_spec(agents) {
        specs.push(dispatch);
    }
    if let Some(dispatch) = dispatch_batch_spec(agents) {
        specs.push(dispatch);
    }
    if !read_only {
        specs.extend(mcp.specs.clone());
    }
    graph_trace("specs.ready");
    specs
}

fn add_reason_node(
    graph: &mut StateGraph<AgentState>,
    provider: Arc<dyn LlmProvider>,
    system: Arc<String>,
    specs: Vec<provider::ToolSpec>,
    token_bus: TokenBus,
) {
    graph.add_node("reason", move |state: AgentState| {
        let provider = provider.clone();
        let system = system.clone();
        let force_action = state.explore_handoff && !state.explore_action_used;
        let candidate_tools = if force_action {
            handoff_tool_specs(&specs)
        } else {
            specs.clone()
        };
        let tools = available_tool_specs(&candidate_tools, &state);
        let bus = token_bus.clone();
        async move {
            let mut messages = to_messages(&system, &state);
            if force_action {
                messages.push(Message::new(
                    Role::System,
                    "<explore_handoff>Read/search budget is exhausted. Do not call read, search, web, codegraph, or dispatch tools. Choose the smallest safe edit/write/apply_edits or verification command now; if no safe action is possible, state the concrete blocker. Do not claim completion without an objective result.</explore_handoff>",
                ));
            }
            let request = CompletionRequest {
                messages,
                tools,
            };
            let on_token = move |chunk: StreamChunk| {
                if let Some(sender) = bus.lock().unwrap().as_ref() {
                    let _ = sender.send(chunk);
                }
            };
            tracing::debug!(
                step = state.steps + 1,
                msgs = request.messages.len(),
                "llm request"
            );
            let completion = provider.complete_streaming(&request, &on_token).await?;
            let usage = completion.usage.clone();
            let tokens = usage.total() as usize;
            tracing::debug!(
                step = state.steps + 1,
                tokens,
                tool_calls = completion.tool_calls.len(),
                "llm response"
            );
            Ok::<_, provider::ProviderError>(reason_patch(
                &state,
                completion.text,
                completion.tool_calls.into_iter().next(),
                usage,
            ))
        }
    });
    graph_trace("system.ready");
}

fn handoff_tool_specs(specs: &[provider::ToolSpec]) -> Vec<provider::ToolSpec> {
    // MCP action tools stay available here; `execute_pending_call` still
    // applies the normal approval/read-only gates. Only known exploration
    // tools are removed, so adding an MCP server does not make the handoff
    // path silently unable to perform its domain-specific action.
    specs
        .iter()
        .filter(|spec| !is_explore_tool(&spec.name))
        .cloned()
        .collect()
}

fn available_tool_specs(
    specs: &[provider::ToolSpec],
    state: &AgentState,
) -> Vec<provider::ToolSpec> {
    specs
        .iter()
        .filter(|spec| {
            !(state.dispatch_wave_count() >= MAX_DISPATCH_BATCHES && spec.name == "dispatch_agents")
                && !(state.codegraph_unavailable && spec.name.starts_with("codegraph__"))
        })
        .cloned()
        .collect()
}

fn reason_patch(
    state: &AgentState,
    text: String,
    call: Option<provider::ToolCall>,
    usage: provider::Usage,
) -> Patch {
    match call {
        Some(call) => Patch::Batch(vec![
            Patch::BumpStep,
            Patch::AddUsage(usage),
            Patch::Message(format!(
                "reason#{}: tool_call {} {}",
                state.steps + 1,
                call.name,
                call.arguments
            )),
            Patch::PushHistory(Message::assistant(text).with_tool_calls(vec![call.clone()])),
            Patch::PendingCall(Some(call)),
            Patch::Action(Some("tool".to_string())),
        ]),
        None => Patch::Batch(vec![
            Patch::BumpStep,
            Patch::AddUsage(usage),
            Patch::Message(format!("reason#{}: (final) {text}", state.steps + 1)),
            Patch::PushHistory(Message::assistant(text)),
            Patch::PendingCall(None),
            Patch::Action(Some("finish".to_string())),
        ]),
    }
}

#[derive(Clone)]
struct ActContext {
    mcp: Arc<McpTools>,
    approver: Arc<dyn Approver>,
    fetch: Arc<dyn provider::search::WebFetch>,
    net: Arc<std::sync::OnceLock<provider::search::NetEnv>>,
    agents: Arc<Agents>,
    main_provider: Arc<dyn LlmProvider>,
    read_only: bool,
}

fn add_act_node(graph: &mut StateGraph<AgentState>, context: ActContext) {
    graph.add_node("act", move |state: AgentState| {
        let context = context.clone();
        async move {
            let patch = match state.pending_call.as_ref() {
                Some(call) => {
                    let observation = if state.dispatch_wave_count() >= MAX_DISPATCH_BATCHES
                        && call.name == "dispatch_agents"
                    {
                        format!(
                            "BLOCKED (dispatch budget): {}/{} dispatch waves used; continue the main task without another batch",
                            state.dispatch_wave_count(), MAX_DISPATCH_BATCHES
                        )
                    } else if state.codegraph_unavailable && call.name.starts_with("codegraph__") {
                        "BLOCKED (codegraph unavailable): use built-in read_file/search or act; do not retry CodeGraph"
                            .to_string()
                    } else if state.explore_handoff && is_explore_tool(&call.name) {
                        format!(
                            "BLOCKED (explore handoff): {} is read-only; choose an edit or verification action",
                            call.name
                        )
                    } else {
                        execute_pending_call(call, &context).await
                    };
                    act_patch(&state, call, observation)
                }
                None => Patch::Message("act: no pending tool_call".to_string()),
            };
            Ok::<_, Infallible>(patch)
        }
    });
}

async fn execute_pending_call(call: &provider::ToolCall, context: &ActContext) -> String {
    if let Some(message) = read_only_block(context.read_only, &call.name) {
        return message;
    }
    if needs_approval(&call.name) && !context.approver.approve(&call.name, &preview_call(call)) {
        return format!("permission denied by user: {}", call.name);
    }
    if call.name == "dispatch_agent" {
        return dispatch_obs(&context.agents, &context.main_provider, call).await;
    }
    if call.name == "dispatch_agents" {
        return dispatch_batch_obs(&context.agents, &context.main_provider, call).await;
    }
    if call.name == "web_search" {
        return web_search_obs(context.fetch.as_ref(), &context.net, call).await;
    }
    if call.name == "fetch_url" {
        return fetch_url_obs(context.fetch.as_ref(), call).await;
    }
    if let Some((client, raw)) = context.mcp.router.get(&call.name) {
        return call_mcp_with_timeout(client, raw, call.arguments.clone(), mcp_tool_timeout())
            .await;
    }
    execute_tool_call(call)
}

fn act_patch(state: &AgentState, call: &provider::ToolCall, observation: String) -> Patch {
    let display_observation = observation.clone();
    let observation = bound_observation(observation);
    let stall = if state.tool_output.as_deref() == Some(observation.as_str()) {
        state.stall + 1
    } else {
        0
    };
    let err_streak = if is_error_observation(&observation) {
        state.err_streak + 1
    } else {
        0
    };
    let explore_streak = next_explore_streak(state, call, &observation);
    let mut patches = vec![
        Patch::Message(format!("act: {} -> {}", call.name, observation)),
        Patch::DisplayMessage(format!("act: {} -> {}", call.name, display_observation)),
        Patch::PushHistory(Message::tool_result(call.id.clone(), observation.clone())),
        Patch::SetStall(stall),
        Patch::SetErrStreak(err_streak),
        Patch::SetExploreStreak(explore_streak),
        Patch::ToolOutput(Some(observation.clone())),
        Patch::PendingCall(None),
    ];
    if state.explore_handoff {
        let action_used = !is_explore_tool(&call.name) && !is_error_observation(&observation);
        patches.push(Patch::SetExploreActionUsed(action_used));
    }
    if state.explore_handoff && is_land_edit_tool(&call.name) && !is_error_observation(&observation)
    {
        patches.push(Patch::SetExploreHandoff(false));
        patches.push(Patch::SetExploreActionUsed(false));
    }
    if call.name == "todo_write" {
        patches.push(Patch::SetTodos(parse_todos(call)));
    }
    if call.name == "dispatch_agents" {
        patches.push(Patch::SetDispatchBatches(
            state
                .dispatch_wave_count()
                .saturating_add(1)
                .min(MAX_DISPATCH_BATCHES),
        ));
    }
    if call.name.starts_with("codegraph__") && codegraph_unavailable(&observation) {
        patches.push(Patch::SetCodegraphUnavailable(true));
    }
    patches.extend(durable_updates(call, &observation));
    Patch::Batch(patches)
}

fn codegraph_unavailable(observation: &str) -> bool {
    let lower = observation.to_ascii_lowercase();
    lower.contains("isn't indexed with codegraph")
        || lower.contains("no .codegraph")
        || lower.contains("codegraph cannot query")
        || lower.contains("codegraph-mcp unavailable")
}

fn next_explore_streak(state: &AgentState, call: &provider::ToolCall, observation: &str) -> usize {
    if is_land_edit_tool(&call.name) && !is_error_observation(observation) {
        0
    } else if is_explore_tool(&call.name) {
        state.explore_streak + 1
    } else {
        state.explore_streak
    }
}

fn add_verify_node(graph: &mut StateGraph<AgentState>, reviewer: Option<Arc<dyn LlmProvider>>) {
    match reviewer {
        Some(reviewer) => {
            graph.add_node("verify", move |state: AgentState| {
                let reviewer = reviewer.clone();
                async move { reviewed_patch(&reviewer, &state).await }
            });
        }
        None => {
            graph.add_node("verify", verify_node);
        }
    }
}

async fn reviewed_patch(
    reviewer: &Arc<dyn LlmProvider>,
    state: &AgentState,
) -> Result<Patch, provider::ProviderError> {
    if !verify_ok(state) {
        let reason = verify_failure_reason(state);
        return Ok(Patch::Batch(vec![
            Patch::Approved(false),
            Patch::Issues(vec![reason.to_string()]),
            Patch::Message(format!(
                "verify: FAIL (deterministic: {reason}) -> back to reason"
            )),
        ]));
    }
    let verdict = reviewer.complete(&review_request(state)).await?;
    if verdict.text.contains("APPROVE") && !verdict.text.contains("REJECT") {
        return Ok(Patch::Batch(vec![
            Patch::Approved(true),
            Patch::Message("verify: PASS (deterministic + 独立 reviewer)".to_string()),
        ]));
    }
    Ok(Patch::Batch(vec![
        Patch::Approved(false),
        Patch::Issues(vec![format!("reviewer 打回: {}", verdict.text)]),
        Patch::Message(format!("verify: reviewer REJECT -> {}", verdict.text)),
    ]))
}

fn add_wrapup_node(
    graph: &mut StateGraph<AgentState>,
    provider: Arc<dyn LlmProvider>,
    system: Arc<String>,
) {
    graph.add_node("wrapup", move |state: AgentState| {
        let provider = provider.clone();
        let system = system.clone();
        async move {
            let reason = halt_reason(&state);
            let mut messages = to_messages(&system, &state);
            messages.push(Message::new(
                Role::System,
                format!(
                    "本轮到此**软暂停**(原因:{}).不是失败,且已不能再调用工具。请用**用户的语言**                     写一段供用户参考的交接说明:①目前已完成/已改动了什么?②还剩什么没做?                     ③若要继续,给出具体、可直接照做的后续步骤或计划。直接说给用户听,                     别提“护栏/节点/超步”等内部机制。",
                    reason.as_str()
                ),
            ));
            let request = CompletionRequest {
                messages,
                tools: vec![],
            };
            let (text, usage) = match provider.complete(&request).await {
                Ok(completion) => (completion.text, completion.usage),
                Err(error) => (
                    format!("(交接说明生成失败:{error})"),
                    provider::Usage::default(),
                ),
            };
            Ok::<_, Infallible>(Patch::Batch(vec![
                Patch::AddUsage(usage),
                Patch::Message(format!("(final) ⏸[{}] {}", reason.as_str(), text)),
                Patch::PushHistory(Message::assistant(text)),
            ]))
        }
    });
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
    use super::{call_mcp_with_timeout, halt_reason, is_error_observation, verify_route_llm};
    use crate::{
        build_agent, build_llm_agent, build_llm_agent_reviewed, build_llm_agent_with, default_tool,
        resolve_mcp, scripted, AgentState, Brain, HaltReason, McpTools, Tool, MAX_DISPATCH_BATCHES,
        MAX_EXPLORE, MAX_STEPS,
    };
    use langgraph::GraphState;
    use langgraph::RunConfig;
    use mcp::McpClient;
    use provider::{ToolCall, ToolSpec};
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn display_stream_keeps_full_observation_while_model_stream_stays_bounded() {
        let call = ToolCall {
            id: "full-display".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "src/large.rs"}),
        };
        let observation = format!("HEAD_MARK\n{}\nTAIL_MARK", "middle line\n".repeat(2_000));
        let mut state = AgentState::new("inspect");
        state.apply(super::act_patch(&state, &call, observation));

        assert!(state.messages[0].contains("截断"));
        assert!(state.messages[0].chars().count() < state.display_messages[0].chars().count());
        assert!(state.display_messages[0].contains("HEAD_MARK"));
        assert!(state.display_messages[0].contains("middle line\nmiddle line\nmiddle line"));
        assert!(state.display_messages[0].contains("TAIL_MARK"));
    }

    #[test]
    fn explore_handoff_does_not_count_blocked_read_as_action() {
        let call = ToolCall {
            id: "blocked-read".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "src/lib.rs"}),
        };
        let mut state = AgentState::new("make a change");
        state.explore_handoff = true;
        state.explore_streak = MAX_EXPLORE;
        state.apply(super::act_patch(
            &state,
            &call,
            "BLOCKED (explore handoff): read_file is read-only; choose an edit or verification action"
                .into(),
        ));

        assert!(state.explore_handoff);
        assert!(!state.explore_action_used);
    }

    #[test]
    fn explore_handoff_counts_successful_action_and_edit_clears_guard() {
        let verify_call = ToolCall {
            id: "verify".into(),
            name: "run_shell".into(),
            arguments: serde_json::json!({"cmd": "cargo test"}),
        };
        let mut verified = AgentState::new("verify");
        verified.explore_handoff = true;
        verified.apply(super::act_patch(
            &verified,
            &verify_call,
            "exit 0: ok".into(),
        ));
        assert!(verified.explore_handoff);
        assert!(verified.explore_action_used);

        let edit_call = ToolCall {
            id: "edit".into(),
            name: "edit_file".into(),
            arguments: serde_json::json!({}),
        };
        let mut edited = AgentState::new("edit");
        edited.explore_handoff = true;
        edited.apply(super::act_patch(
            &edited,
            &edit_call,
            "edited 1 file".into(),
        ));
        assert!(!edited.explore_handoff);
        assert!(!edited.explore_action_used);
    }

    #[test]
    fn dispatch_batches_allow_multiple_waves_until_runtime_budget() {
        let specs = vec![
            ToolSpec {
                name: "dispatch_agents".into(),
                description: "batch".into(),
                schema: serde_json::json!({}),
            },
            ToolSpec {
                name: "read_file".into(),
                description: "read".into(),
                schema: serde_json::json!({}),
            },
        ];
        let mut state = AgentState::new("inspect");
        assert_eq!(super::available_tool_specs(&specs, &state).len(), 2);
        state.dispatch_batches_used = 1;
        assert_eq!(super::available_tool_specs(&specs, &state).len(), 2);
        state.dispatch_batches_used = MAX_DISPATCH_BATCHES;
        let available = super::available_tool_specs(&specs, &state);
        assert_eq!(available.len(), 1);
        assert_eq!(available[0].name, "read_file");
    }

    #[test]
    fn dispatch_batch_attempt_consumes_one_wave_even_on_failure() {
        let call = ToolCall {
            id: "batch-1".into(),
            name: "dispatch_agents".into(),
            arguments: serde_json::json!({"tasks": []}),
        };
        let mut state = AgentState::new("inspect");
        state.apply(super::act_patch(
            &state,
            &call,
            "dispatch_agents requires 2-3 tasks, got 0".into(),
        ));
        assert_eq!(state.dispatch_wave_count(), 1);
        state.dispatch_batches_used = MAX_DISPATCH_BATCHES;
        state.apply(super::act_patch(
            &state,
            &call,
            "BLOCKED (dispatch budget): 8/8 dispatch waves used".into(),
        ));
        assert_eq!(state.dispatch_wave_count(), MAX_DISPATCH_BATCHES);
    }

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

    #[tokio::test]
    async fn repeated_exploration_gets_one_real_action_turn() {
        use provider::{Completion, ScriptedProvider};
        let mut steps = Vec::with_capacity(MAX_EXPLORE + 1);
        let mut paths = Vec::with_capacity(MAX_EXPLORE);
        for index in 0..MAX_EXPLORE {
            let mut path = std::env::temp_dir();
            path.push(format!(
                "ridge-explore-handoff-{}-{index}.txt",
                std::process::id()
            ));
            std::fs::write(&path, format!("handoff target {index}")).unwrap();
            let path_arg = path.to_string_lossy().into_owned();
            paths.push(path);
            steps.push(Completion {
                tool_calls: vec![ToolCall {
                    id: format!("read-{index}"),
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": path_arg}),
                }],
                ..Default::default()
            });
        }
        steps.push(Completion {
            tool_calls: vec![ToolCall {
                id: "verify".into(),
                name: "run_shell".into(),
                arguments: serde_json::json!({"cmd": "exit 0"}),
            }],
            ..Default::default()
        });
        let scripted = ScriptedProvider::new(steps);
        let app = build_llm_agent(Arc::new(scripted)).unwrap();
        let out = app
            .invoke(AgentState::new("inspect then verify"))
            .await
            .unwrap();
        assert!(out.approved, "messages: {:?}", out.messages);
        assert!(out
            .messages
            .iter()
            .any(|message| message.contains("exploration guard triggered")));
        assert!(out
            .messages
            .iter()
            .any(|message| message.contains("run_shell")));
        for path in paths {
            std::fs::remove_file(path).ok();
        }
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
