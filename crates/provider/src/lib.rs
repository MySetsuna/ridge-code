//! # provider —— LLM provider 边界
//!
//! 把不同厂商的 wire 格式归一化到一套统一的内部表示,藏在 [`LlmProvider`] trait 后面:
//! - Anthropic:工具调用是 assistant 的 `tool_use` 块(`input` 是对象);
//! - OpenAI:工具调用是 `tool_calls` 数组(`function.arguments` 是 **JSON 字符串**)。
//!
//! 两边都归一化成 [`ToolCall`] { name, arguments(Value) }。wire 解析是**纯函数**,可离线单测,
//! 不烧 key(见 `openai::parse_response` / `anthropic::parse_response`)。真实 HTTP 客户端是
//! 这层之上的一薄层(下一迭代接),trait 与归一化不变。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// provider 层错误统一归一化成 boxed 类型(同 langgraph 的边界原则)。
pub type ProviderError = Box<dyn std::error::Error + Send + Sync>;

/// 消息角色。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// 一条对话消息(统一表示,多轮工具循环用)。
/// - assistant 发起工具调用时,`tool_calls` 非空;
/// - `role=tool` 的工具结果回灌时,`tool_call_id` 指向对应调用。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tool_calls: Vec<ToolCall>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self::new(Role::System, content)
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::new(Role::User, content)
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new(Role::Assistant, content)
    }

    /// assistant 回合发起了工具调用。
    pub fn with_tool_calls(mut self, calls: Vec<ToolCall>) -> Self {
        self.tool_calls = calls;
        self
    }

    /// 工具结果回灌(role=tool),`call_id` 匹配发起它的 tool_call。
    pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
        }
    }
}

/// 暴露给模型的工具规格(名字 + 描述 + JSON Schema)。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

/// 归一化后的一次工具调用。`arguments` 是解析好的对象(OpenAI 的 JSON 字符串已被解开)。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// 一次补全结果:自然语言文本 + 若干工具调用。
#[derive(Clone, Debug, Default)]
pub struct Completion {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
}

/// 一次补全请求:对话历史 + 可用工具。
#[derive(Clone, Debug, Default)]
pub struct CompletionRequest {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
}

/// LLM provider 抽象。真实实现(Anthropic/OpenAI HTTP)与离线 [`ScriptedProvider`] 都藏在这后面。
#[async_trait::async_trait]
pub trait LlmProvider: Send + Sync {
    async fn complete(&self, req: &CompletionRequest) -> Result<Completion, ProviderError>;
}

/// 离线脚本 provider:按顺序吐预设的 [`Completion`],零联网、确定性,用于 demo / 测试。
pub struct ScriptedProvider {
    steps: std::sync::Mutex<std::collections::VecDeque<Completion>>,
}

impl ScriptedProvider {
    pub fn new(steps: Vec<Completion>) -> Self {
        Self {
            steps: std::sync::Mutex::new(steps.into()),
        }
    }
}

#[async_trait::async_trait]
impl LlmProvider for ScriptedProvider {
    async fn complete(&self, _req: &CompletionRequest) -> Result<Completion, ProviderError> {
        Ok(self.steps.lock().unwrap().pop_front().unwrap_or_default())
    }
}

/// OpenAI 兼容响应的归一化(纯函数,离线可测)。
pub mod openai {
    use super::{Completion, CompletionRequest, Message, ProviderError, Role, ToolCall};
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
        let text = msg["content"].as_str().unwrap_or("").to_string();
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
        Ok(Completion { text, tool_calls })
    }
}

