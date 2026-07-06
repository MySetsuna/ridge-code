//! 原生 Anthropic Messages API 的 `LlmProvider` 实现。
//! 落地 PLAN.md §6「一个 Anthropic + 一个 OpenAI 兼容」的前者。
//! wire 类型私有,内部把 rc-types 内部表示 ↔ Anthropic 格式互转:
//!   - system 是顶层参数(内部 Role::System 抽出);
//!   - tool 结果是 user 消息里的 `tool_result` 块(内部 Role::Tool);
//!   - 工具调用是 assistant 的 `tool_use` 块(input 为 JSON 对象);
//!   - 相邻同角色消息合并(Anthropic 要求角色交替)。

use crate::LlmProvider;
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use rc_types::{Completion, Message, Role, ToolCall, ToolSpec, Usage};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 8192;

/// 原生 Anthropic provider(非流式,与 OpenAI 实现对齐)。
pub struct AnthropicProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    max_tokens: u32,
}

impl AnthropicProvider {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
        max_tokens: Option<u32>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            model: model.into(),
            max_tokens: max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        }
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    async fn complete(&self, messages: &[Message], tools: &[ToolSpec]) -> Result<Completion> {
        let (system, rest) = split_system(messages);
        let req = AnthropicRequest {
            model: &self.model,
            max_tokens: self.max_tokens,
            system,
            messages: to_req_messages(&rest),
            tools: tools.iter().map(to_req_tool).collect(),
        };
        let url = format!("{}/messages", self.base_url);
        let resp = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&req)
            .send()
            .await
            .with_context(|| format!("请求 {url} 失败"))?;
        let status = resp.status();
        let body = resp.text().await.context("读取响应体失败")?;
        if !status.is_success() {
            bail!("Anthropic 返回 {status}: {body}");
        }
        let parsed: AnthropicResponse =
            serde_json::from_str(&body).with_context(|| format!("解析响应失败: {body}"))?;
        Ok(parse_response(parsed))
    }

    fn model_id(&self) -> &str {
        &self.model
    }
}

// ---- 纯翻译(私有,离线可测) ----

/// 抽出所有 system 消息拼成顶层 system 字符串,返回其余消息。
fn split_system(messages: &[Message]) -> (Option<String>, Vec<&Message>) {
    let mut sys_parts: Vec<&str> = Vec::new();
    let mut rest: Vec<&Message> = Vec::new();
    for m in messages {
        if m.role == Role::System {
            if !m.content.is_empty() {
                sys_parts.push(&m.content);
            }
        } else {
            rest.push(m);
        }
    }
    let system = if sys_parts.is_empty() {
        None
    } else {
        Some(sys_parts.join("\n\n"))
    };
    (system, rest)
}

/// 内部消息 → Anthropic 请求消息,并合并相邻同角色(保证角色交替)。
fn to_req_messages(messages: &[&Message]) -> Vec<ReqMessage> {
    let mut out: Vec<ReqMessage> = Vec::new();
    for m in messages {
        let (role, blocks) = to_role_blocks(m);
        if blocks.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.role == role => last.content.extend(blocks),
            _ => out.push(ReqMessage {
                role,
                content: blocks,
            }),
        }
    }
    out
}

/// 单条内部消息 → (Anthropic 角色, 内容块)。
fn to_role_blocks(m: &Message) -> (&'static str, Vec<ReqBlock>) {
    match m.role {
        Role::User => {
            let mut blocks = Vec::new();
            if !m.content.is_empty() {
                blocks.push(ReqBlock::Text {
                    text: m.content.clone(),
                });
            }
            ("user", blocks)
        }
        Role::Assistant => {
            let mut blocks = Vec::new();
            if !m.content.is_empty() {
                blocks.push(ReqBlock::Text {
                    text: m.content.clone(),
                });
            }
            for tc in &m.tool_calls {
                blocks.push(ReqBlock::ToolUse {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    input: parse_input(&tc.arguments),
                });
            }
            ("assistant", blocks)
        }
        // 工具结果在 Anthropic 里是 user 消息的 tool_result 块。
        Role::Tool => {
            let block = ReqBlock::ToolResult {
                tool_use_id: m.tool_call_id.clone().unwrap_or_default(),
                content: m.content.clone(),
            };
            ("user", vec![block])
        }
        // system 已在 split_system 抽走,不会到这。
        Role::System => ("user", Vec::new()),
    }
}

