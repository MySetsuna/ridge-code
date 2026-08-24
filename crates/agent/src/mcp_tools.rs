use crate::rich_output::{Color, RichOutput};
use crate::state::Todo;
use mcp::{McpClient, McpError};
use provider::ToolSpec;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// MCP server 生命周期中用户可见的阶段。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpServerState {
    Configured,
    Started,
    Initialized,
    ToolsListed,
    Failed,
}

impl McpServerState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Configured => "configured",
            Self::Started => "started",
            Self::Initialized => "initialized",
            Self::ToolsListed => "tools listed",
            Self::Failed => "failed",
        }
    }
}

/// MCP server 当前状态及其已走过的生命周期，供 TUI `/mcp` 展示。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServerStatus {
    pub name: String,
    pub state: McpServerState,
    pub trail: Vec<McpServerState>,
    pub detail: String,
}

impl McpServerStatus {
    pub fn configured(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            state: McpServerState::Configured,
            trail: vec![McpServerState::Configured],
            detail: "configured".to_string(),
        }
    }

    fn advance(&mut self, state: McpServerState, detail: impl Into<String>) {
        self.state = state;
        if self.trail.last().copied() != Some(state) {
            self.trail.push(state);
        }
        self.detail = detail.into();
    }

    fn started(&mut self) {
        self.advance(McpServerState::Started, "stdio process started");
    }

    fn initialized(&mut self) {
        self.advance(McpServerState::Initialized, "initialize succeeded");
    }

    fn tools_listed(&mut self, count: usize) {
        self.advance(
            McpServerState::ToolsListed,
            format!("{count} tool(s) listed"),
        );
    }

    pub fn failed(&mut self, detail: impl Into<String>) {
        self.advance(McpServerState::Failed, detail);
    }

    pub fn trail_labels(&self) -> Vec<&'static str> {
        self.trail.iter().map(|state| state.label()).collect()
    }
}

/// 将错误压缩成不含命令参数、token 或 API key 的可展示原因。
pub fn mcp_error_summary(error: &McpError) -> String {
    error.redacted_summary()
}

/// 已连好的 MCP 工具:暴露给 LLM 的 [`ToolSpec`] + 「命名空间名 → (客户端, 原始工具名)」路由表。
#[derive(Default)]
pub struct McpTools {
    pub(crate) specs: Vec<ToolSpec>,
    pub(crate) router: HashMap<String, (Arc<McpClient>, String)>,
    statuses: Vec<McpServerStatus>,
    startup_errors: Vec<String>,
}

impl McpTools {
    pub fn empty() -> Self {
        Self::default()
    }

    /// 已接入的 MCP 工具名(命名空间形式,如 `nlm__notebook_list`)。供 `/tools` 列举。
    pub fn tool_names(&self) -> Vec<String> {
        self.specs.iter().map(|s| s.name.clone()).collect()
    }

    pub fn statuses(&self) -> &[McpServerStatus] {
        &self.statuses
    }

    /// Deterministic startup errors, including namespace collisions. A
    /// colliding tool is removed from both the LLM spec list and router.
    pub fn startup_errors(&self) -> &[String] {
        &self.startup_errors
    }
}

/// 连上一批 MCP 客户端:各自 initialize + list_tools,把工具归一化成 [`ToolSpec`](命名空间)+ 建路由表。
/// **降级不崩**:单个服务器连不上/列不出工具 → 跳过,其余照常。
pub async fn resolve_mcp(clients: Vec<Arc<McpClient>>) -> McpTools {
    resolve_mcp_with_statuses(clients, Vec::new()).await
}

/// 与 [`resolve_mcp`] 相同，但接收启动阶段已记录的 configured/failed 状态。
pub async fn resolve_mcp_with_statuses(
    clients: Vec<Arc<McpClient>>,
    statuses: Vec<McpServerStatus>,
) -> McpTools {
    resolve_mcp_with_options(
        clients,
        statuses,
        mcp_startup_timeout(),
        mcp_startup_parallelism(),
    )
    .await
}

