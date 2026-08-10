//! 具体 LlmProvider 实现:OpenAI 兼容 / 原生 Anthropic Messages(传输 × 归一化的薄接线层)。
use crate::http::{HttpClient, ReqwestClient};
use crate::{
    anthropic, oauth, openai, Completion, CompletionRequest, LlmProvider, Message, ProviderError,
    StreamChunk,
};
use serde_json::Value;
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
        on_token: &(dyn Fn(StreamChunk) + Send + Sync),
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
                if !c.reasoning.is_empty() {
                    on_token(StreamChunk::Reasoning(c.reasoning.clone()));
                }
                if !c.text.is_empty() {
                    on_token(StreamChunk::Answer(c.text.clone()));
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
    /// api-key 模式:走 `x-api-key`;**oauth 模式**(iter-43):此处存 access_token,走 `Bearer`。
    secret: String,
    /// iter-43:true → `Authorization: Bearer` + `anthropic-beta`(去 `x-api-key`),订阅登录用。
    oauth: bool,
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
            secret: api_key.into(),
            oauth: false,
            max_tokens: 4096,
            http: Arc::new(ReqwestClient::new()),
        }
    }

    /// OAuth 订阅模式(iter-43):`access_token` 走 `Authorization: Bearer` + `anthropic-beta`,
    /// 且 system 首块注入 Claude Code 身份(OAuth 路径要求)。见 [`oauth`] 的验证边界。
    pub fn new_oauth(
        base_url: impl Into<String>,
        model: impl Into<String>,
        access_token: impl Into<String>,
    ) -> Self {
        Self {
            oauth: true,
            ..Self::new(base_url, model, access_token)
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
        // oauth 路径:前置一条 Claude Code 身份 system(build_request 会 join 进顶层 system)。
        let oauth_req;
        let req = if self.oauth {
            let mut m = req.clone();
            m.messages
                .insert(0, Message::system(oauth::anthropic_oauth::SYSTEM_IDENTITY));
            oauth_req = m;
            &oauth_req
        } else {
            req
        };
        let body = anthropic::build_request(&self.model, self.max_tokens, req);
        let url = format!("{}/messages", self.base_url.trim_end_matches('/'));
        let headers = if self.oauth {
            vec![
                (
                    "Authorization".to_string(),
                    format!("Bearer {}", self.secret),
                ),
                (
                    "anthropic-beta".to_string(),
                    oauth::anthropic_oauth::BETA.to_string(),
                ),
                ("anthropic-version".to_string(), "2023-06-01".to_string()),
                ("Content-Type".to_string(), "application/json".to_string()),
            ]
        } else {
            vec![
                ("x-api-key".to_string(), self.secret.clone()),
                ("anthropic-version".to_string(), "2023-06-01".to_string()),
                ("Content-Type".to_string(), "application/json".to_string()),
            ]
        };
        let resp = self.http.post_json(&url, &headers, &body).await?;
        anthropic::parse_response(&resp)
    }
}
