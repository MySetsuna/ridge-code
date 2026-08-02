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

    /// 「带 header 的 GET JSON」这一件事(取模型列表用)。默认不支持(报错)——
    /// 既有测试替身零改动;只有真实 [`ReqwestClient`] 与取模型的测试替身实现它。
    async fn get_json(
        &self,
        _url: &str,
        _headers: &[(String, String)],
    ) -> Result<Value, ProviderError> {
        Err("this HttpClient does not support GET".into())
    }

    /// 「POST form-urlencoded 拿 JSON」(标准 OAuth token 端点用,iter-48)。默认不支持。
    async fn post_form(&self, _url: &str, _form: &[(&str, &str)]) -> Result<Value, ProviderError> {
        Err("this HttpClient does not support form POST".into())
    }
}

/// 生产用的真实客户端(reqwest)。
pub struct ReqwestClient {
    client: reqwest::Client,
}

/// LLM 请求超时秒数(env `RIDGE_HTTP_TIMEOUT` 可调,默认 180)。**防端点卡住令任务永久 hang** ——
/// 无超时时,GLM 等端点偶发流式卡住会冻住整个 reason 节点(超步内 await 永不返回,`max_supersteps`
/// 拦不住)。非流式=整请求超时;流式=响应头等待 + **逐块 idle** 超时(不误杀正常慢流)。
fn timeout_secs() -> u64 {
    std::env::var("RIDGE_HTTP_TIMEOUT")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(180)
}

impl ReqwestClient {
    pub fn new() -> Self {
        // connect 超时防连接挂死;逐调用另加请求/idle 超时(见各方法)。
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { client }
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
        let mut rb = self
            .client
            .post(url)
            // 显式序列化，避免部分 SOCKS/HTTP 代理对 reqwest `.json()` 的
            // transfer framing 处理异常，导致服务端把 JSON 看成无效 body。
            .body(body.to_string())
            .timeout(std::time::Duration::from_secs(timeout_secs()));
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

    async fn get_json(
        &self,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<Value, ProviderError> {
        let mut rb = self
            .client
            .get(url)
            .timeout(std::time::Duration::from_secs(timeout_secs()));
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

    async fn post_form(&self, url: &str, form: &[(&str, &str)]) -> Result<Value, ProviderError> {
        let body = form
            .iter()
            .map(|(key, value)| format!("{}={}", form_component(key), form_component(value)))
            .collect::<Vec<_>>()
            .join("&");
        let rb = self
            .client
            .post(url)
            // Match the OpenAI Codex token exchange wire: explicit raw form body.
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .timeout(std::time::Duration::from_secs(timeout_secs()));
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
        let mut rb = self.client.post(url).body(body.to_string());
        for (k, v) in headers {
            rb = rb.header(k.as_str(), v.as_str());
        }
        let dur = std::time::Duration::from_secs(timeout_secs());
        // 响应头等待超时:端点接了连接却不回头 → 中止,不永久 hang。
        let mut resp =
            tokio::time::timeout(dur, rb.send())
                .await
                .map_err(|_| -> ProviderError {
                    "stream request timed out waiting for response headers".into()
                })??;
        let status = resp.status();
        if !status.is_success() {
            let t = resp.text().await.unwrap_or_default();
            return Err(format!("http {status}: {t}").into());
        }
        // `chunk()` 增量读 body(无需 reqwest "stream" feature),按 \n 切出完整 SSE 行。
        // **逐块 idle 超时**(根因修复):流中途卡住(无数据/SSE 不发 `[DONE]`)→ 中止,
        // 不令 reason 节点永久冻结;正常慢流每块刷新计时,不误杀。
        let mut buf = String::new();
        loop {
            let next =
                tokio::time::timeout(dur, resp.chunk())
                    .await
                    .map_err(|_| -> ProviderError {
                        "stream stalled (no data within idle timeout)".into()
                    })??;
            let Some(bytes) = next else { break };
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

fn form_component(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    encoded
}
