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
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
}

/// 一次补全请求:对话历史 + 可用工具。
#[derive(Clone, Debug, Default)]
pub struct CompletionRequest {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
}

/// 剥掉思考模型漏进正文的 `<think>...</think>` 块与游离标签(如 GLM 会把 `</think>` 漏进 content)。
pub(crate) fn strip_thinking(text: &str) -> String {
    let mut s = text.to_string();
    // 成对的 <think>...</think> 块整段删掉。
    while let (Some(a), Some(b)) = (s.find("<think>"), s.find("</think>")) {
        if b > a {
            s.replace_range(a..b + "</think>".len(), "");
        } else {
            break; // 闭合早于开启 → 交给下面的游离标签清理
        }
    }
    s.replace("<think>", "")
        .replace("</think>", "")
        .trim()
        .to_string()
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
        on_token: &(dyn Fn(String) + Send + Sync),
    ) -> Result<Completion, ProviderError> {
        let c = self.complete(req).await?;
        if !c.text.is_empty() {
            on_token(c.text.clone());
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
        on_token: &(dyn Fn(String) + Send + Sync),
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
pub mod openai {
    use super::{
        strip_thinking, Completion, CompletionRequest, Message, ProviderError, Role, ToolCall,
        Usage,
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
        let text = strip_thinking(msg["content"].as_str().unwrap_or(""));
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
            tool_calls,
            usage,
        })
    }

    /// 流式增量的累加器:SSE 每帧的 `delta` 往里叠(文本直接拼,工具调用按 index 拼片段)。
    #[derive(Default)]
    pub struct StreamAcc {
        pub text: String,
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
    /// `on_token` 收 owned `String`(避 async_trait+HRTB 坑);`?Sized` 便于传 `&dyn Fn`。
    pub fn accumulate_stream<F: Fn(String) + ?Sized>(acc: &mut StreamAcc, v: &Value, on_token: &F) {
        let delta = &v["choices"][0]["delta"];
        if let Some(content) = delta["content"].as_str() {
            if !content.is_empty() {
                on_token(content.to_string());
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
        /// 收尾:组装成最终 [`Completion`](剥思考标签、把工具调用 arguments 片段解析回对象)。
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
            Completion {
                text: strip_thinking(&self.text),
                tool_calls,
                usage: self.usage,
            }
        }
    }
}

/// Anthropic Messages 响应的归一化(纯函数,离线可测)。
pub mod anthropic {
    use super::{
        strip_thinking, Completion, CompletionRequest, ProviderError, Role, ToolCall, Usage,
    };
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
}

/// HTTP 传输层 —— 与「归一化」解耦(NotebookLM 建议:`LlmProvider` 不硬编码 reqwest)。
/// 测试可注入 stub / mock server,CI 保持离线绿。
pub mod http {
    use super::ProviderError;
    use serde_json::Value;

    /// 只做「POST JSON 拿 JSON」这一件事,便于测试替身注入。
    #[async_trait::async_trait]
    pub trait HttpClient: Send + Sync {
        async fn post_json(
            &self,
            url: &str,
            headers: &[(String, String)],
            body: &Value,
        ) -> Result<Value, ProviderError>;

        /// POST 后**逐行**读 SSE 响应,每条 `data:` 行(去掉前缀、跳过 `[DONE]`)喂给 `on_line`。
        /// 回调收 owned `String`(避 HRTB 坑)。默认不支持(报错)—— 只有真实 [`ReqwestClient`]
        /// 与流式测试替身实现它;provider 会据错降级到整段。
        async fn post_json_stream(
            &self,
            _url: &str,
            _headers: &[(String, String)],
            _body: &Value,
            _on_line: &(dyn Fn(String) + Send + Sync),
        ) -> Result<(), ProviderError> {
            Err("this HttpClient does not support streaming".into())
        }
    }

    /// 生产用的真实客户端(reqwest)。
    pub struct ReqwestClient {
        client: reqwest::Client,
    }

    impl ReqwestClient {
        pub fn new() -> Self {
            Self {
                client: reqwest::Client::new(),
            }
        }
    }

    impl Default for ReqwestClient {
        fn default() -> Self {
            Self::new()
        }
    }

    #[async_trait::async_trait]
    impl HttpClient for ReqwestClient {
        async fn post_json(
            &self,
            url: &str,
            headers: &[(String, String)],
            body: &Value,
        ) -> Result<Value, ProviderError> {
            let mut rb = self.client.post(url).json(body);
            for (k, v) in headers {
                rb = rb.header(k.as_str(), v.as_str());
            }
            let resp = rb.send().await?;
            let status = resp.status();
            let val: Value = resp.json().await?;
            if !status.is_success() {
                return Err(format!("http {status}: {val}").into());
            }
            Ok(val)
        }

        async fn post_json_stream(
            &self,
            url: &str,
            headers: &[(String, String)],
            body: &Value,
            on_line: &(dyn Fn(String) + Send + Sync),
        ) -> Result<(), ProviderError> {
            let mut rb = self.client.post(url).json(body);
            for (k, v) in headers {
                rb = rb.header(k.as_str(), v.as_str());
            }
            let mut resp = rb.send().await?;
            let status = resp.status();
            if !status.is_success() {
                let t = resp.text().await.unwrap_or_default();
                return Err(format!("http {status}: {t}").into());
            }
            // `chunk()` 增量读 body(无需 reqwest "stream" feature),按 \n 切出完整 SSE 行。
            let mut buf = String::new();
            while let Some(bytes) = resp.chunk().await? {
                buf.push_str(&String::from_utf8_lossy(&bytes));
                while let Some(nl) = buf.find('\n') {
                    let line = buf[..nl].trim().to_string();
                    buf.drain(..=nl);
                    if let Some(data) = line.strip_prefix("data:") {
                        let data = data.trim();
                        if data == "[DONE]" {
                            return Ok(());
                        }
                        on_line(data.to_string());
                    }
                }
            }
            Ok(())
        }
    }
}

/// Web 搜索 —— 先**探测网络环境**(能否直连国际网络 / 是否在 GFW 内),据此**换搜索引擎**:
/// 直连 → DuckDuckGo;受限(墙内)→ Bing 中国版(`cn.bing.com`,墙内可达且静态 HTML 好解析,
/// 而 DuckDuckGo/Google 在墙内不可达)。HTTP 只走 [`WebFetch`] 接缝,测试注入假抓取器 → 不联网可测。
pub mod search {
    use super::ProviderError;
    use std::time::Duration;

    /// 抓文本(GET)的最小接缝:网页/JSON 都走它,便于测试替身注入。
    #[async_trait::async_trait]
    pub trait WebFetch: Send + Sync {
        async fn get_text(&self, url: &str) -> Result<String, ProviderError>;
    }

    /// 生产用真实抓取器(reqwest,带浏览器 UA + 超时,躲最基础的反爬、防卡死)。
    pub struct ReqwestFetch {
        client: reqwest::Client,
    }
    impl ReqwestFetch {
        pub fn new() -> Self {
            let client = reqwest::Client::builder()
                .user_agent(
                    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                     (KHTML, like Gecko) Chrome/122.0 Safari/537.36",
                )
                .timeout(Duration::from_secs(15))
                .build()
                .unwrap_or_default();
            Self { client }
        }
    }
    impl Default for ReqwestFetch {
        fn default() -> Self {
            Self::new()
        }
    }
    #[async_trait::async_trait]
    impl WebFetch for ReqwestFetch {
        async fn get_text(&self, url: &str) -> Result<String, ProviderError> {
            let resp = self.client.get(url).send().await?;
            let status = resp.status();
            let text = resp.text().await?;
            if !status.is_success() {
                return Err(format!("http {status}").into());
            }
            Ok(text)
        }
    }

    /// 网络环境。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum NetEnv {
        /// 能直连国际网络(裸连或带 VPN/代理)。
        International,
        /// 受限(GFW 内,连不到国际端点)。
        Restricted,
    }

    /// 探测能否直连国际网络:**并发**探两个国际端点(Google / gstatic 的 `generate_204`),
    /// 每个 3s 超时,**任一通** → International;都不通 → Restricted。比单探针更抗抖动、收敛更快
    /// (单个端点被限速/抽风不至于误判),且 3s 上限避免卡住 15s 的 HTTP 超时。
    /// ponytail: 2 探针够用;captive-portal(返 200 假页)仍可能误判,升级路径 = 校验响应体特征。
    pub async fn detect_net(fetch: &dyn WebFetch) -> NetEnv {
        async fn reachable(fetch: &dyn WebFetch, url: &str) -> bool {
            let timed =
                tokio::time::timeout(std::time::Duration::from_secs(3), fetch.get_text(url)).await;
            matches!(timed, Ok(Ok(_)))
        }
        let (a, b) = tokio::join!(
            reachable(fetch, "https://www.google.com/generate_204"),
            reachable(fetch, "https://www.gstatic.com/generate_204"),
        );
        if a || b {
            NetEnv::International
        } else {
            NetEnv::Restricted
        }
    }

    /// 该环境的**首选**搜索引擎名(展示/审计用;实际是引擎链的第一个)。
    pub fn engine_for(env: NetEnv) -> &'static str {
        engines(env).first().map(|e| e.name).unwrap_or("none")
    }

    /// 一个**无 key** 搜索引擎:query(已 urlencode)→ 请求 URL,HTML → 结果解析。
    struct Engine {
        name: &'static str,
        url: fn(&str) -> String,
        parse: fn(&str) -> Vec<SearchResult>,
    }

    fn ddg_url(q: &str) -> String {
        format!("https://html.duckduckgo.com/html/?q={q}")
    }
    fn bing_url(q: &str) -> String {
        format!("https://www.bing.com/search?q={q}")
    }
    fn bing_cn_url(q: &str) -> String {
        format!("https://cn.bing.com/search?q={q}")
    }

    /// 按网络环境给出**有序引擎链**:首选打头,其余作 fallback(某引擎改版/抽风/被限流 →
    /// 自动换下一个)。**全是无 key 直抓 HTML,不依赖任何第三方付费 API**(Brave/Tavily 等)。
    fn engines(env: NetEnv) -> &'static [Engine] {
        const INTL: &[Engine] = &[
            Engine {
                name: "duckduckgo",
                url: ddg_url,
                parse: parse_duckduckgo,
            },
            Engine {
                name: "bing",
                url: bing_url,
                parse: parse_bing,
            },
        ];
        const CN: &[Engine] = &[Engine {
            name: "bing-cn",
            url: bing_cn_url,
            parse: parse_bing,
        }];
        match env {
            NetEnv::International => INTL,
            NetEnv::Restricted => CN,
        }
    }

    /// 一条搜索结果。
    #[derive(Debug, Clone, PartialEq)]
    pub struct SearchResult {
        pub title: String,
        pub url: String,
        pub snippet: String,
    }

    /// 搜索:按 env 取**引擎链**,**依次**尝试直到拿到非空结果(每条去重、裁 top 8)。
    /// 某引擎报错/返回空 → 自动落到下一个;全失败才返回错(全空则返回空列表)。
    /// ponytail: 全程无 key 直抓 HTML,靠多引擎 fallback 抗单点失效;仍怕全体同时改版,
    /// 升级路径 = 往 [`engines`] 里加更多无 key 引擎(Mojeek / SearXNG 公共实例),**不接付费 API**。
    pub async fn web_search(
        fetch: &dyn WebFetch,
        query: &str,
        env: NetEnv,
    ) -> Result<Vec<SearchResult>, ProviderError> {
        let q = urlencode(query);
        let mut last_err = None;
        for eng in engines(env) {
            match fetch.get_text(&(eng.url)(&q)).await {
                Ok(html) => {
                    let mut results = (eng.parse)(&html);
                    // 按 URL 去重(保序):同一结果被引擎重复列出时只留一条。
                    let mut seen = std::collections::HashSet::new();
                    results.retain(|r| seen.insert(r.url.clone()));
                    results.truncate(8);
                    if !results.is_empty() {
                        return Ok(results);
                    }
                }
                Err(e) => last_err = Some(e),
            }
        }
        last_err.map_or(Ok(Vec::new()), Err)
    }

    /// 抓一个网页并抽成**可读正文**(供 RAG:web_search 拿链接 → fetch_url 读正文 → 据原文作答)。
    pub async fn fetch_url(fetch: &dyn WebFetch, url: &str) -> Result<String, ProviderError> {
        let html = fetch.get_text(url).await?;
        Ok(html_to_text(&html))
    }

    /// HTML → 正文:先删 script/style 等整块,块级结束标签转换行,再去标签、压缩空白、截断防爆。
    /// ponytail: 非 readability 级正文抽取,够喂模型;升级路径 = 接 readability/dom 解析。
    fn html_to_text(html: &str) -> String {
        let mut buf = strip_blocks(html, &["script", "style", "noscript", "head", "svg"]);
        for tag in [
            "</p>", "</div>", "</li>", "</h1>", "</h2>", "</h3>", "</h4>", "</tr>", "<br>",
            "<br/>", "<br />",
        ] {
            buf = buf.replace(tag, "\n");
        }
        let text = strip_tags(&buf); // 去剩余标签 + 解实体(保留我插入的 \n)
        let mut out = String::new();
        let mut blank = false;
        for line in text.lines() {
            let t = line.split_whitespace().collect::<Vec<_>>().join(" ");
            if t.is_empty() {
                if !blank {
                    out.push('\n');
                }
                blank = true;
            } else {
                out.push_str(&t);
                out.push('\n');
                blank = false;
            }
        }
        let out = out.trim();
        if out.chars().count() > 4000 {
            let head: String = out.chars().take(4000).collect();
            format!("{head}\n…(正文截断)")
        } else {
            out.to_string()
        }
    }

    /// 删掉 `<tag ...>…</tag>` 整块(ASCII 大小写不敏感,字节布局不变→索引安全)。无闭合则删到结尾。
    fn strip_blocks(html: &str, tags: &[&str]) -> String {
        let mut s = html.to_string();
        for tag in tags {
            let open = format!("<{tag}");
            let close = format!("</{tag}>");
            loop {
                let lower = s.to_ascii_lowercase(); // ASCII fold 不改字节长度,索引对 s 有效
                let Some(a) = lower.find(&open) else { break };
                let end = match lower[a..].find(&close) {
                    Some(rel) => a + rel + close.len(),
                    None => s.len(),
                };
                s.replace_range(a..end, " ");
            }
        }
        s
    }

    /// 解析 DuckDuckGo html 端点:结果锚点 `class="result__a"` + 摘要 `class="result__snippet"`。
    fn parse_duckduckgo(html: &str) -> Vec<SearchResult> {
        let mut out = Vec::new();
        for chunk in html.split("class=\"result__a\"").skip(1) {
            // chunk 形如: ` href="//duckduckgo.com/l/?uddg=...">Title</a> ... result__snippet ...>Snippet</a>`
            let href = attr(chunk, "href=\"").unwrap_or_default();
            // 跳过广告位:DuckDuckGo 的赞助结果走 `/y.js?ad_domain=…` 跳转,不是自然结果。
            if href.contains("/y.js") || href.contains("ad_domain=") {
                continue;
            }
            let title = between(chunk, ">", "</a>")
                .map(strip_tags)
                .unwrap_or_default();
            let url = clean_ddg_url(&href);
            let snippet = chunk
                .find("result__snippet")
                .and_then(|p| between(&chunk[p..], ">", "</a>"))
                .map(strip_tags)
                .unwrap_or_default();
            if !title.is_empty() && !url.is_empty() {
                out.push(SearchResult {
                    title,
                    url,
                    snippet,
                });
            }
        }
        out
    }

    /// 解析 Bing 静态 HTML:每条结果 `<li class="b_algo">`,标题在 `<h2><a href>`,摘要在 `<p>`。
    fn parse_bing(html: &str) -> Vec<SearchResult> {
        let mut out = Vec::new();
        for chunk in html.split("class=\"b_algo\"").skip(1) {
            let Some(h2) = chunk.find("<h2") else {
                continue;
            };
            let url = attr(&chunk[h2..], "href=\"").unwrap_or_default();
            let title = between(&chunk[h2..], ">", "</a>")
                .map(strip_tags)
                .unwrap_or_default();
            let snippet = chunk
                .find("<p")
                .and_then(|p| between(&chunk[p..], ">", "</p>"))
                .map(strip_tags)
                .unwrap_or_default();
            if !title.is_empty() && !url.is_empty() {
                out.push(SearchResult {
                    title,
                    url,
                    snippet,
                });
            }
        }
        out
    }

    /// 取 `hay` 中 `start` 之后到 `end` 之前的片段(找不到 → None)。
    fn between<'a>(hay: &'a str, start: &str, end: &str) -> Option<&'a str> {
        let i = hay.find(start)? + start.len();
        let rest = &hay[i..];
        let j = rest.find(end)?;
        Some(&rest[..j])
    }

    /// 取 `key`(如 `href="`)后到下一个 `"` 之前的属性值。
    fn attr(hay: &str, key: &str) -> Option<String> {
        between(hay, key, "\"").map(|s| s.to_string())
    }

    /// DuckDuckGo 的 href 是跳转链接 `//duckduckgo.com/l/?uddg=<编码真实 URL>&rut=...` —— 解出真实 URL。
    fn clean_ddg_url(href: &str) -> String {
        let h = href.replace("&amp;", "&");
        if let Some(v) = h.split("uddg=").nth(1) {
            return percent_decode(v.split('&').next().unwrap_or(v));
        }
        if let Some(rest) = h.strip_prefix("//") {
            return format!("https://{rest}");
        }
        h
    }

    /// 去掉 `<...>` 标签 + 解一点常见 HTML 实体。ponytail: 非完整 HTML 解析,够抽标题/摘要用。
    fn strip_tags(s: &str) -> String {
        let mut out = String::new();
        let mut in_tag = false;
        for c in s.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => out.push(c),
                _ => {}
            }
        }
        out.replace("&amp;", "&")
            .replace("&#39;", "'")
            .replace("&quot;", "\"")
            .replace("&nbsp;", " ")
            .trim()
            .to_string()
    }

    /// 极简 percent-encode(RFC3986 unreserved 之外全转义)。
    fn urlencode(s: &str) -> String {
        let mut out = String::new();
        for b in s.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(b as char)
                }
                b' ' => out.push_str("%20"),
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    }

    /// 极简 percent-decode(`%XX` → 字节,`+` → 空格)。
    fn percent_decode(s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                if let Some(b) = std::str::from_utf8(&bytes[i + 1..i + 3])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                {
                    out.push(b);
                    i += 3;
                    continue;
                }
            }
            out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// 记录被请求的 URL、按 URL 回不同 HTML 的假抓取器(不联网)。
        struct FakeFetch {
            duck: String,
            bing: String,
            probe_ok: bool,
        }
        #[async_trait::async_trait]
        impl WebFetch for FakeFetch {
            async fn get_text(&self, url: &str) -> Result<String, ProviderError> {
                if url.contains("generate_204") {
                    return if self.probe_ok {
                        Ok(String::new())
                    } else {
                        Err("blocked".into())
                    };
                }
                if url.contains("duckduckgo") {
                    Ok(self.duck.clone())
                } else if url.contains("bing") {
                    Ok(self.bing.clone())
                } else {
                    Err("unexpected url".into())
                }
            }
        }

        fn fake() -> FakeFetch {
            FakeFetch {
                duck: r#"<a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.rust-lang.org%2F&amp;rut=x">Rust 语言</a>
                    <a class="result__snippet" href="//x">A language empowering everyone.</a>"#.to_string(),
                bing: r#"<li class="b_algo"><h2><a href="https://doc.rust-lang.org/book/">The Rust Book</a></h2><p class="b_lineclamp2">学 Rust 的书。</p></li>"#.to_string(),
                probe_ok: true,
            }
        }

        #[tokio::test]
        async fn detect_net_reads_probe() {
            let mut f = fake();
            f.probe_ok = true;
            assert_eq!(detect_net(&f).await, NetEnv::International);
            f.probe_ok = false;
            assert_eq!(detect_net(&f).await, NetEnv::Restricted);
        }

        #[tokio::test]
        async fn detect_net_international_if_any_probe_ok() {
            // google 探针失败、gstatic 成功 → 仍判直连(多探针 OR 语义)。
            struct MixedProbe;
            #[async_trait::async_trait]
            impl WebFetch for MixedProbe {
                async fn get_text(&self, url: &str) -> Result<String, ProviderError> {
                    if url.contains("gstatic") {
                        Ok(String::new())
                    } else {
                        Err("blocked".into())
                    }
                }
            }
            assert_eq!(detect_net(&MixedProbe).await, NetEnv::International);
        }

        #[tokio::test]
        async fn international_uses_duckduckgo_and_decodes_url() {
            let r = web_search(&fake(), "rust 语言", NetEnv::International)
                .await
                .unwrap();
            assert_eq!(r.len(), 1);
            assert_eq!(r[0].title, "Rust 语言");
            assert_eq!(r[0].url, "https://www.rust-lang.org/"); // uddg 解码
            assert!(r[0].snippet.contains("empowering"));
        }

        #[tokio::test]
        async fn restricted_uses_bing() {
            let r = web_search(&fake(), "rust", NetEnv::Restricted)
                .await
                .unwrap();
            assert_eq!(r.len(), 1);
            assert_eq!(r[0].title, "The Rust Book");
            assert_eq!(r[0].url, "https://doc.rust-lang.org/book/");
            assert!(r[0].snippet.contains("Rust"));
        }

        #[tokio::test]
        async fn fetch_url_extracts_readable_text() {
            struct PageFetch;
            #[async_trait::async_trait]
            impl WebFetch for PageFetch {
                async fn get_text(&self, _url: &str) -> Result<String, ProviderError> {
                    Ok(
                        r#"<html><head><title>T</title><style>.x{color:red}</style></head>
                        <body><script>alert('nope')</script>
                        <h1>标题</h1><p>第一段正文。</p><p>第二段正文。</p></body></html>"#
                            .to_string(),
                    )
                }
            }
            let text = fetch_url(&PageFetch, "https://x.com").await.unwrap();
            assert!(
                text.contains("标题") && text.contains("第一段正文"),
                "{text}"
            );
            assert!(
                !text.contains("alert") && !text.contains("color:red"),
                "脚本/样式应被剔除: {text}"
            );
            // 块级标签转了换行 → 段落分行,不会糊成一坨。
            assert!(text.lines().count() >= 3, "应按段分行: {text}");
        }

        #[test]
        fn parse_duckduckgo_skips_ads() {
            let html = r#"
                <a class="result__a" href="//duckduckgo.com/y.js?ad_domain=spam.com&ad_provider=bing">广告</a>
                <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Freal.com%2F">真实结果</a>"#;
            let r = parse_duckduckgo(html);
            assert_eq!(r.len(), 1, "广告位应被过滤: {r:?}");
            assert_eq!(r[0].url, "https://real.com/");
        }

        #[tokio::test]
        async fn web_search_falls_back_when_primary_empty() {
            // 首选 DuckDuckGo 返回无结果 → 自动落到 Bing(无 key fallback,不依赖付费 API)。
            struct FallbackFetch;
            #[async_trait::async_trait]
            impl WebFetch for FallbackFetch {
                async fn get_text(&self, url: &str) -> Result<String, ProviderError> {
                    if url.contains("duckduckgo") {
                        Ok("<html>no results here</html>".to_string())
                    } else if url.contains("bing") {
                        Ok(r#"<li class="b_algo"><h2><a href="https://b.com/">命中</a></h2><p>摘要</p></li>"#.to_string())
                    } else {
                        Err("unexpected".into())
                    }
                }
            }
            let r = web_search(&FallbackFetch, "q", NetEnv::International)
                .await
                .unwrap();
            assert_eq!(r.len(), 1, "DDG 空 → 应落到 Bing: {r:?}");
            assert_eq!(r[0].url, "https://b.com/");
        }

        #[tokio::test]
        async fn web_search_dedupes_by_url() {
            struct DupFetch;
            #[async_trait::async_trait]
            impl WebFetch for DupFetch {
                async fn get_text(&self, _url: &str) -> Result<String, ProviderError> {
                    // 同一 URL 出现两次的 DDG 结果。
                    Ok(r#"<a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fa.com%2F">A1</a>
                        <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fa.com%2F">A2</a>"#.to_string())
                }
            }
            let r = web_search(&DupFetch, "q", NetEnv::International)
                .await
                .unwrap();
            assert_eq!(r.len(), 1, "重复 URL 应去重: {r:?}");
            assert_eq!(r[0].url, "https://a.com/");
        }

        #[test]
        fn strip_blocks_removes_script_case_insensitive() {
            let out = strip_blocks("<BODY>keep<SCRIPT>drop</SCRIPT>keep2</BODY>", &["script"]);
            assert!(out.contains("keep") && out.contains("keep2") && !out.contains("drop"));
        }

        #[test]
        fn urlencode_and_decode_roundtrip() {
            assert_eq!(urlencode("a b&c"), "a%20b%26c");
            assert_eq!(percent_decode("https%3A%2F%2Fx.com%2F"), "https://x.com/");
            assert_eq!(engine_for(NetEnv::International), "duckduckgo");
            assert_eq!(engine_for(NetEnv::Restricted), "bing-cn");
        }
    }
}