/// 工具参数(JSON 字符串)→ Anthropic 的 input 对象;非 object 则空对象。
fn parse_input(arguments: &str) -> Value {
    serde_json::from_str::<Value>(arguments)
        .ok()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

fn to_req_tool(t: &ToolSpec) -> ReqTool {
    ReqTool {
        name: t.name.clone(),
        description: t.description.clone(),
        input_schema: t.parameters.clone(),
    }
}

/// Anthropic 响应 → 内部 Completion(text 拼内容、tool_use 转 ToolCall)。
fn parse_response(resp: AnthropicResponse) -> Completion {
    let mut content = String::new();
    let mut tool_calls = Vec::new();
    for block in resp.content {
        match block {
            RespBlock::Text { text } => content.push_str(&text),
            RespBlock::ToolUse { id, name, input } => tool_calls.push(ToolCall {
                id,
                name,
                arguments: input.to_string(),
            }),
            RespBlock::Other => {}
        }
    }
    let usage = resp
        .usage
        .map(|u| Usage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
        })
        .unwrap_or_default();
    let message = Message {
        role: Role::Assistant,
        content,
        tool_calls,
        tool_call_id: None,
    };
    Completion { message, usage }
}

// ---- Anthropic wire 格式(私有,不外泄) ----

#[derive(Serialize)]
struct AnthropicRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<ReqMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ReqTool>,
}

#[derive(Serialize)]
struct ReqMessage {
    role: &'static str,
    content: Vec<ReqBlock>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ReqBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

#[derive(Serialize)]
struct ReqTool {
    name: String,
    description: String,
    input_schema: Value,
}

#[derive(Deserialize)]
struct AnthropicResponse {
    #[serde(default)]
    content: Vec<RespBlock>,
    #[serde(default)]
    usage: Option<RespUsage>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RespBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    /// 忽略 thinking / 其它块类型。
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct RespUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant_with_tool(id: &str, name: &str, args: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: vec![ToolCall {
                id: id.into(),
                name: name.into(),
                arguments: args.into(),
            }],
            tool_call_id: None,
        }
    }

    #[test]
    fn split_system_lifts_system_out() {
        let msgs = [Message::system("你是助手"), Message::user("干活")];
        let (system, rest) = split_system(&msgs);
        assert_eq!(system.as_deref(), Some("你是助手"));
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].role, Role::User);
    }

    #[test]
    fn tool_result_becomes_user_block_and_merges() {
        // assistant(1 tool_use) 后跟 2 条 tool_result → 应合并成 1 条 user(2 个 tool_result 块)。
        let msgs = [
            assistant_with_tool("t1", "read_file", r#"{"path":"a"}"#),
            Message::tool_result("t1", "内容A"),
            Message::tool_result("t1", "内容B"),
        ];
        let refs: Vec<&Message> = msgs.iter().collect();
        let req = to_req_messages(&refs);
        // assistant 一条 + user 一条(合并后)。
        assert_eq!(req.len(), 2);
        assert_eq!(req[0].role, "assistant");
        assert_eq!(req[1].role, "user");
        assert_eq!(req[1].content.len(), 2);
        // 序列化后确认是 tool_result 块。
        let v = serde_json::to_value(&req[1].content).unwrap();
        assert_eq!(v[0]["type"], "tool_result");
        assert_eq!(v[0]["tool_use_id"], "t1");
        assert_eq!(v[1]["content"], "内容B");
    }

    #[test]
    fn assistant_tool_use_serializes_input_object() {
        let msgs = [assistant_with_tool(
            "t1",
            "write_file",
            r#"{"path":"a","content":"x"}"#,
        )];
        let refs: Vec<&Message> = msgs.iter().collect();
        let req = to_req_messages(&refs);
        let v = serde_json::to_value(&req[0].content).unwrap();
        assert_eq!(v[0]["type"], "tool_use");
        assert_eq!(v[0]["name"], "write_file");
        assert_eq!(v[0]["input"]["path"], "a"); // input 是对象,不是字符串
    }

    #[test]
    fn parse_response_extracts_text_and_tool_use() {
        let resp: AnthropicResponse = serde_json::from_value(json!({
            "content": [
                {"type":"text","text":"好的"},
                {"type":"tool_use","id":"t9","name":"list_dir","input":{"path":"."}},
                {"type":"thinking","thinking":"..."}
            ],
            "usage": {"input_tokens": 12, "output_tokens": 7}
        }))
        .unwrap();
        let completion = parse_response(resp);
        assert_eq!(completion.message.content, "好的");
        assert_eq!(completion.message.tool_calls.len(), 1);
        assert_eq!(completion.message.tool_calls[0].name, "list_dir");
        assert_eq!(
            completion.message.tool_calls[0].arguments,
            r#"{"path":"."}"#
        );
        assert_eq!(completion.usage.input_tokens, 12);
        assert_eq!(completion.usage.output_tokens, 7);
    }
}
