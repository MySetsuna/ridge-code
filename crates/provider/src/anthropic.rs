use super::{strip_thinking, Completion, CompletionRequest, ProviderError, Role, ToolCall, Usage};
use serde_json::{json, Value};

/// 把统一历史铺成 Anthropic `/messages` 请求体(纯函数,离线可测)。
/// 三处关键差异:system 抽成顶层参数;工具调用是 assistant 的 `tool_use` 块;
/// 工具结果是 **user** 消息里的 `tool_result` 块;且**合并相邻同角色**(Anthropic 要求角色交替)。
pub fn build_request(model: &str, max_tokens: u32, req: &CompletionRequest) -> Value {
    let system = req
        .messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    let mut msgs: Vec<Value> = Vec::new();
    for m in req.messages.iter().filter(|m| m.role != Role::System) {
        let (role, blocks) = match m.role {
            Role::User => ("user", vec![json!({"type":"text","text": m.content})]),
            Role::Tool => (
                "user",
                vec![
                    json!({"type":"tool_result","tool_use_id": m.tool_call_id, "content": m.content}),
                ],
            ),
            Role::Assistant => {
                let mut blocks = Vec::new();
                if !m.content.is_empty() {
                    blocks.push(json!({"type":"text","text": m.content}));
                }
                for c in &m.tool_calls {
                    blocks.push(
                        json!({"type":"tool_use","id": c.id, "name": c.name, "input": c.arguments}),
                    );
                }
                ("assistant", blocks)
            }
            Role::System => unreachable!("system 已在上面滤除"),
        };

        // 合并相邻同角色:把新块追加进上一条同角色消息的 content 数组。
        match msgs.last_mut() {
            Some(last) if last["role"] == role => {
                let arr = last["content"].as_array_mut().unwrap();
                arr.extend(blocks);
            }
            _ => msgs.push(json!({"role": role, "content": blocks})),
        }
    }

    let mut body = json!({ "model": model, "max_tokens": max_tokens, "messages": msgs });
    if !system.is_empty() {
        body["system"] = json!(system);
    }
    body
}

/// 从 `/messages` 响应 JSON 抠出文本 + 工具调用。
/// Anthropic 的 `content` 是块数组:`text` 块拼成文本,`tool_use` 块变 ToolCall(`input` 已是对象)。
pub fn parse_response(v: &Value) -> Result<Completion, ProviderError> {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    if let Some(arr) = v["content"].as_array() {
        for block in arr {
            match block["type"].as_str() {
                Some("text") => text.push_str(block["text"].as_str().unwrap_or("")),
                Some("tool_use") => tool_calls.push(ToolCall {
                    id: block["id"].as_str().unwrap_or("").to_string(),
                    name: block["name"].as_str().unwrap_or("").to_string(),
                    arguments: block["input"].clone(),
                }),
                _ => {}
            }
        }
    }
    let usage = Usage {
        prompt_tokens: v["usage"]["input_tokens"].as_u64().unwrap_or(0) as u32,
        completion_tokens: v["usage"]["output_tokens"].as_u64().unwrap_or(0) as u32,
    };
    Ok(Completion {
        text: strip_thinking(&text),
        tool_calls,
        usage,
    })
}
