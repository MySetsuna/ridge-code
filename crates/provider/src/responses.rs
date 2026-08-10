use super::{
    Completion, CompletionRequest, ProviderError, Role, StreamChunk, ToolCall, ToolSpec, Usage,
};
use serde_json::{json, Value};
use std::collections::HashMap;

/// Build the Responses API body used by the ChatGPT subscription backend.
pub fn build_request(model: &str, req: &CompletionRequest) -> Value {
    build_request_with_effort(model, req, crate::DEFAULT_REASONING_EFFORT)
}

/// Build a Responses API body with the requested reasoning effort.
pub fn build_request_with_effort(
    model: &str,
    req: &CompletionRequest,
    reasoning_effort: &str,
) -> Value {
    let mut instructions = Vec::new();
    let mut input = Vec::new();

    let messages = crate::repair_tool_history(&req.messages);
    for message in &messages {
        match &message.role {
            Role::System => instructions.push(message.content.clone()),
            Role::User => input.push(json!({
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": message.content}],
            })),
            Role::Assistant => {
                if !message.content.is_empty() {
                    input.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": message.content}],
                    }));
                }
                for call in &message.tool_calls {
                    input.push(json!({
                        "type": "function_call",
                        "call_id": call.id,
                        "name": call.name,
                        "arguments": call.arguments.to_string(),
                    }));
                }
            }
            Role::Tool => {
                if let Some(call_id) = &message.tool_call_id {
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": message.content,
                    }));
                }
            }
        }
    }

    let mut body = json!({
        "model": model,
        "input": input,
        "tool_choice": "auto",
        "parallel_tool_calls": true,
        "reasoning": {"effort": reasoning_effort, "summary": "auto"},
        "store": false,
        "stream": true,
        "include": ["reasoning.encrypted_content"],
    });
    if !instructions.is_empty() {
        body["instructions"] = Value::String(instructions.join("\n"));
    }
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(req.tools.iter().map(tool_to_wire).collect());
    }
    body
}

fn tool_to_wire(tool: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.schema,
    })
}

#[derive(Default)]
pub struct StreamAcc {
    pub text: String,
    fallback_text: String,
    pub reasoning: String,
    tool_calls: Vec<ToolCallAcc>,
    pending_tool_calls: HashMap<String, ToolCallAcc>,
    pub usage: Usage,
    pub completed: bool,
    pub error: Option<String>,
}

#[derive(Default, Clone)]
struct ToolCallAcc {
    id: String,
    call_id: String,
    name: String,
    arguments: String,
}

pub fn accumulate_stream<F: Fn(StreamChunk) + ?Sized>(
    acc: &mut StreamAcc,
    value: &Value,
    on_token: &F,
) {
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match kind {
        "response.output_text.delta" => append_response_text(acc, value, on_token, false),
        "response.reasoning_summary_text.delta"
        | "response.reasoning_text.delta"
        | "response.reasoning_content.delta" => append_response_text(acc, value, on_token, true),
        "response.function_call_arguments.delta" => append_function_delta(acc, value),
        "response.output_item.done" => append_output_item(acc, value),
        "response.completed" => mark_completed(acc, value),
        "response.failed" | "response.incomplete" => record_stream_error(acc, value),
        _ => {}
    }
}

fn append_response_text<F: Fn(StreamChunk) + ?Sized>(
    acc: &mut StreamAcc,
    value: &Value,
    on_token: &F,
    reasoning: bool,
) {
    let Some(delta) = value.get("delta").and_then(Value::as_str) else {
        return;
    };
    let chunk = if reasoning {
        StreamChunk::Reasoning(delta.to_string())
    } else {
        StreamChunk::Answer(delta.to_string())
    };
    on_token(chunk);
    if reasoning {
        acc.reasoning.push_str(delta);
    } else {
        acc.text.push_str(delta);
    }
}

