//! provider 抽象 + OpenAI 兼容实现(M0)。
//! 第三方多 provider crate(llm / llm-connector)应包在 LlmProvider trait 后面(见 PLAN.md §6),
//! 绝不让上层直接依赖,以便替换 / 对单个 provider 降级到裸 reqwest。

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use rc_types::{Completion, Message, Role, ToolCall, ToolSpec, Usage};
use serde::{Deserialize, Serialize};

mod stub;
pub use stub::StubProvider;

/// 统一的模型 provider 接口。
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// 给定对话历史与工具,返回模型的一次回复(可能含 tool_calls)。
    async fn complete(&self, messages: &[Message], tools: &[ToolSpec]) -> Result<Completion>;
    fn model_id(&self) -> &str;
}

/// 任何 OpenAI 兼容端点(DeepSeek / GLM / Qwen / 本地 等)。
pub struct OpenAiCompatProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl OpenAiCompatProvider {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            model: model.into(),
        }
    }
}

#[async_trait]
impl LlmProvider for OpenAiCompatProvider {
    async fn complete(&self, messages: &[Message], tools: &[ToolSpec]) -> Result<Completion> {
        let req = ChatRequest {
            model: &self.model,
            messages: messages.iter().map(to_wire_message).collect(),
            tools: tools.iter().map(to_wire_tool).collect(),
        };
        let url = format!("{}/chat/completions", self.base_url);
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&req)
            .send()
            .await
            .with_context(|| format!("请求 {url} 失败"))?;
        let status = resp.status();
        let body = resp.text().await.context("读取响应体失败")?;
        if !status.is_success() {
            bail!("provider 返回 {status}: {body}");
        }
        let parsed: ChatResponse =
            serde_json::from_str(&body).with_context(|| format!("解析响应失败: {body}"))?;
        let choice = parsed.choices.into_iter().next().context("响应无 choices")?;
        let message = Message {
            role: Role::Assistant,
            content: choice.message.content.unwrap_or_default(),
            tool_calls: choice
                .message
                .tool_calls
                .into_iter()
                .map(|tc| ToolCall {
                    id: tc.id,
                    name: tc.function.name,
                    arguments: tc.function.arguments,
                })
                .collect(),
            tool_call_id: None,
        };
        let usage = parsed
            .usage
            .map(|u| Usage {
                input_tokens: u.prompt_tokens,
                output_tokens: u.completion_tokens,
            })
            .unwrap_or_default();
        Ok(Completion { message, usage })
    }

    fn model_id(&self) -> &str {
        &self.model
    }
}

// ---- OpenAI 兼容 wire 格式(私有,不外泄) ----

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<WireMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<WireTool>,
}

#[derive(Serialize)]
struct WireMessage {
    role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<WireToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct WireToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: WireFunctionCall,
}

#[derive(Serialize, Deserialize)]
struct WireFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Serialize)]
struct WireTool {
    #[serde(rename = "type")]
    kind: &'static str,
    function: WireFunctionDef,
}

#[derive(Serialize)]
struct WireFunctionDef {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct Choice {
    message: RespMessage,
}

#[derive(Deserialize)]
struct RespMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<WireToolCall>,
}

#[derive(Deserialize)]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn to_wire_message(m: &Message) -> WireMessage {
    WireMessage {
        role: role_str(m.role),
        // assistant 带 tool_calls 时 content 允许为 null
        content: if m.content.is_empty() && !m.tool_calls.is_empty() {
            None
        } else {
            Some(m.content.clone())
        },
        tool_calls: m
            .tool_calls
            .iter()
            .map(|tc| WireToolCall {
                id: tc.id.clone(),
                kind: "function".to_string(),
                function: WireFunctionCall {
                    name: tc.name.clone(),
                    arguments: tc.arguments.clone(),
                },
            })
            .collect(),
        tool_call_id: m.tool_call_id.clone(),
    }
}

fn to_wire_tool(t: &ToolSpec) -> WireTool {
    WireTool {
        kind: "function",
        function: WireFunctionDef {
            name: t.name.clone(),
            description: t.description.clone(),
            parameters: t.parameters.clone(),
        },
    }
}
