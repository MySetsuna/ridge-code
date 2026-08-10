use super::{
    split_thinking, Completion, CompletionRequest, Message, ProviderError, Role, StreamChunk,
    ToolCall, Usage,
};
use serde_json::{json, Value};

/// 把统一历史铺成 OpenAI `/chat/completions` 请求体(纯函数,离线可测)。
/// 关键:assistant 的工具调用进 `tool_calls`(arguments 序列化成字符串),
/// 工具结果是独立的 `role=tool` 消息且带 `tool_call_id`。
pub fn build_request(model: &str, req: &CompletionRequest) -> Value {
    let repaired = crate::repair_tool_history(&req.messages);
    let messages: Vec<Value> = repaired.iter().map(message_to_wire).collect();
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
    let (mut text, reasoning) = split_thinking(
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
    // 弱模型兜底:结构化 tool_calls 空 → 从文本捞 `<tool_call>` 块救回(见 salvage_text_tool_calls)。
    if tool_calls.is_empty() {
        let (salvaged, cleaned) = salvage_text_tool_calls(&text);
        if !salvaged.is_empty() {
            tool_calls = salvaged;
            text = cleaned;
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

/// 弱模型(如 GLM-4.5-air)常把工具调用当**纯文本**吐进 `content`,而非结构化 `tool_calls`
/// 数组 —— 引擎遂当终答、回合空转即停(实测病灶)。此处兜底:**仅当结构化 tool_calls 为空**时,
/// 从文本捞 `<tool_call>…</tool_call>` 块救回,并返回**剥掉这些块后**的纯文本(散文原样保留)。
/// 认两种内体:①整块 JSON `{"name":…,"arguments":{…}|"…"}`;②GLM 文本
/// `名字<arg_key>k</arg_key><arg_value>v</arg_value>…`。纯函数,离线可测。
/// ponytail: 只覆盖这两种最常见文本格式;冒出别的再扩,勿预造万能解析器。
pub fn salvage_text_tool_calls(text: &str) -> (Vec<ToolCall>, String) {
    const OPEN: &str = "<tool_call>";
    const CLOSE: &str = "</tool_call>";
    let mut calls = Vec::new();
    let mut cleaned = String::new();
    let mut rest = text;
    loop {
        let Some(start) = rest.find(OPEN) else {
            cleaned.push_str(rest);
            break;
        };
        cleaned.push_str(&rest[..start]);
        let after = &rest[start + OPEN.len()..];
        let Some(end) = after.find(CLOSE) else {
            // 无闭合标签 → 不是完整调用,原样留着,停。
            cleaned.push_str(&rest[start..]);
            break;
        };
        match parse_one_text_tool_call(after[..end].trim(), calls.len()) {
            Some(call) => calls.push(call),
            // 认不出 → 原样保留整块,不吞用户文本。
            None => cleaned.push_str(&rest[start..start + OPEN.len() + end + CLOSE.len()]),
        }
        rest = &after[end + CLOSE.len()..];
    }
    (calls, cleaned.trim().to_string())
}

/// 解析单个 `<tool_call>` 块的内体成一次 [`ToolCall`]。认不出 → None。
fn parse_one_text_tool_call(inner: &str, idx: usize) -> Option<ToolCall> {
    let id = format!("salvaged-{idx}");
    // ① 整块 JSON:{"name": "...", "arguments": {…}|"…"}
    if let Some(call) = parse_json_tool_call(inner, &id) {
        return Some(call);
    }
    // ② GLM 文本格式:工具名在首个 `<` 之前;其后成对 <arg_key>k</arg_key> <arg_value>v</arg_value>。
    let name_end = inner.find('<').unwrap_or(inner.len());
    let name = inner[..name_end].trim();
    if name.is_empty() || name.starts_with('{') {
        return None;
    }
    let mut args = serde_json::Map::new();
    let mut cursor = &inner[name_end..];
    while let Some(k) = extract_between(&mut cursor, "<arg_key>", "</arg_key>") {
        let v = extract_between(&mut cursor, "<arg_value>", "</arg_value>").unwrap_or("");
        args.insert(k.trim().to_string(), parse_scalar(v.trim()));
    }
    Some(ToolCall {
        id,
        name: name.to_string(),
        arguments: Value::Object(args),
    })
}

fn parse_json_tool_call(inner: &str, id: &str) -> Option<ToolCall> {
    let value = serde_json::from_str::<Value>(inner).ok()?;
    let name = value.get("name")?.as_str()?;
    let arguments = match value.get("arguments") {
        Some(Value::String(s)) => {
            serde_json::from_str(s).unwrap_or_else(|_| Value::Object(Default::default()))
        }
        Some(other) => other.clone(),
        None => Value::Object(Default::default()),
    };
    Some(ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments,
    })
}

/// `<arg_value>` 文本 → JSON:先试当 JSON 解析(数字/布尔/对象/数组),失败则原样作字符串。
/// 路径/正则(含 `\`、`.*` 等)不是合法 JSON,自然落回字符串,符合大多数工具 schema。
fn parse_scalar(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.to_string()))
}

/// 从 `*hay` 取 `open`…`close` 之间首个片段,并把游标推进到 `close` 之后。
fn extract_between<'a>(hay: &mut &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = hay.find(open)? + open.len();
    let end_rel = hay[start..].find(close)?;
    let val = &hay[start..start + end_rel];
    *hay = &hay[start + end_rel + close.len()..];
    Some(val)
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
    append_text_delta(
        acc,
        delta,
        "reasoning_content",
        StreamChunk::Reasoning,
        on_token,
    );
    append_text_delta(acc, delta, "content", StreamChunk::Answer, on_token);
    append_tool_deltas(acc, delta);
    // usage 通常在最后一帧(choices 为空)带回(需请求里开 stream_options.include_usage)。
    update_usage(acc, v.get("usage"));
}

