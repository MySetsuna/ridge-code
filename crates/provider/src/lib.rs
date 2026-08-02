//! # provider —— LLM provider 边界
//!
//! 把不同厂商的 wire 格式归一化到一套统一的内部表示,藏在 [`LlmProvider`] trait 后面:
//! - Anthropic:工具调用是 assistant 的 `tool_use` 块(`input` 是对象);
//! - OpenAI:工具调用是 `tool_calls` 数组(`function.arguments` 是 **JSON 字符串**)。
//!
//! 两边都归一化成 [`ToolCall`] { name, arguments(Value) }。wire 解析/构建是**纯函数**,可离线单测,
//! 不烧 key(见 `openai` / `anthropic` 模块)。真实 HTTP 客户端([`OpenAiProvider`]/[`AnthropicProvider`])
//! 是这层之上的一薄层,把传输([`http::HttpClient`])与归一化解耦,测试用捕获替身零联网校验。

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

/// 一次调用的 token 用量(成本记账用)。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

impl Usage {
    pub fn total(&self) -> u32 {
        self.prompt_tokens + self.completion_tokens
    }
}

/// 一次补全结果:自然语言文本 + 若干工具调用 + token 用量。
#[derive(Clone, Debug, Default)]
pub struct Completion {
    /// 面向用户的**回答**(OpenAI `content` / Anthropic text),已剥除思考。
    pub text: String,
    /// 思考模型的**推理正文**(GLM `reasoning_content` / inline `<think>`)。仅供展示,
    /// **不回灌** history(免污染上下文、免端点拒收)。无思考则空。
    pub reasoning: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
}

/// 流式增量的一段。思考模型(GLM 等)两路输出:`Answer` = 回答正文(`content`),
/// `Reasoning` = 思考(`reasoning_content`)。TUI/headless 据此**分区分色**——回答恒显(白),
/// 思考灰显。旧代码把二者塌缩成一路、再粗剪,遂令回答被吞或误当思考,此枚举为根治。
#[derive(Clone, Debug)]
pub enum StreamChunk {
    Answer(String),
    Reasoning(String),
}

/// 一次补全请求:对话历史 + 可用工具。
#[derive(Clone, Debug, Default)]
pub struct CompletionRequest {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
}

/// 从正文**剥出** inline 思考,返回 `(净回答, 思考)`。覆盖思考模型三种漏法:
/// ①成对 `<think>…</think>`;②**裸 `</think>`**(无起始标签 —— GLM 常态:思考在前、答案在后,
/// 以裸 `</think>` 分隔 → 其前全为思考,旧代码只删标签、把思考并进回答,是「回答被当思考」之一因);
/// ③孤立未闭合 `<think>`(流式截断)→ 其后全为思考。多段循环处理。
pub(crate) fn extract_inline_think(text: &str) -> (String, String) {
    let (mut answer, mut think) = (String::new(), String::new());
    let mut rest = text;
    while !rest.is_empty() {
        let a = rest.find("<think>");
        let b = rest.find("</think>");
        match (a, b) {
            // 成对 `<think>…</think>`:块前是回答,块内是思考。
            (Some(a), Some(b)) if a < b => {
                answer.push_str(&rest[..a]);
                think.push_str(&rest[a + "<think>".len()..b]);
                rest = &rest[b + "</think>".len()..];
            }
            // 孤立未闭合 `<think>`(流式截断):其后全为思考。
            (Some(a), None) => {
                answer.push_str(&rest[..a]);
                think.push_str(&rest[a + "<think>".len()..]);
                rest = "";
            }
            // 裸/前置 `</think>`(含 a≥b 的错序):其前皆思考(GLM 无起始标签的常态)。
            (_, Some(b)) => {
                think.push_str(&rest[..b]);
                rest = &rest[b + "</think>".len()..];
            }
            // 无任何标签:剩余全是回答。
            (None, None) => {
                answer.push_str(rest);
                rest = "";
            }
        }
    }
    (answer.trim().to_string(), think.trim().to_string())
}

/// 分出 `(回答, 思考)`:把 content 里的 inline 思考剥出,与独立 `reasoning_content` 字段合并。
pub(crate) fn split_thinking(content: &str, reasoning_field: &str) -> (String, String) {
    let (answer, inline) = extract_inline_think(content);
    let reasoning = match (reasoning_field.trim().is_empty(), inline.is_empty()) {
        (true, _) => inline,
        (false, true) => reasoning_field.trim().to_string(),
        (false, false) => format!("{}\n{inline}", reasoning_field.trim()),
    };
    (answer, reasoning)
}