fn append_function_delta(acc: &mut StreamAcc, value: &Value) {
    let Some(key) = value
        .get("call_id")
        .or_else(|| value.get("item_id"))
        .and_then(Value::as_str)
    else {
        return;
    };
    let call = acc
        .pending_tool_calls
        .entry(key.to_string())
        .or_insert_with(|| ToolCallAcc {
            id: value
                .get("item_id")
                .and_then(Value::as_str)
                .unwrap_or(key)
                .to_string(),
            call_id: value
                .get("call_id")
                .and_then(Value::as_str)
                .unwrap_or(key)
                .to_string(),
            ..Default::default()
        });
    if let Some(delta) = value.get("delta").and_then(Value::as_str) {
        call.arguments.push_str(delta);
    }
}

fn append_output_item(acc: &mut StreamAcc, value: &Value) {
    let Some(item) = value.get("item") else {
        return;
    };
    match item.get("type").and_then(Value::as_str) {
        Some("function_call") => append_function_item(acc, item),
        Some("message") => append_message_item(acc, item),
        _ => {}
    }
}

fn append_function_item(acc: &mut StreamAcc, item: &Value) {
    let call_id = item
        .get("call_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let item_id = item
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let key = if !call_id.is_empty() {
        &call_id
    } else {
        &item_id
    };
    let mut call = acc.pending_tool_calls.remove(key).unwrap_or_default();
    call.id = item_id;
    call.call_id = call_id;
    call.name = item
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if let Some(arguments) = item.get("arguments").and_then(Value::as_str) {
        call.arguments = arguments.to_string();
    }
    if !call.name.is_empty() {
        upsert_tool_call(&mut acc.tool_calls, call);
    }
}

fn append_message_item(acc: &mut StreamAcc, item: &Value) {
    let Some(content) = item.get("content").and_then(Value::as_array) else {
        return;
    };
    for part in content {
        if let Some(text) = part
            .get("text")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            acc.fallback_text.push_str(text);
        }
    }
}

fn mark_completed(acc: &mut StreamAcc, value: &Value) {
    acc.completed = true;
    let Some(usage) = value
        .get("response")
        .and_then(|response| response.get("usage"))
    else {
        return;
    };
    acc.usage.prompt_tokens = usage["input_tokens"].as_u64().unwrap_or(0) as u32;
    acc.usage.completion_tokens = usage["output_tokens"].as_u64().unwrap_or(0) as u32;
}

fn record_stream_error(acc: &mut StreamAcc, value: &Value) {
    let message = value
        .pointer("/response/error/message")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .pointer("/response/incomplete_details/reason")
                .and_then(Value::as_str)
        })
        .unwrap_or("Responses API request failed");
    acc.error = Some(message.to_string());
}

fn upsert_tool_call(calls: &mut Vec<ToolCallAcc>, call: ToolCallAcc) {
    if let Some(existing) = calls
        .iter_mut()
        .find(|existing| !call.call_id.is_empty() && existing.call_id == call.call_id)
    {
        *existing = call;
    } else {
        calls.push(call);
    }
}

impl StreamAcc {
    pub fn into_completion(mut self) -> Completion {
        for call in self.pending_tool_calls.into_values() {
            if !call.name.is_empty() {
                upsert_tool_call(&mut self.tool_calls, call);
            }
        }
        let tool_calls = self
            .tool_calls
            .into_iter()
            .filter(|call| !call.name.is_empty())
            .map(|call| ToolCall {
                id: if call.call_id.is_empty() {
                    call.id
                } else {
                    call.call_id
                },
                name: call.name,
                arguments: serde_json::from_str(&call.arguments)
                    .unwrap_or_else(|_| Value::Object(Default::default())),
            })
            .collect();
        if self.text.is_empty() {
            self.text = self.fallback_text;
        }
        Completion {
            text: self.text,
            reasoning: self.reasoning,
            tool_calls,
            usage: self.usage,
        }
    }
}

pub fn stream_error(acc: &StreamAcc) -> Option<ProviderError> {
    acc.error
        .as_ref()
        .map(|message| format!("ChatGPT Responses API: {message}").into())
}
