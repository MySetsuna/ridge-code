use crate::rich_output::{Color, RichOutput};
use crate::state::Todo;
use mcp::{McpClient, McpError};
use provider::ToolSpec;
use std::collections::HashMap;
use std::sync::Arc;

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
    match error {
        McpError::Transport(_) => "transport error".to_string(),
        McpError::Rpc { code, .. } => format!("RPC error {code}"),
        McpError::BadResponse(_) => "invalid MCP response".to_string(),
    }
}

/// 已连好的 MCP 工具:暴露给 LLM 的 [`ToolSpec`] + 「命名空间名 → (客户端, 原始工具名)」路由表。
#[derive(Default)]
pub struct McpTools {
    pub(crate) specs: Vec<ToolSpec>,
    pub(crate) router: HashMap<String, (Arc<McpClient>, String)>,
    statuses: Vec<McpServerStatus>,
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
}

/// 连上一批 MCP 客户端:各自 initialize + list_tools,把工具归一化成 [`ToolSpec`](命名空间)+ 建路由表。
/// **降级不崩**:单个服务器连不上/列不出工具 → 跳过,其余照常。
pub async fn resolve_mcp(clients: Vec<Arc<McpClient>>) -> McpTools {
    resolve_mcp_with_statuses(clients, Vec::new()).await
}

/// 与 [`resolve_mcp`] 相同，但接收启动阶段已记录的 configured/failed 状态。
pub async fn resolve_mcp_with_statuses(
    clients: Vec<Arc<McpClient>>,
    mut statuses: Vec<McpServerStatus>,
) -> McpTools {
    let mut out = McpTools {
        statuses: std::mem::take(&mut statuses),
        ..McpTools::empty()
    };
    for client in clients {
        let name = client.namespace().to_string();
        status_for(&mut out.statuses, &name).started();
        if let Err(error) = client.initialize().await {
            status_for(&mut out.statuses, &name)
                .failed(format!("initialize failed: {}", mcp_error_summary(&error)));
            continue;
        }
        status_for(&mut out.statuses, &name).initialized();
        let tools = match client.list_tools().await {
            Ok(tools) => tools,
            Err(error) => {
                status_for(&mut out.statuses, &name)
                    .failed(format!("tools/list failed: {}", mcp_error_summary(&error)));
                continue;
            }
        };
        status_for(&mut out.statuses, &name).tools_listed(tools.len());
        for t in tools {
            let ns = client.namespaced(&t.name);
            out.specs.push(ToolSpec {
                name: ns.clone(),
                description: t.description,
                schema: t.input_schema,
            });
            out.router.insert(ns, (client.clone(), t.name));
        }
    }
    out
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
    use super::{expand_mentions, render_todos, resolve_mcp, McpServerState};
    use crate::exec::{execute_tool_call, parse_todos};
    use crate::needs_approval;
    use mcp::{FnTransport, McpClient, McpError};
    use provider::ToolCall;
    use std::sync::Arc;

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
                |method: &str, _params: &serde_json::Value| match method {
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
}