/// LLM provider 抽象。真实实现(Anthropic/OpenAI HTTP)与离线 [`ScriptedProvider`] 都藏在这后面。
#[async_trait::async_trait]
pub trait LlmProvider: Send + Sync {
    async fn complete(&self, req: &CompletionRequest) -> Result<Completion, ProviderError>;

    /// 流式补全:每收到一段文本就调 `on_token`,供 REPL 逐字显示(像 Claude Code)。
    /// 回调收**owned `String`**(而非 `&str`)—— 避开 `async_trait` + HRTB 的生命周期坑,
    /// 每段一次小分配,SSE 场景可忽略。**默认回落到 [`complete`](LlmProvider::complete)**
    /// (整段拿到后一次性 emit),不支持流式的 provider 无需改动、坏路径也自动降级。
    async fn complete_streaming(
        &self,
        req: &CompletionRequest,
        on_token: &(dyn Fn(StreamChunk) + Send + Sync),
    ) -> Result<Completion, ProviderError> {
        let c = self.complete(req).await?;
        if !c.reasoning.is_empty() {
            on_token(StreamChunk::Reasoning(c.reasoning.clone()));
        }
        if !c.text.is_empty() {
            on_token(StreamChunk::Answer(c.text.clone()));
        }
        Ok(c)
    }
}

/// 运行时可**热切换**底层 provider 的包装:让 REPL 的 `/model <name>` 即时换模型,
/// 而**不必重建 agent 图**。图只见到一个 `Arc<dyn LlmProvider>`,真正的实现藏在
/// `Mutex<Arc<dyn LlmProvider>>` 后面;[`swap`](SwapProvider::swap) 换芯,下一次补全即生效。
pub struct SwapProvider {
    inner: std::sync::Mutex<std::sync::Arc<dyn LlmProvider>>,
}

impl SwapProvider {
    pub fn new(initial: std::sync::Arc<dyn LlmProvider>) -> Self {
        Self {
            inner: std::sync::Mutex::new(initial),
        }
    }

    /// 换掉底层 provider(下一次 `complete` 起生效)。
    pub fn swap(&self, next: std::sync::Arc<dyn LlmProvider>) {
        *self.inner.lock().unwrap() = next;
    }

    /// 取当前底层的 clone —— **持锁只到 clone**,不跨 await 持锁(std Mutex 不可跨 await)。
    fn current(&self) -> std::sync::Arc<dyn LlmProvider> {
        self.inner.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl LlmProvider for SwapProvider {
    async fn complete(&self, req: &CompletionRequest) -> Result<Completion, ProviderError> {
        self.current().complete(req).await
    }
    async fn complete_streaming(
        &self,
        req: &CompletionRequest,
        on_token: &(dyn Fn(StreamChunk) + Send + Sync),
    ) -> Result<Completion, ProviderError> {
        self.current().complete_streaming(req, on_token).await
    }
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
pub mod openai;

/// Anthropic Messages 响应的归一化(纯函数,离线可测)。
pub mod anthropic;

/// HTTP 传输层 —— 与「归一化」解耦(NotebookLM 建议:`LlmProvider` 不硬编码 reqwest)。
/// 测试可注入 stub / mock server,CI 保持离线绿。
pub mod http;

/// OAuth 2.0 (PKCE) 订阅登录 —— `ridgecode login --claude`(iter-43)。
///
/// 纯核(PKCE / 授权 URL / token 解析 / 刷新判定 / base64url)离线可测;实际 token 交换与刷新
/// 走 [`http::HttpClient`] 接缝,测试注入捕获替身零联网。
///
/// **诚实的验证边界**:确定性测试证明「机器」正确(给定常量);Anthropic 活 OAuth 端点 /
/// client_id / 所需 system 前缀 / beta header 值属 ToS 灰区且需订阅,**由用户实跑
/// `ridgecode login --claude` 验证**。常量集中于 [`anthropic_oauth`],补全侧 base_url 可配置覆盖。
pub mod oauth;

/// 实时模型目录 —— 向某 provider 的 `{base_url}/models` 发鉴权 GET,解析出模型 id 与
/// (端点自报的)上下文窗口大小。抓取只走 [`http::HttpClient`] 接缝,测试注入替身 → 零网络可测。
pub mod models;

/// Web 搜索 —— 先**探测网络环境**(能否直连国际网络 / 是否在 GFW 内),据此**换搜索引擎**:
/// 直连 → DuckDuckGo;受限(墙内)→ Bing 中国版(`cn.bing.com`,墙内可达且静态 HTML 好解析,
/// 而 DuckDuckGo/Google 在墙内不可达)。HTTP 只走 [`WebFetch`] 接缝,测试注入假抓取器 → 不联网可测。
pub mod search;

mod providers;
pub use providers::*;

#[cfg(test)]
mod tests;
