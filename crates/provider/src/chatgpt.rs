use crate::http::{HttpClient, ReqwestClient};
use crate::responses;
use crate::{Completion, CompletionRequest, LlmProvider, ProviderError, StreamChunk};
use std::sync::Arc;

/// ChatGPT subscription provider. OAuth subscription tokens use the Codex
/// Responses backend, not api.openai.com/v1/chat/completions.
pub struct ChatGptProvider {
    base_url: String,
    model: String,
    access_token: String,
    account_id: Option<String>,
    http: Arc<dyn HttpClient>,
}

impl ChatGptProvider {
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        access_token: impl Into<String>,
        account_id: Option<String>,
    ) -> Self {
        let access_token = access_token.into();
        Self {
            base_url: base_url.into(),
            model: model.into(),
            account_id: account_id
                .or_else(|| crate::oauth::chatgpt_account_id(None, &access_token)),
            access_token,
            http: Arc::new(ReqwestClient::new()),
        }
    }

    pub fn with_http(mut self, http: Arc<dyn HttpClient>) -> Self {
        self.http = http;
        self
    }

    fn url(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        if base.ends_with("/responses") {
            base.to_string()
        } else {
            format!("{base}/responses")
        }
    }

    fn headers(&self) -> Result<Vec<(String, String)>, ProviderError> {
        let account_id = self.account_id.as_deref().ok_or_else(|| {
            "ChatGPT OAuth token has no chatgpt_account_id; run ridgecode login --codex again"
                .to_string()
        })?;
        Ok(vec![
            (
                "Authorization".to_string(),
                format!("Bearer {}", self.access_token),
            ),
            ("ChatGPT-Account-Id".to_string(), account_id.to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Accept".to_string(), "text/event-stream".to_string()),
            ("originator".to_string(), "codex_cli_rs".to_string()),
        ])
    }
}

#[async_trait::async_trait]
impl LlmProvider for ChatGptProvider {
    async fn complete(&self, req: &CompletionRequest) -> Result<Completion, ProviderError> {
        self.complete_streaming(req, &|_| {}).await
    }

    async fn complete_streaming(
        &self,
        req: &CompletionRequest,
        on_token: &(dyn Fn(StreamChunk) + Send + Sync),
    ) -> Result<Completion, ProviderError> {
        let body = responses::build_request(&self.model, req);
        let url = self.url();
        let headers = self.headers()?;
        let acc = std::sync::Mutex::new(responses::StreamAcc::default());
        let on_line = |data: String| {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&data) {
                if let Ok(mut acc) = acc.lock() {
                    responses::accumulate_stream(&mut acc, &value, on_token);
                }
            }
        };
        self.http
            .post_json_stream(&url, &headers, &body, &on_line)
            .await?;
        let acc = acc
            .into_inner()
            .map_err(|_| "ChatGPT Responses stream accumulator poisoned")?;
        if let Some(message) = acc.error.as_deref() {
            return Err(format!("ChatGPT Responses API: {message}").into());
        }
        if !acc.completed {
            return Err("ChatGPT Responses stream ended before response.completed".into());
        }
        Ok(acc.into_completion())
    }
}