fn append_text_delta<F, C>(acc: &mut StreamAcc, delta: &Value, field: &str, chunk: C, on_token: &F)
where
    F: Fn(StreamChunk) + ?Sized,
    C: Fn(String) -> StreamChunk,
{
    let Some(text) = delta[field].as_str().filter(|text| !text.is_empty()) else {
        return;
    };
    on_token(chunk(text.to_string()));
    if field == "content" {
        acc.text.push_str(text);
    } else {
        acc.reasoning.push_str(text);
    }
}

fn append_tool_deltas(acc: &mut StreamAcc, delta: &Value) {
    let Some(tool_calls) = delta["tool_calls"].as_array() else {
        return;
    };
    for tool_call in tool_calls {
        append_tool_delta(acc, tool_call);
    }
}

fn append_tool_delta(acc: &mut StreamAcc, tool_call: &Value) {
    let index = tool_call["index"].as_u64().unwrap_or(0) as usize;
    while acc.tool_calls.len() <= index {
        acc.tool_calls.push(ToolCallAcc::default());
    }
    let slot = &mut acc.tool_calls[index];
    copy_non_empty(&mut slot.id, tool_call["id"].as_str());
    copy_non_empty(&mut slot.name, tool_call["function"]["name"].as_str());
    if let Some(arguments) = tool_call["function"]["arguments"].as_str() {
        slot.args.push_str(arguments);
    }
}

fn copy_non_empty(target: &mut String, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        *target = value.to_string();
    }
}

fn update_usage(acc: &mut StreamAcc, usage: Option<&Value>) {
    let Some(usage) = usage.filter(|usage| !usage.is_null()) else {
        return;
    };
    acc.usage.prompt_tokens = usage["prompt_tokens"].as_u64().unwrap_or(0) as u32;
    acc.usage.completion_tokens = usage["completion_tokens"].as_u64().unwrap_or(0) as u32;
}