async fn resolve_mcp_with_options(
    clients: Vec<Arc<McpClient>>,
    mut statuses: Vec<McpServerStatus>,
    timeout: Duration,
    parallelism: usize,
) -> McpTools {
    let ordered_clients = clients;
    let semaphore = Arc::new(tokio::sync::Semaphore::new(parallelism.max(1)));
    let mut tasks = Vec::with_capacity(ordered_clients.len());

    // Run each server independently, but collect by input index below.  A
    // slow server therefore cannot delay a fast one, and completion order can
    // never change the visible status/tool order or namespace winner.
    for (index, client) in ordered_clients.iter().cloned().enumerate() {
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("MCP startup semaphore remains open");
        tasks.push(tokio::spawn(async move {
            let _permit = permit;
            (index, resolve_mcp_client(client, timeout).await)
        }));
    }

    let mut results = (0..ordered_clients.len())
        .map(|_| None)
        .collect::<Vec<Option<ClientResolution>>>();
    for task in tasks {
        match task.await {
            Ok((index, result)) => results[index] = Some(result),
            Err(_) => {
                // Join failures contain no server payload.  The corresponding
                // indexed slot is reported as a generic, redacted failure.
            }
        }
    }

    let mut out = McpTools {
        statuses: std::mem::take(&mut statuses),
        ..McpTools::empty()
    };
    let mut owners = HashMap::<String, String>::new();
    let mut collided_names = std::collections::BTreeSet::<String>::new();
    let mut collision_servers = std::collections::BTreeSet::<String>::new();
    for (index, client) in ordered_clients.into_iter().enumerate() {
        let name = client.namespace().to_string();
        status_for(&mut out.statuses, &name).started();
        match results[index].take() {
            Some(ClientResolution::Ready(tools)) => {
                status_for(&mut out.statuses, &name).initialized();
                let tools = stable_tools(tools);
                status_for(&mut out.statuses, &name).tools_listed(tools.len());
                append_tools(
                    &mut out,
                    &client,
                    tools,
                    &mut owners,
                    &mut collided_names,
                    &mut collision_servers,
                );
            }
            Some(ClientResolution::Failed(failure)) => {
                if failure.stage == StartupStage::ToolsList {
                    status_for(&mut out.statuses, &name).initialized();
                }
                status_for(&mut out.statuses, &name).failed(failure.detail());
            }
            Some(ClientResolution::TimedOut(stage)) => {
                if stage == StartupStage::ToolsList {
                    status_for(&mut out.statuses, &name).initialized();
                }
                status_for(&mut out.statuses, &name).failed(format!(
                    "{} timed out after {}ms",
                    stage.label(),
                    timeout.as_millis()
                ));
            }
            None => {
                status_for(&mut out.statuses, &name).failed("startup task failed");
            }
        }
    }
    if !out.startup_errors.is_empty() {
        let detail = format!("namespace collision(s): {}", out.startup_errors.join(", "));
        for status in &mut out.statuses {
            if collision_servers.contains(&status.name) {
                status.failed(detail.clone());
            }
        }
    }
    out
}

const DEFAULT_MCP_STARTUP_TIMEOUT_SECS: u64 = 15;
const MAX_MCP_STARTUP_TIMEOUT_SECS: u64 = 300;
const DEFAULT_MCP_STARTUP_PARALLELISM: usize = 4;
const MAX_MCP_STARTUP_PARALLELISM: usize = 32;