use http::{HttpClient, ReqwestClient};
use std::sync::Arc;

/// OpenAI 兼容 provider:`build_request` → HTTP → `parse_response`,全走归一化层。
pub struct OpenAiProvider {
    base_url: String,
    model: String,
    api_key: String,
    http: Arc<dyn HttpClient>,
}

impl OpenAiProvider {
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            api_key: api_key.into(),
            http: Arc::new(ReqwestClient::new()),
        }
    }

    /// 注入自定义传输(测试用 stub / mock)。
    pub fn with_http(mut self, http: Arc<dyn HttpClient>) -> Self {
        self.http = http;
        self
    }
}

#[async_trait::async_trait]
impl LlmProvider for OpenAiProvider {
    async fn complete(&self, req: &CompletionRequest) -> Result<Completion, ProviderError> {
        let body = openai::build_request(&self.model, req);
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let headers = [
            (
                "Authorization".to_string(),
                format!("Bearer {}", self.api_key),
            ),
            ("Content-Type".to_string(), "application/json".to_string()),
        ];
        let resp = self.http.post_json(&url, &headers, &body).await?;
        openai::parse_response(&resp)
    }

    async fn complete_streaming(
        &self,
        req: &CompletionRequest,
        on_token: &(dyn Fn(String) + Send + Sync),
    ) -> Result<Completion, ProviderError> {
        let mut body = openai::build_request(&self.model, req);
        body["stream"] = Value::Bool(true);
        body["stream_options"] = serde_json::json!({ "include_usage": true });
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let headers = [
            (
                "Authorization".to_string(),
                format!("Bearer {}", self.api_key),
            ),
            ("Content-Type".to_string(), "application/json".to_string()),
        ];
        let acc = std::sync::Mutex::new(openai::StreamAcc::default());
        let on_line = |data: String| {
            if let Ok(v) = serde_json::from_str::<Value>(&data) {
                openai::accumulate_stream(&mut acc.lock().unwrap(), &v, on_token);
            }
        };
        match self
            .http
            .post_json_stream(&url, &headers, &body, &on_line)
            .await
        {
            Ok(()) => Ok(acc.into_inner().unwrap().into_completion()),
            // 传输不支持流式(或流式失败)→ 降级到整段 complete(不丢功能)。
            Err(_) => {
                let c = self.complete(req).await?;
                if !c.text.is_empty() {
                    on_token(c.text.clone());
                }
                Ok(c)
            }
        }
    }
}

