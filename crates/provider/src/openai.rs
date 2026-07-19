use super::{
    split_thinking, Completion, CompletionRequest, Message, ProviderError, Role, StreamChunk,
    ToolCall, Usage,
};
use serde_json::{json, Value};

/// 把统一历史铺成 OpenAI `/chat/completions` 请求体(纯函数,离线可测)。
/// 关键:assistant 的工具调用进 `tool_calls`(arguments 序列化成字符串),
/// 工具结果是独立的 `role=tool` 消息且带 `tool_call_id`。
pub fn build_request(model: &str, req: &CompletionRequest) -> Value {
    let messages: Vec<Value> = req.messages.iter().map(message_to_wire).collect();
    let mut body = json!({ "model": model, "messages": messages });
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(
            req.tools
                .iter()
                .map(|t| {
                    json!({"type":"function","function":{
                        "name": t.name, "description": t.description, "parameters": t.schema
                    }})
                })
                .collect(),
        );
    }
    body
}

fn message_to_wire(m: &Message) -> Value {
    match m.role {
        Role::System => json!({"role":"system","content": m.content}),
        Role::User => json!({"role":"user","content": m.content}),
        Role::Tool => {
            json!({"role":"tool","tool_call_id": m.tool_call_id, "content": m.content})
        }
        Role::Assistant if !m.tool_calls.is_empty() => {
            let calls: Vec<Value> = m
                .tool_calls
                .iter()
                .map(|c| {
                    json!({"id": c.id, "type":"function", "function":{
                        "name": c.name, "arguments": c.arguments.to_string()
                    }})
                })
                .collect();
            json!({"role":"assistant","content": m.content, "tool_calls": calls})
        }
        Role::Assistant => json!({"role":"assistant","content": m.content}),
    }
}

/// 从 `/chat/completions` 响应 JSON 抠出文本 + 工具调用。
/// OpenAI 的 `function.arguments` 是 JSON **字符串**,这里解开成对象。
pub fn parse_response(v: &Value) -> Result<Completion, ProviderError> {
    let msg = &v["choices"][0]["message"];
    // 两路分流:content 的 inline `<think>` 剥出,并与独立 `reasoning_content` 字段(GLM 等)合并。
    let (text, reasoning) = split_thinking(
        msg["content"].as_str().unwrap_or(""),
        msg["reasoning_content"].as_str().unwrap_or(""),
    );
    let mut tool_calls = Vec::new();
    if let Some(arr) = msg["tool_calls"].as_array() {
        for tc in arr {
            let args_str = tc["function"]["arguments"].as_str().unwrap_or("{}");
            tool_calls.push(ToolCall {
                id: tc["id"].as_str().unwrap_or("").to_string(),
                name: tc["function"]["name"].as_str().unwrap_or("").to_string(),
                arguments: serde_json::from_str(args_str)
                    .unwrap_or_else(|_| Value::Object(Default::default())),
            });
        }
    }
    let usage = Usage {
        prompt_tokens: v["usage"]["prompt_tokens"].as_u64().unwrap_or(0) as u32,
        completion_tokens: v["usage"]["completion_tokens"].as_u64().unwrap_or(0) as u32,
    };
    Ok(Completion {
        text,
        reasoning,
        tool_calls,
        usage,
    })
}

/// 流式增量的累加器:SSE 每帧的 `delta` 往里叠(文本直接拼,工具调用按 index 拼片段)。
#[derive(Default)]
pub struct StreamAcc {
    pub text: String,
    /// 思考累加(`reasoning_content` 增量),与 `text` 分道,收尾时进 `Completion.reasoning`。
    pub reasoning: String,
    pub tool_calls: Vec<ToolCallAcc>,
    pub usage: Usage,
}

/// 工具调用在流式里是**分片**来的:首帧带 id/name,后续帧只带 arguments 的片段。
#[derive(Default)]
pub struct ToolCallAcc {
    pub id: String,
    pub name: String,
    pub args: String,
}

/// 把一帧 SSE delta 叠进累加器;文本增量顺带回调 `on_token`(供逐字显示)。
/// `on_token` 收 [`StreamChunk`](思考/回答分道),`?Sized` 便于传 `&dyn Fn`。
/// 思考模型(GLM 等)先流 `reasoning_content`、后流 `content` —— 两路都回调,回答恒显、思考灰显。
pub fn accumulate_stream<F: Fn(StreamChunk) + ?Sized>(
    acc: &mut StreamAcc,
    v: &Value,
    on_token: &F,
) {
    let delta = &v["choices"][0]["delta"];
    if let Some(reasoning) = delta["reasoning_content"].as_str() {
        if !reasoning.is_empty() {
            on_token(StreamChunk::Reasoning(reasoning.to_string()));
            acc.reasoning.push_str(reasoning);
        }
    }
    if let Some(content) = delta["content"].as_str() {
        if !content.is_empty() {
            on_token(StreamChunk::Answer(content.to_string()));
            acc.text.push_str(content);
        }
    }
    if let Some(tcs) = delta["tool_calls"].as_array() {
        for tc in tcs {
            let idx = tc["index"].as_u64().unwrap_or(0) as usize;
            while acc.tool_calls.len() <= idx {
                acc.tool_calls.push(ToolCallAcc::default());
            }
            let slot = &mut acc.tool_calls[idx];
            if let Some(id) = tc["id"].as_str() {
                if !id.is_empty() {
                    slot.id = id.to_string();
                }
            }
            if let Some(name) = tc["function"]["name"].as_str() {
                if !name.is_empty() {
                    slot.name = name.to_string();
                }
            }
            if let Some(args) = tc["function"]["arguments"].as_str() {
                slot.args.push_str(args);
            }
        }
    }
    // usage 通常在最后一帧(choices 为空)带回(需请求里开 stream_options.include_usage)。
    if let Some(u) = v.get("usage") {
        if !u.is_null() {
            acc.usage.prompt_tokens = u["prompt_tokens"].as_u64().unwrap_or(0) as u32;
            acc.usage.completion_tokens = u["completion_tokens"].as_u64().unwrap_or(0) as u32;
        }
    }
}

impl StreamAcc {
    /// 收尾:组装成最终 [`Completion`](分出回答/思考、把工具调用 arguments 片段解析回对象)。
    pub fn into_completion(self) -> Completion {
        let tool_calls = self
            .tool_calls
            .into_iter()
            .filter(|t| !t.name.is_empty())
            .map(|t| ToolCall {
                id: t.id,
                name: t.name,
                arguments: serde_json::from_str(&t.args)
                    .unwrap_or_else(|_| Value::Object(Default::default())),
            })
            .collect();
        // content 里若仍漏进 inline `<think>`(流式未走 reasoning_content 的端点),此处剥净并并入思考。
        let (text, reasoning) = split_thinking(&self.text, &self.reasoning);
        Completion {
            text,
            reasoning,
            tool_calls,
            usage: self.usage,
        }
    }
}