fn mcp_startup_timeout() -> Duration {
    let max = Duration::from_secs(MAX_MCP_STARTUP_TIMEOUT_SECS);
    if let Some(milliseconds) = std::env::var("RIDGE_MCP_STARTUP_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
    {
        return Duration::from_millis(milliseconds).min(max);
    }
    if let Some(seconds) = std::env::var("RIDGE_MCP_STARTUP_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
    {
        return Duration::from_secs(seconds).min(max);
    }
    Duration::from_secs(DEFAULT_MCP_STARTUP_TIMEOUT_SECS)
}

fn mcp_startup_parallelism() -> usize {
    std::env::var("RIDGE_MCP_STARTUP_PARALLELISM")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .map_or(DEFAULT_MCP_STARTUP_PARALLELISM, |value| {
            value.min(MAX_MCP_STARTUP_PARALLELISM)
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartupStage {
    Initialize,
    ToolsList,
}

impl StartupStage {
    fn label(self) -> &'static str {
        match self {
            Self::Initialize => "initialize",
            Self::ToolsList => "tools/list",
        }
    }
}

struct StartupFailure {
    stage: StartupStage,
    error: McpError,
}

impl StartupFailure {
    fn detail(&self) -> String {
        format!(
            "{} failed: {}",
            self.stage.label(),
            mcp_error_summary(&self.error)
        )
    }
}

enum ClientResolution {
    Ready(Vec<mcp::McpTool>),
    Failed(StartupFailure),
    TimedOut(StartupStage),
}

async fn resolve_mcp_client(client: Arc<McpClient>, timeout: Duration) -> ClientResolution {
    let mut stage = StartupStage::Initialize;
    let result = tokio::time::timeout(timeout, async {
        client.initialize().await.map_err(|error| StartupFailure {
            stage: StartupStage::Initialize,
            error,
        })?;
        stage = StartupStage::ToolsList;
        client.list_tools().await.map_err(|error| StartupFailure {
            stage: StartupStage::ToolsList,
            error,
        })
    })
    .await;
    match result {
        Ok(Ok(tools)) => ClientResolution::Ready(tools),
        Ok(Err(failure)) => ClientResolution::Failed(failure),
        Err(_) => ClientResolution::TimedOut(stage),
    }
}

fn stable_tools(mut tools: Vec<mcp::McpTool>) -> Vec<mcp::McpTool> {
    tools.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.description.cmp(&right.description))
            .then_with(|| {
                serde_json::to_string(&left.input_schema)
                    .unwrap_or_default()
                    .cmp(&serde_json::to_string(&right.input_schema).unwrap_or_default())
            })
    });
    tools.dedup_by(|left, right| left.name == right.name);
    tools
}

fn append_tools(
    out: &mut McpTools,
    client: &Arc<McpClient>,
    tools: Vec<mcp::McpTool>,
    owners: &mut HashMap<String, String>,
    collided_names: &mut std::collections::BTreeSet<String>,
    collision_servers: &mut std::collections::BTreeSet<String>,
) {
    for tool in tools {
        let namespace = client.namespaced(&tool.name);
        if collided_names.contains(&namespace) {
            collision_servers.insert(client.namespace().to_string());
            continue;
        }
        if let Some(owner) = owners.get(&namespace).cloned() {
            out.router.remove(&namespace);
            out.specs.retain(|spec| spec.name != namespace);
            collided_names.insert(namespace.clone());
            collision_servers.insert(owner.clone());
            collision_servers.insert(client.namespace().to_string());
            out.startup_errors
                .push(format!("{namespace} ({owner}, {})", client.namespace()));
            continue;
        }
        out.specs.push(ToolSpec {
            name: namespace.clone(),
            description: tool.description,
            schema: tool.input_schema,
        });
        owners.insert(namespace.clone(), client.namespace().to_string());
        out.router.insert(namespace, (client.clone(), tool.name));
    }
}

fn status_for<'a>(statuses: &'a mut Vec<McpServerStatus>, name: &str) -> &'a mut McpServerStatus {
    if let Some(index) = statuses.iter().position(|status| status.name == name) {
        return &mut statuses[index];
    }
    statuses.push(McpServerStatus::configured(name));
    statuses.last_mut().expect("status was just pushed")
}

/// 单个 `@file` 注入的正文上限(超出截断,防爆上下文)。
const MENTION_CAP: usize = 20_000;

/// 展开输入里的 `@path` 引用(像 Claude Code):把每个**存在的**文件正文注入进消息,
/// 让模型直接看到文件内容而不必自己 read_file。不存在的 `@xxx` 原样留着(模型当普通文本看)。
/// ponytail: 路径 = `@` 后一串非空白(去尾部标点);同一路径只注一次;单文件截断到 [`MENTION_CAP`]。
pub fn expand_mentions(input: &str) -> String {
    let mut extra = String::new();
    let mut seen = std::collections::HashSet::new();
    for token in input.split_whitespace() {
        let Some(raw) = token.strip_prefix('@') else {
            continue;
        };
        let path = raw.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '，', '。']);
        if path.is_empty() || !seen.insert(path.to_string()) {
            continue;
        }
        if let Ok(mut content) = std::fs::read_to_string(path) {
            if content.chars().count() > MENTION_CAP {
                content = content.chars().take(MENTION_CAP).collect::<String>() + "\n…(截断)";
            }
            extra.push_str(&format!("\n\n[文件 @{path}]:\n{content}"));
        }
    }
    if extra.is_empty() {
        input.to_string()
    } else {
        format!("{input}{extra}")
    }
}