/// 原生 Anthropic Messages provider。
pub struct AnthropicProvider {
    base_url: String,
    model: String,
    api_key: String,
    max_tokens: u32,
    http: Arc<dyn HttpClient>,
}

impl AnthropicProvider {
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            api_key: api_key.into(),
            max_tokens: 4096,
            http: Arc::new(ReqwestClient::new()),
        }
    }

    pub fn with_http(mut self, http: Arc<dyn HttpClient>) -> Self {
        self.http = http;
        self
    }
}

#[async_trait::async_trait]
impl LlmProvider for AnthropicProvider {
    async fn complete(&self, req: &CompletionRequest) -> Result<Completion, ProviderError> {
        let body = anthropic::build_request(&self.model, self.max_tokens, req);
        let url = format!("{}/messages", self.base_url.trim_end_matches('/'));
        let headers = [
            ("x-api-key".to_string(), self.api_key.clone()),
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
        ];
        let resp = self.http.post_json(&url, &headers, &body).await?;
        anthropic::parse_response(&resp)
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

    #[tokio::test]
    async fn swap_provider_hot_switches_inner() {
        let comp = |t: &str| Completion {
            text: t.into(),
            ..Default::default()
        };
        let a: Arc<dyn LlmProvider> = Arc::new(ScriptedProvider::new(vec![comp("from-A")]));
        let b: Arc<dyn LlmProvider> = Arc::new(ScriptedProvider::new(vec![comp("from-B")]));
        let sw = SwapProvider::new(a);
        let req = CompletionRequest::default();
        // 起始走 A。
        assert_eq!(sw.complete(&req).await.unwrap().text, "from-A");
        // 热切换到 B → 下一次补全即走 B(图无需重建)。
        sw.swap(b);
        assert_eq!(sw.complete(&req).await.unwrap().text, "from-B");
    }

    #[test]
    fn plain_text_response_has_no_tool_calls() {
        let wire = json!({"choices": [{"message": {"content": "all done"}}]});
        let c = openai::parse_response(&wire).unwrap();
        assert_eq!(c.text, "all done");
        assert!(c.tool_calls.is_empty());
    }

    #[test]
    fn strips_thinking_tags_from_content() {
        // 成对块。
        assert_eq!(strip_thinking("<think>reasoning here</think>pong"), "pong");
        // GLM 实测:游离的 </think> 漏进正文。
        assert_eq!(
            strip_thinking("\nresult\n</think>\nThe answer"),
            "result\n\nThe answer".trim()
        );
        // 无标签原样(去首尾空白)。
        assert_eq!(strip_thinking("  clean  "), "clean");

        let wire = json!({"choices": [{"message": {"content": "<think>x</think>done"}}]});
        assert_eq!(openai::parse_response(&wire).unwrap().text, "done");
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

    /// 注入 stub 传输:完整走 build_request → (假)HTTP → parse_response,零网络。
    struct StubHttp(Value);

    #[async_trait::async_trait]
    impl HttpClient for StubHttp {
        async fn post_json(
            &self,
            _url: &str,
            _headers: &[(String, String)],
            _body: &Value,
        ) -> Result<Value, ProviderError> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn openai_provider_full_path_with_stub_http() {
        let canned = json!({"choices":[{"message":{"content":"","tool_calls":[{
            "id":"c1","type":"function",
            "function":{"name":"run_shell","arguments":"{\"cmd\":\"cargo build\"}"}
        }]}}]});
        let p = OpenAiProvider::new("http://unused", "gpt-x", "key")
            .with_http(Arc::new(StubHttp(canned)));
        let req = CompletionRequest {
            messages: vec![Message::user("build it")],
            tools: vec![],
        };
        let c = p.complete(&req).await.unwrap();
        assert_eq!(c.tool_calls[0].name, "run_shell");
        assert_eq!(c.tool_calls[0].arguments, json!({"cmd": "cargo build"}));
    }

    /// 假流式传输:把预设的 SSE `data:` 帧逐条喂给 on_line(不用 mockito、零网络)。
    struct FakeStream(Vec<String>);
    #[async_trait::async_trait]
    impl HttpClient for FakeStream {
        async fn post_json(
            &self,
            _u: &str,
            _h: &[(String, String)],
            _b: &Value,
        ) -> Result<Value, ProviderError> {
            Err("only streaming".into())
        }
        async fn post_json_stream(
            &self,
            _u: &str,
            _h: &[(String, String)],
            _b: &Value,
            on_line: &(dyn Fn(String) + Send + Sync),
        ) -> Result<(), ProviderError> {
            for frame in &self.0 {
                on_line(frame.clone());
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn openai_streaming_assembles_text_tokens_toolcalls_usage() {
        // 文本分 3 帧 + 一个分片工具调用(name 首帧、arguments 两帧)+ 末帧 usage。
        let frames: Vec<String> = vec![
            r#"{"choices":[{"delta":{"content":"你好"}}]}"#.into(),
            r#"{"choices":[{"delta":{"content":",世界"}}]}"#.into(),
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"run_shell","arguments":"{\"cmd\":"}}]}}]}"#.into(),
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"ls\"}"}}]}}]}"#.into(),
            r#"{"choices":[],"usage":{"prompt_tokens":11,"completion_tokens":7}}"#.into(),
        ];
        let p = OpenAiProvider::new("http://unused", "gpt-x", "key")
            .with_http(Arc::new(FakeStream(frames)));
        let req = CompletionRequest {
            messages: vec![Message::user("hi")],
            tools: vec![],
        };
        let streamed = std::sync::Mutex::new(String::new());
        let on_token = |t: String| streamed.lock().unwrap().push_str(&t);
        let c = p.complete_streaming(&req, &on_token).await.unwrap();
        // 逐字回调拼起来 == 完整文本。
        assert_eq!(*streamed.lock().unwrap(), "你好,世界");
        assert_eq!(c.text, "你好,世界");
        // 分片工具调用被拼装回完整对象。
        assert_eq!(c.tool_calls.len(), 1);
        assert_eq!(c.tool_calls[0].name, "run_shell");
        assert_eq!(c.tool_calls[0].arguments, json!({"cmd": "ls"}));
        assert_eq!(c.usage.total(), 18);
    }

    /// 默认 provider(无流式实现)+ 不支持流式的传输 → complete_streaming 降级到整段,一次性 emit。
    #[tokio::test]
    async fn streaming_falls_back_to_whole_text() {
        let canned = json!({"choices":[{"message":{"content":"整段回答"}}]});
        let p = OpenAiProvider::new("http://unused", "gpt-x", "key")
            .with_http(Arc::new(StubHttp(canned))); // StubHttp 没实现 post_json_stream → 默认报错 → 降级
        let req = CompletionRequest {
            messages: vec![Message::user("hi")],
            tools: vec![],
        };
        let got = std::sync::Mutex::new(String::new());
        let c = p
            .complete_streaming(&req, &|t: String| got.lock().unwrap().push_str(&t))
            .await
            .unwrap();
        assert_eq!(c.text, "整段回答");
        assert_eq!(*got.lock().unwrap(), "整段回答");
    }

    /// 记录 provider 实际发出的一次请求。
    #[derive(Clone)]
    struct SeenRequest {
        url: String,
        headers: Vec<(String, String)>,
        body: Value,
    }

    /// 捕获型传输替身:记录 provider 拼出的请求,并回一个 canned 响应。
    /// 用它校验「provider 把请求拼对了」——比起真打本地 server(在有系统代理的机器上会挂起),
    /// 这是确定性、零网络、瞬时的,覆盖同样的东西(auth 头 / url / 请求体)。
    struct CapturingHttp {
        seen: std::sync::Mutex<Option<SeenRequest>>,
        reply: Value,
    }

    impl CapturingHttp {
        fn new(reply: Value) -> Self {
            Self {
                seen: std::sync::Mutex::new(None),
                reply,
            }
        }

        fn seen(&self) -> SeenRequest {
            self.seen.lock().unwrap().clone().unwrap()
        }
    }

    #[async_trait::async_trait]
    impl HttpClient for CapturingHttp {
        async fn post_json(
            &self,
            url: &str,
            headers: &[(String, String)],
            body: &Value,
        ) -> Result<Value, ProviderError> {
            *self.seen.lock().unwrap() = Some(SeenRequest {
                url: url.to_string(),
                headers: headers.to_vec(),
                body: body.clone(),
            });
            Ok(self.reply.clone())
        }
    }

    #[tokio::test]
    async fn openai_provider_sends_bearer_auth_and_correct_url() {
        let cap = Arc::new(CapturingHttp::new(
            json!({"choices":[{"message":{"content":"ok"}}]}),
        ));
        let p = OpenAiProvider::new("https://api.example.com", "gpt-x", "sk-test")
            .with_http(cap.clone());
        let c = p
            .complete(&CompletionRequest {
                messages: vec![Message::user("hi")],
                tools: vec![],
            })
            .await
            .unwrap();

        assert_eq!(c.text, "ok");
        let seen = cap.seen();
        assert_eq!(seen.url, "https://api.example.com/chat/completions");
        assert!(seen
            .headers
            .iter()
            .any(|(k, v)| k == "Authorization" && v == "Bearer sk-test"));
        assert_eq!(seen.body["messages"][0]["role"], "user");
    }

    #[tokio::test]
    async fn anthropic_provider_sends_api_key_version_and_system_top_level() {
        let cap = Arc::new(CapturingHttp::new(
            json!({"content":[{"type":"text","text":"ok"}]}),
        ));
        let p = AnthropicProvider::new("https://api.anthropic.com/v1", "claude-x", "sk-ant")
            .with_http(cap.clone());
        let c = p
            .complete(&CompletionRequest {
                messages: vec![Message::system("be terse"), Message::user("hi")],
                tools: vec![],
            })
            .await
            .unwrap();

        assert_eq!(c.text, "ok");
        let seen = cap.seen();
        assert_eq!(seen.url, "https://api.anthropic.com/v1/messages");
        assert!(seen
            .headers
            .iter()
            .any(|(k, v)| k == "x-api-key" && v == "sk-ant"));
        assert!(seen
            .headers
            .iter()
            .any(|(k, v)| k == "anthropic-version" && !v.is_empty()));
        // system 抽成顶层参数,不混进 messages。
        assert_eq!(seen.body["system"], "be terse");
        assert_eq!(seen.body["messages"][0]["role"], "user");
    }
}
