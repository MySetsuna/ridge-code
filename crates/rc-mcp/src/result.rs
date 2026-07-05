//! 把 MCP 工具返回(`CallToolResult`)渲染成给模型看的文本。
//! 与内置工具 `rc_tools::dispatch` 的文本风格一致:失败前缀 `ERROR:`,让模型能自我纠正。

use rmcp::model::CallToolResult;

/// 渲染工具结果为纯文本回灌给模型。
pub(crate) fn render_call_result(result: &CallToolResult) -> String {
    let text = collect_text(result);
    if result.is_error == Some(true) {
        format!("ERROR: {text}")
    } else {
        text
    }
}

/// 取文本内容块拼接;为空则退回 structured_content 的 JSON;仍空则占位说明。
/// (图片/资源等非文本块先忽略——编码 Worker 只消费文本。)
fn collect_text(result: &CallToolResult) -> String {
    let parts: Vec<&str> = result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|t| t.text.as_str()))
        .collect();
    if !parts.is_empty() {
        return parts.join("\n");
    }
    if let Some(sc) = &result.structured_content {
        return sc.to_string();
    }
    "(工具无文本输出)".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn result_from(v: serde_json::Value) -> CallToolResult {
        serde_json::from_value(v).expect("反序列化 CallToolResult")
    }

    #[test]
    fn renders_text_blocks_joined() {
        let r = result_from(json!({
            "content": [
                {"type":"text","text":"line1"},
                {"type":"text","text":"line2"}
            ]
        }));
        assert_eq!(render_call_result(&r), "line1\nline2");
    }

    #[test]
    fn error_result_prefixes_error() {
        let r = result_from(json!({
            "content": [{"type":"text","text":"boom"}],
            "isError": true
        }));
        assert_eq!(render_call_result(&r), "ERROR: boom");
    }

    #[test]
    fn empty_content_notes_no_output() {
        let r = result_from(json!({ "content": [] }));
        assert_eq!(render_call_result(&r), "(工具无文本输出)");
    }
}