/// Anthropic Messages 响应的归一化(纯函数,离线可测)。
pub mod anthropic {
    use super::{Completion, CompletionRequest, ProviderError, Role, ToolCall};
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
                        blocks.push(json!({"type":"tool_use","id": c.id, "name": c.name, "input": c.arguments}));
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
        Ok(Completion { text, tool_calls })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn openai_and_anthropic_normalize_to_same_tool_call() {
        // 两种厂商的 wire 格式,归一化后应得到等价的 ToolCall。
        let openai_wire = json!({
            "choices": [{"message": {
                "content": "let me run it",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "run_shell", "arguments": "{\"cmd\":\"cargo build\"}"}
                }]
            }}]
        });
        let anthropic_wire = json!({
            "content": [
                {"type": "text", "text": "let me run it"},
                {"type": "tool_use", "id": "toolu_1", "name": "run_shell", "input": {"cmd": "cargo build"}}
            ]
        });

        let a = openai::parse_response(&openai_wire).unwrap();
        let b = anthropic::parse_response(&anthropic_wire).unwrap();

        assert_eq!(a.text, "let me run it");
        assert_eq!(b.text, "let me run it");
        assert_eq!(a.tool_calls.len(), 1);
        assert_eq!(a.tool_calls[0].name, "run_shell");
        assert_eq!(a.tool_calls[0].arguments, json!({"cmd": "cargo build"}));
        // 归一化到位:除了 id,两边的工具调用等价。
        assert_eq!(a.tool_calls[0].name, b.tool_calls[0].name);
        assert_eq!(a.tool_calls[0].arguments, b.tool_calls[0].arguments);
    }

    #[test]
    fn plain_text_response_has_no_tool_calls() {
        let wire = json!({"choices": [{"message": {"content": "all done"}}]});
        let c = openai::parse_response(&wire).unwrap();
        assert_eq!(c.text, "all done");
        assert!(c.tool_calls.is_empty());
    }

    /// 多轮工具历史 → OpenAI wire:assistant 带 tool_calls,工具结果是 role=tool + tool_call_id。
    #[test]
    fn openai_build_request_multiturn_tool_loop() {
        let req = CompletionRequest {
            messages: vec![
                Message::system("sys"),
                Message::user("do it"),
                Message::assistant("").with_tool_calls(vec![ToolCall {
                    id: "c1".to_string(),
                    name: "run_shell".to_string(),
                    arguments: json!({"cmd": "cargo build"}),
                }]),
                Message::tool_result("c1", "exit 0"),
            ],
            tools: vec![ToolSpec {
                name: "run_shell".to_string(),
                description: "run".to_string(),
                schema: json!({"type": "object"}),
            }],
        };
        let body = openai::build_request("gpt-x", &req);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[2]["role"], "assistant");
        assert_eq!(msgs[2]["tool_calls"][0]["id"], "c1");
        assert_eq!(msgs[2]["tool_calls"][0]["function"]["name"], "run_shell");
        assert_eq!(msgs[3]["role"], "tool");
        assert_eq!(msgs[3]["tool_call_id"], "c1");
        assert_eq!(msgs[3]["content"], "exit 0");
        assert!(body["tools"].is_array());
    }

    /// 多轮工具历史 → Anthropic wire:system 顶层;tool_use / tool_result 块;相邻 user 合并。
    #[test]
    fn anthropic_build_request_merges_adjacent_and_uses_blocks() {
        let req = CompletionRequest {
            messages: vec![
                Message::system("sys"),
                Message::user("do it"),
                Message::assistant("").with_tool_calls(vec![ToolCall {
                    id: "c1".to_string(),
                    name: "run_shell".to_string(),
                    arguments: json!({"cmd": "cargo build"}),
                }]),
                Message::tool_result("c1", "exit 0"),
                Message::user("and now?"),
            ],
            tools: vec![],
        };
        let body = anthropic::build_request("claude-x", 1024, &req);
        assert_eq!(body["system"], "sys");
        assert_eq!(body["max_tokens"], 1024);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3, "user / assistant / merged-user");
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"][0]["text"], "do it");
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["content"][0]["type"], "tool_use");
        assert_eq!(msgs[1]["content"][0]["id"], "c1");
        // tool_result 与其后的 user 文本被合并进同一条 user 消息(Anthropic 要求角色交替)。
        assert_eq!(msgs[2]["role"], "user");
        assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
        assert_eq!(msgs[2]["content"][0]["tool_use_id"], "c1");
        assert_eq!(msgs[2]["content"][1]["text"], "and now?");
    }
}