/// 把任务清单渲染成彩色 checklist(供 REPL 显示进度):完成 `[x]` 绿、进行中 `[~]` 黄、待办 `[ ]`。
/// 空清单 → 空串。
pub fn render_todos(todos: &[Todo]) -> String {
    if todos.is_empty() {
        return String::new();
    }
    let mut s = RichOutput::new()
        .with_color(Color::BrightCyan)
        .bold()
        .format("📋 任务清单:");
    for t in todos {
        let (mark, color) = match t.status.as_str() {
            "completed" => ("[x]", Color::Green),
            "in_progress" => ("[~]", Color::Yellow),
            _ => ("[ ]", Color::White),
        };
        s.push('\n');
        s.push_str(
            &RichOutput::new()
                .with_color(color)
                .format(&format!("  {mark} {}", t.content)),
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::{
        expand_mentions, render_todos, resolve_mcp, resolve_mcp_with_options, McpServerState,
    };
    use crate::exec::{execute_tool_call, parse_todos};
    use crate::needs_approval;
    use mcp::{FnTransport, McpClient, McpError};
    use provider::ToolCall;
    use std::sync::Arc;
    use std::time::Duration;

    /// todo_write:解析 todos + 渲染 checklist + 只读不走权限门。
    #[test]
    fn todo_write_parses_and_renders() {
        let call = ToolCall {
            id: "t".to_string(),
            name: "todo_write".to_string(),
            arguments: serde_json::json!({"todos": [
                {"content": "读代码", "status": "completed"},
                {"content": "改 bug", "status": "in_progress"},
                {"content": "跑测试", "status": "pending"},
            ]}),
        };
        let todos = parse_todos(&call);
        assert_eq!(todos.len(), 3);
        assert_eq!(todos[0].status, "completed");
        assert!(execute_tool_call(&call).contains("3 项"));
        assert!(!needs_approval("todo_write"), "内部清单更新不打扰用户");
        // 渲染:完成打 [x]、进行中 [~]、待办 [ ]。
        let r = render_todos(&todos);
        assert!(
            r.contains("[x] 读代码") && r.contains("[~] 改 bug") && r.contains("[ ] 跑测试"),
            "{r}"
        );
        assert!(render_todos(&[]).is_empty(), "空清单 → 空串");
    }

    /// `@file` 引用:存在的文件注入正文,不存在的原样留着。
    #[test]
    fn expand_mentions_injects_existing_files() {
        let mut path = std::env::temp_dir();
        path.push("ridge_mention_test.txt");
        std::fs::write(&path, "文件正文ABC").unwrap();
        let p = path.to_str().unwrap();
        let out = expand_mentions(&format!("看看 @{p} 说了什么,还有 @/no/such/file"));
        assert!(out.contains("文件正文ABC"), "应注入存在文件: {out}");
        assert!(out.contains(&format!("[文件 @{p}]")), "带来源标注: {out}");
        assert!(out.contains("@/no/such/file"), "不存在的原样留着");
        // 无 @ → 原样返回。
        assert_eq!(expand_mentions("普通输入"), "普通输入");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn resolve_mcp_keeps_runtime_stage_and_redacts_failures() {
        let ready = Arc::new(McpClient::new(
            "ready",
            Box::new(FnTransport(
                move |method: &str, _params: &serde_json::Value| match method {
                    "initialize" => Ok(serde_json::json!({})),
                    "tools/list" => Ok(serde_json::json!({
                        "tools": [{
                            "name": "search",
                            "description": "search",
                            "inputSchema": {"type": "object"}
                        }]
                    })),
                    _ => Ok(serde_json::json!({})),
                },
            )),
        ));
        let init_failed = Arc::new(McpClient::new(
            "init-failed",
            Box::new(FnTransport(|method: &str, _params: &serde_json::Value| {
                if method == "initialize" {
                    Err(McpError::Transport("RIDGE_API_KEY=secret".into()))
                } else {
                    Ok(serde_json::json!({}))
                }
            })),
        ));
        let list_failed = Arc::new(McpClient::new(
            "list-failed",
            Box::new(FnTransport(
                |method: &str, _params: &serde_json::Value| match method {
                    "initialize" => Ok(serde_json::json!({})),
                    "tools/list" => Err(McpError::Rpc {
                        code: -32001,
                        message: "secret should not escape".into(),
                    }),
                    _ => Ok(serde_json::json!({})),
                },
            )),
        ));

        let resolved = resolve_mcp(vec![ready, init_failed, list_failed]).await;
        assert_eq!(resolved.tool_names(), vec!["ready__search"]);

        let ready_status = &resolved.statuses()[0];
        assert_eq!(ready_status.state, McpServerState::ToolsListed);
        assert_eq!(
            ready_status.trail_labels(),
            vec!["configured", "started", "initialized", "tools listed"]
        );

        let init_status = &resolved.statuses()[1];
        assert_eq!(init_status.state, McpServerState::Failed);
        assert_eq!(init_status.detail, "initialize failed: transport error");
        assert!(!init_status.detail.contains("secret"));

        let list_status = &resolved.statuses()[2];
        assert_eq!(list_status.state, McpServerState::Failed);
        assert_eq!(list_status.detail, "tools/list failed: RPC error -32001");
        assert!(!list_status.detail.contains("secret"));
    }

    struct BarrierTransport {
        barrier: Arc<tokio::sync::Barrier>,
        tools: Vec<&'static str>,
    }

    #[async_trait::async_trait]
    impl mcp::McpTransport for BarrierTransport {
        async fn request(
            &self,
            method: &str,
            _params: serde_json::Value,
        ) -> Result<serde_json::Value, McpError> {
            match method {
                "initialize" => {
                    self.barrier.wait().await;
                    Ok(serde_json::json!({}))
                }
                "tools/list" => Ok(serde_json::json!({
                    "tools": self
                        .tools
                        .iter()
                        .map(|name| serde_json::json!({
                            "name": name,
                            "description": name,
                            "inputSchema": {"type": "object"}
                        }))
                        .collect::<Vec<_>>()
                })),
                _ => Ok(serde_json::json!({})),
            }
        }
    }

    struct HangingTransport;

    #[async_trait::async_trait]
    impl mcp::McpTransport for HangingTransport {
        async fn request(
            &self,
            _method: &str,
            _params: serde_json::Value,
        ) -> Result<serde_json::Value, McpError> {
            std::future::pending::<Result<serde_json::Value, McpError>>().await
        }
    }

    #[tokio::test]
    async fn resolve_mcp_runs_startup_in_parallel_and_orders_results() {
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let first = Arc::new(McpClient::new(
            "first",
            Box::new(BarrierTransport {
                barrier: barrier.clone(),
                tools: vec!["zeta", "alpha"],
            }),
        ));
        let second = Arc::new(McpClient::new(
            "second",
            Box::new(BarrierTransport {
                barrier,
                tools: vec!["beta", "alpha"],
            }),
        ));

        let resolved = tokio::time::timeout(
            Duration::from_secs(1),
            resolve_mcp_with_options(
                vec![first, second],
                Vec::new(),
                Duration::from_millis(100),
                2,
            ),
        )
        .await
        .expect("independent MCP startups should make progress together");
        assert_eq!(
            resolved.tool_names(),
            vec![
                "first__alpha",
                "first__zeta",
                "second__alpha",
                "second__beta"
            ]
        );
        assert_eq!(
            resolved
                .statuses()
                .iter()
                .map(|status| status.name.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );
    }

    #[tokio::test]
    async fn namespace_collision_fails_closed_with_deterministic_error() {
        let make = || {
            Arc::new(McpClient::new(
                "same-server",
                Box::new(FnTransport(
                    move |method: &str, _params: &serde_json::Value| match method {
                        "initialize" => Ok(serde_json::json!({})),
                        "tools/list" => Ok(serde_json::json!({
                            "tools": [{"name": "same", "description": "duplicate"}]
                        })),
                        _ => Ok(serde_json::json!({})),
                    },
                )),
            ))
        };
        let resolved = resolve_mcp_with_options(
            vec![make(), make()],
            Vec::new(),
            Duration::from_millis(100),
            2,
        )
        .await;
        assert!(
            resolved.tool_names().is_empty(),
            "collision must not pick a winner"
        );
        assert_eq!(
            resolved.startup_errors(),
            &["same-server__same (same-server, same-server)".to_string()]
        );
        assert!(resolved.statuses().iter().all(|status| {
            status.state == McpServerState::Failed
                && status.detail.contains("same-server__same")
                && status.detail.contains("same-server, same-server")
        }));
    }

    #[tokio::test]
    async fn resolve_mcp_timeout_is_bounded_and_redacted() {
        let client = Arc::new(McpClient::new("hanging", Box::new(HangingTransport)));
        let resolved =
            resolve_mcp_with_options(vec![client], Vec::new(), Duration::from_millis(5), 1).await;
        assert_eq!(resolved.tool_names(), Vec::<String>::new());
        let status = &resolved.statuses()[0];
        assert_eq!(status.state, McpServerState::Failed);
        assert_eq!(status.detail, "initialize timed out after 5ms");
        assert!(status.trail_labels().contains(&"failed"));
    }
}
