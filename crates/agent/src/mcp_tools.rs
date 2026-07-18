use crate::rich_output::*;
use crate::state::*;
use mcp::McpClient;
use provider::ToolSpec;
use std::collections::HashMap;
use std::sync::Arc;

/// 已连好的 MCP 工具:暴露给 LLM 的 [`ToolSpec`] + 「命名空间名 → (客户端, 原始工具名)」路由表。
#[derive(Default)]
pub struct McpTools {
    pub(crate) specs: Vec<ToolSpec>,
    pub(crate) router: HashMap<String, (Arc<McpClient>, String)>,
}

impl McpTools {
    pub fn empty() -> Self {
        Self::default()
    }

    /// 已接入的 MCP 工具名(命名空间形式,如 `nlm__notebook_list`)。供 `/tools` 列举。
    pub fn tool_names(&self) -> Vec<String> {
        self.specs.iter().map(|s| s.name.clone()).collect()
    }
}

/// 连上一批 MCP 客户端:各自 initialize + list_tools,把工具归一化成 [`ToolSpec`](命名空间)+ 建路由表。
/// **降级不崩**:单个服务器连不上/列不出工具 → 跳过,其余照常。
pub async fn resolve_mcp(clients: Vec<Arc<McpClient>>) -> McpTools {
    let mut out = McpTools::empty();
    for client in clients {
        if client.initialize().await.is_err() {
            continue;
        }
        let Ok(tools) = client.list_tools().await else {
            continue;
        };
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
    use super::*;
    use crate::*;
    use provider::ToolCall;

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
}