impl StreamAcc {
    /// 收尾:组装成最终 [`Completion`](分出回答/思考、把工具调用 arguments 片段解析回对象)。
    pub fn into_completion(self) -> Completion {
        let mut tool_calls: Vec<ToolCall> = self
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
        let (mut text, reasoning) = split_thinking(&self.text, &self.reasoning);
        // 弱模型兜底:结构化 tool_calls 空 → 从文本捞 `<tool_call>` 块救回(见 salvage_text_tool_calls)。
        if tool_calls.is_empty() {
            let (salvaged, cleaned) = salvage_text_tool_calls(&text);
            if !salvaged.is_empty() {
                tool_calls = salvaged;
                text = cleaned;
            }
        }
        Completion {
            text,
            reasoning,
            tool_calls,
            usage: self.usage,
        }
    }
}

#[cfg(test)]
mod salvage_tests {
    use super::{parse_response, salvage_text_tool_calls};
    use serde_json::json;

    /// 实测病灶格式:GLM 把调用当纯文本吐(`<arg_key>`/`<arg_value>`),散文在前。
    #[test]
    fn glm_text_tool_call_is_salvaged_and_prose_kept() {
        let text = "我来梳理下。\n<tool_call>search\n\
            <arg_key>path</arg_key>\n<arg_value>C:\\code\\ridge-code\\crates\\agent\\src\\tui</arg_value>\n\
            <arg_key>pattern</arg_key>\n<arg_value>Ui.*struct</arg_value>\n</tool_call>";
        let (calls, cleaned) = salvage_text_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "search");
        assert_eq!(
            calls[0].arguments["path"],
            "C:\\code\\ridge-code\\crates\\agent\\src\\tui"
        );
        assert_eq!(calls[0].arguments["pattern"], "Ui.*struct");
        assert_eq!(cleaned, "我来梳理下。"); // `<tool_call>` 块剥除,散文保留
    }

    /// 另一常见文本格式:整块 JSON。
    #[test]
    fn json_inside_tool_call_is_salvaged() {
        let text = r#"<tool_call>{"name": "read_file", "arguments": {"path": "a.rs"}}</tool_call>"#;
        let (calls, _) = salvage_text_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments["path"], "a.rs");
    }

    /// 无工具调用的普通回答:原样返回,不误伤。
    #[test]
    fn plain_text_without_tool_call_untouched() {
        let (calls, cleaned) = salvage_text_tool_calls("普通回答,无工具调用");
        assert!(calls.is_empty());
        assert_eq!(cleaned, "普通回答,无工具调用");
    }

    /// 结构化 tool_calls 在场时,parse_response 用它,不触发文本兜底。
    #[test]
    fn structured_tool_calls_bypass_salvage() {
        let v = json!({"choices":[{"message":{
            "content": "<tool_call>search\n<arg_key>path</arg_key>\n<arg_value>x</arg_value>\n</tool_call>",
            "tool_calls": [{"id":"c1","function":{"name":"read_file","arguments":"{\"path\":\"real.rs\"}"}}]
        }}]});
        let c = parse_response(&v).unwrap();
        assert_eq!(c.tool_calls.len(), 1);
        assert_eq!(c.tool_calls[0].name, "read_file"); // 用结构化,非文本里的 search
    }

    /// 结构化为空时,parse_response 从文本救回。
    #[test]
    fn parse_response_salvages_when_structured_empty() {
        let v = json!({"choices":[{"message":{
            "content": "<tool_call>search\n<arg_key>path</arg_key>\n<arg_value>x</arg_value>\n</tool_call>"
        }}]});
        let c = parse_response(&v).unwrap();
        assert_eq!(c.tool_calls.len(), 1);
        assert_eq!(c.tool_calls[0].name, "search");
        assert_eq!(c.tool_calls[0].arguments["path"], "x");
    }
}
