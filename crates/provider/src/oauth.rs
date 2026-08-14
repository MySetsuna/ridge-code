use super::http::HttpClient;
use super::ProviderError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Anthropic 公开的 Claude Code OAuth client 常量(尽力值;活端点由用户实跑验证)。
pub mod anthropic_oauth {
    pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
    /// Claude Pro/Max **订阅**授权页(区别于 console 的 API-key 档)。
    pub const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
    pub const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
    pub const REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
    pub const SCOPES: &str = "org:create_api_key user:profile user:inference";
    /// 补全侧 OAuth 路径必带的 beta 头值。
    pub const BETA: &str = "oauth-2025-04-20";
    /// OAuth 路径要求 system 首块声明 Claude Code 身份(否则 API 拒);属活验证边界。
    pub const SYSTEM_IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";
}

/// token 端点 body 格式:Anthropic 用 JSON,标准 OAuth(RFC 6749,OpenAI)用 form-urlencoded。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenWire {
    Json,
    Form,
}

/// per-provider OAuth 常量集(iter-48 G1:泛化自 anthropic 硬绑)。活端点由用户实跑验证。
pub struct OAuthConfig {
    /// oauth.json 键名("anthropic"/"openai")。
    pub provider: &'static str,
    pub client_id: &'static str,
    pub authorize_url: &'static str,
    pub token_url: &'static str,
    pub redirect_uri: &'static str,
    pub scopes: &'static str,
    /// authorize URL 前置额外 query(各 provider 的活 OAuth 流所需;无则空串)。
    pub extra_query: &'static str,
    pub token_wire: TokenWire,
}

pub const ANTHROPIC: OAuthConfig = OAuthConfig {
    provider: "anthropic",
    client_id: anthropic_oauth::CLIENT_ID,
    authorize_url: anthropic_oauth::AUTHORIZE_URL,
    token_url: anthropic_oauth::TOKEN_URL,
    redirect_uri: anthropic_oauth::REDIRECT_URI,
    scopes: anthropic_oauth::SCOPES,
    extra_query: "code=true&",
    token_wire: TokenWire::Json,
};

/// ChatGPT Plus/Pro(Codex)订阅(iter-48 G2)。保持官方 Codex CLI 的授权参数;
/// 缺少简化流标记/连接器 scopes 会被 Hydra 判为 invalid_request。
pub const OPENAI: OAuthConfig = OAuthConfig {
    provider: "openai",
    client_id: "app_EMoamEEZ73f0CkXaXp7hrann",
    authorize_url: "https://auth.openai.com/oauth/authorize",
    token_url: "https://auth.openai.com/oauth/token",
    redirect_uri: "http://localhost:1455/auth/callback",
    scopes: "openid profile email offline_access api.connectors.read api.connectors.invoke",
    extra_query:
        "id_token_add_organizations=true&codex_cli_simplified_flow=true&originator=codex_cli_rs&",
    token_wire: TokenWire::Form,
};

/// OpenAI Codex 无本地端口设备授权端点。
pub const OPENAI_DEVICE_USERCODE_URL: &str =
    "https://auth.openai.com/api/accounts/deviceauth/usercode";
pub const OPENAI_DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
pub const OPENAI_DEVICE_VERIFICATION_URL: &str = "https://auth.openai.com/codex/device";
pub const OPENAI_DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";

/// SuperGrok / X Premium device-code login (RFC 8628) against accounts.x.ai.
/// Client id is xAI's shared public agent client used by OpenClaw/Hermes.
pub const XAI: OAuthConfig = OAuthConfig {
    provider: "xai",
    client_id: "b1a00492-073a-47ea-816f-4c329264a828",
    authorize_url: "https://accounts.x.ai/sign-in",
    token_url: "https://accounts.x.ai/oauth2/token",
    redirect_uri: "http://127.0.0.1:54545/callback",
    scopes: "openid profile email offline_access",
    extra_query: "",
    token_wire: TokenWire::Form,
};
pub const XAI_DEVICE_CODE_URL: &str = "https://accounts.x.ai/oauth2/device/code";
pub const XAI_DEVICE_VERIFICATION_URL: &str = "https://accounts.x.ai/oauth2/device";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rfc8628DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub interval_secs: u64,
}

/// 无填充 base64url 编码(RFC 4648 §5;非 crypto,纯编码,手写 + 测,省一依赖)。
pub fn base64url_nopad(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(T[((n >> 6) & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(T[(n & 63) as usize] as char);
        }
    }
    out
}

fn decode_base64url_nopad(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0u8;
    for byte in s.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        } as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        while bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= if bits == 0 { 0 } else { (1u32 << bits) - 1 };
        }
    }
    Some(out)
}

fn jwt_payload(jwt: &str) -> Option<Value> {
    let mut parts = jwt.split('.');
    let (_header, payload, _signature) = (parts.next()?, parts.next()?, parts.next()?);
    if payload.is_empty() {
        return None;
    }
    serde_json::from_slice(&decode_base64url_nopad(payload)?).ok()
}

fn account_id_from_claims(payload: &Value) -> Option<String> {
    let auth = payload
        .get("https://api.openai.com/auth")
        .or_else(|| payload.get("auth"));
    auth.and_then(|claims| claims.get("chatgpt_account_id"))
        .or_else(|| payload.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(ToOwned::to_owned)
}

/// Extract the ChatGPT workspace/account id required by the Codex backend.
/// Only the JWT payload is decoded; signature verification remains the server's job.
pub fn chatgpt_account_id(id_token: Option<&str>, access_token: &str) -> Option<String> {
    id_token
        .into_iter()
        .chain(std::iter::once(access_token))
        .find_map(|token| jwt_payload(token).and_then(|payload| account_id_from_claims(&payload)))
}

/// PKCE 对:`challenge = base64url_nopad(sha256(verifier))`(S256)。
#[derive(Clone, Debug)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    /// 由 verifier **纯派生** challenge(可测)。
    pub fn from_verifier(verifier: impl Into<String>) -> Self {
        use sha2::{Digest, Sha256};
        let verifier = verifier.into();
        let digest = Sha256::digest(verifier.as_bytes());
        Self {
            challenge: base64url_nopad(&digest),
            verifier,
        }
    }

    /// 新随机 PKCE(32 字节 OS 随机 → base64url verifier)。非纯(随机),薄封装。
    pub fn generate() -> Self {
        Self::from_verifier(random_token())
    }
}

/// 32 字节 OS 安全随机 → base64url(PKCE verifier / state 用)。
/// 熵源不可用极罕见;失败即 panic —— 无法安全登录好过用弱随机。
pub fn random_token() -> String {
    let mut buf = [0u8; 32];
    getrandom::getrandom(&mut buf).expect("OS entropy source unavailable");
    base64url_nopad(&buf)
}

/// 存储 / 传递的 OAuth 凭据(serde 落 `oauth.json`)。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthToken {
    pub access_token: String,
    pub refresh_token: String,
    /// 过期时刻(epoch 秒)。刷新判定用它 —— 纯函数 now 传参,不读墙钟。
    pub expires_at_epoch: u64,
    /// OpenAI OAuth returns this JWT; kept for account-id recovery after refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
    /// ChatGPT workspace/account id used by `chatgpt.com/backend-api`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}

/// OpenAI device authorization 的一次性用户码。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceCode {
    pub device_auth_id: String,
    pub user_code: String,
    pub interval_secs: u64,
}

/// 设备流轮询完成后，交给标准 OAuth token 交换的 PKCE 材料。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceAuthorization {
    pub authorization_code: String,
    pub code_challenge: String,
    pub code_verifier: String,
}

impl OAuthToken {
    /// 需要刷新?(含 60s 余量;now 传参保证可测)。
    pub fn needs_refresh(&self, now_epoch: u64) -> bool {
        now_epoch + 60 >= self.expires_at_epoch
    }

    pub fn preserve_chatgpt_metadata_from(&mut self, previous: &Self) {
        self.id_token = self.id_token.clone().or_else(|| previous.id_token.clone());
        self.account_id = self
            .account_id
            .clone()
            .or_else(|| previous.account_id.clone())
            .or_else(|| chatgpt_account_id(self.id_token.as_deref(), &self.access_token));
    }
}

/// 构造订阅授权 URL(纯;per-provider 经 [`OAuthConfig`])。用户浏览器打开授权,
/// 回调页给出 `code#state`(anthropic)或重定向到本地回调(openai)。
pub fn authorize_url(cfg: &OAuthConfig, challenge: &str, state: &str) -> String {
    authorize_url_with_redirect(cfg, challenge, state, cfg.redirect_uri)
}

/// Build an authorization URL while selecting a registered localhost fallback port.
pub fn authorize_url_with_redirect(
    cfg: &OAuthConfig,
    challenge: &str,
    state: &str,
    redirect_uri: &str,
) -> String {
    let scope_enc = cfg.scopes.replace(' ', "%20");
    let redirect_enc = redirect_uri.replace(':', "%3A").replace('/', "%2F");
    format!(
        "{}?{}client_id={}&response_type=code\
             &redirect_uri={redirect_enc}&scope={scope_enc}\
             &code_challenge={challenge}&code_challenge_method=S256&state={state}",
        cfg.authorize_url, cfg.extra_query, cfg.client_id
    )
}

/// 从 token 端点 JSON 解析 [`OAuthToken`](纯;`expires_at = now + expires_in`)。
/// 刷新响应可能不带新 refresh_token → 回落用旧的(`fallback_refresh`)。
pub fn parse_token_response(
    v: &Value,
    now_epoch: u64,
    fallback_refresh: Option<&str>,
) -> Result<OAuthToken, ProviderError> {
    let access_token = v["access_token"]
        .as_str()
        .ok_or("token response missing access_token")?
        .to_string();
    let refresh_token = v["refresh_token"]
        .as_str()
        .map(str::to_string)
        .or_else(|| fallback_refresh.map(str::to_string))
        .ok_or("token response missing refresh_token")?;
    let expires_in = v["expires_in"].as_u64().unwrap_or(3600);
    let id_token = v["id_token"].as_str().map(str::to_string);
    let account_id = chatgpt_account_id(id_token.as_deref(), &access_token);
    Ok(OAuthToken {
        access_token,
        refresh_token,
        expires_at_epoch: now_epoch + expires_in,
        account_id,
        id_token,
    })
}

/// 授权码换 token(HTTP 走接缝)。`code_and_state` 为回调给的 `code#state`,拆开回填。
/// body 格式按 `cfg.token_wire` 分流:Json(anthropic,带 state)/ Form(标准 OAuth,无 state)。
pub async fn exchange_code(
    http: &dyn HttpClient,
    cfg: &OAuthConfig,
    code_and_state: &str,
    verifier: &str,
    now_epoch: u64,
) -> Result<OAuthToken, ProviderError> {
    exchange_code_with_redirect(
        http,
        cfg,
        code_and_state,
        verifier,
        cfg.redirect_uri,
        now_epoch,
    )
    .await
}

/// 授权码换 token,允许设备授权流覆盖 redirect URI。
pub async fn exchange_code_with_redirect(
    http: &dyn HttpClient,
    cfg: &OAuthConfig,
    code_and_state: &str,
    verifier: &str,
    redirect_uri: &str,
    now_epoch: u64,
) -> Result<OAuthToken, ProviderError> {
    let (code, state) = split_code_state(code_and_state);
    let v = match cfg.token_wire {
        TokenWire::Json => {
            let body = json!({
                "grant_type": "authorization_code",
                "code": code,
                "state": state,
                "client_id": cfg.client_id,
                "redirect_uri": redirect_uri,
                "code_verifier": verifier,
            });
            http.post_json(cfg.token_url, &json_headers(), &body)
                .await?
        }
        TokenWire::Form => {
            let form = [
                ("grant_type", "authorization_code"),
                ("code", code.as_str()),
                ("client_id", cfg.client_id),
                ("redirect_uri", redirect_uri),
                ("code_verifier", verifier),
            ];
            http.post_form(cfg.token_url, &form).await?
        }
    };
    parse_token_response(&v, now_epoch, None)
}

/// 请求 OpenAI device authorization 用户码。
pub async fn request_device_code(
    http: &dyn HttpClient,
    client_id: &str,
) -> Result<DeviceCode, ProviderError> {
    let v = http
        .post_json(
            OPENAI_DEVICE_USERCODE_URL,
            &json_headers(),
            &json!({ "client_id": client_id }),
        )
        .await?;
    let device_auth_id = v["device_auth_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or("device auth response missing device_auth_id")?
        .to_string();
    let user_code = v
        .get("user_code")
        .or_else(|| v.get("usercode"))
        .and_then(Value::as_str)
        .filter(|code| !code.is_empty())
        .ok_or("device auth response missing user_code")?
        .to_string();
    let interval_secs = json_u64(&v, "interval").unwrap_or(5).max(1);
    Ok(DeviceCode {
        device_auth_id,
        user_code,
        interval_secs,
    })
}

/// 轮询 OpenAI device authorization,直至用户完成授权或 15 分钟超时。
pub async fn poll_device_code(
    http: &dyn HttpClient,
    device: &DeviceCode,
) -> Result<DeviceAuthorization, ProviderError> {
    let started = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(15 * 60);
    loop {
        let response = http
            .post_json(
                OPENAI_DEVICE_TOKEN_URL,
                &json_headers(),
                &json!({
                    "device_auth_id": device.device_auth_id,
                    "user_code": device.user_code,
                }),
            )
            .await;
        match response {
            Ok(v) => {
                let authorization_code = v["authorization_code"]
                    .as_str()
                    .filter(|code| !code.is_empty())
                    .ok_or("device auth response missing authorization_code")?
                    .to_string();
                let code_challenge = v["code_challenge"]
                    .as_str()
                    .filter(|challenge| !challenge.is_empty())
                    .ok_or("device auth response missing code_challenge")?
                    .to_string();
                let code_verifier = v["code_verifier"]
                    .as_str()
                    .filter(|verifier| !verifier.is_empty())
                    .ok_or("device auth response missing code_verifier")?
                    .to_string();
                return Ok(DeviceAuthorization {
                    authorization_code,
                    code_challenge,
                    code_verifier,
                });
            }
            Err(error) if is_device_pending(&error) => {
                if started.elapsed() >= timeout {
                    return Err("device auth timed out after 15 minutes".into());
                }
                let remaining = timeout.saturating_sub(started.elapsed());
                tokio::time::sleep(
                    std::time::Duration::from_secs(device.interval_secs).min(remaining),
                )
                .await;
            }
            Err(error) => return Err(error),
        }
    }
}

fn json_u64(value: &Value, key: &str) -> Option<u64> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .or_else(|| value.get(key).and_then(Value::as_str)?.trim().parse().ok())
}

fn is_device_pending(error: &ProviderError) -> bool {
    let text = error.to_string();
    text.starts_with("http 403") || text.starts_with("http 404")
}

/// refresh_token 换新 token(HTTP 走接缝;body 格式同 exchange 按 wire 分流)。
pub async fn refresh(
    http: &dyn HttpClient,
    cfg: &OAuthConfig,
    refresh_token: &str,
    now_epoch: u64,
) -> Result<OAuthToken, ProviderError> {
    let v = match cfg.token_wire {
        TokenWire::Json => {
            let body = json!({
                "grant_type": "refresh_token",
                "refresh_token": refresh_token,
                "client_id": cfg.client_id,
            });
            http.post_json(cfg.token_url, &json_headers(), &body)
                .await?
        }
        TokenWire::Form => {
            let form = [
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("client_id", cfg.client_id),
            ];
            http.post_form(cfg.token_url, &form).await?
        }
    };
    parse_token_response(&v, now_epoch, Some(refresh_token))
}

pub fn parse_rfc8628_device_response(v: &Value) -> Result<Rfc8628DeviceCode, ProviderError> {
    let device_code = v["device_code"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or("device code response missing device_code")?
        .to_string();
    let user_code = v["user_code"]
        .as_str()
        .filter(|code| !code.is_empty())
        .ok_or("device code response missing user_code")?
        .to_string();
    let verification_uri = v
        .get("verification_uri_complete")
        .or_else(|| v.get("verification_uri"))
        .and_then(Value::as_str)
        .filter(|uri| !uri.is_empty())
        .unwrap_or(XAI_DEVICE_VERIFICATION_URL)
        .to_string();
    let interval_secs = json_u64(v, "interval").unwrap_or(5).max(1);
    Ok(Rfc8628DeviceCode {
        device_code,
        user_code,
        verification_uri,
        interval_secs,
    })
}

pub fn is_rfc8628_pending(error: &ProviderError) -> bool {
    let text = error.to_string();
    text.contains("authorization_pending") || text.contains("slow_down")
}

pub async fn request_rfc8628_device_code(
    http: &dyn HttpClient,
    cfg: &OAuthConfig,
    device_code_url: &str,
) -> Result<Rfc8628DeviceCode, ProviderError> {
    let form = [("client_id", cfg.client_id), ("scope", cfg.scopes)];
    parse_rfc8628_device_response(&http.post_form(device_code_url, &form).await?)
}

pub async fn poll_rfc8628_device_token(
    http: &dyn HttpClient,
    cfg: &OAuthConfig,
    device: &Rfc8628DeviceCode,
    now_epoch: u64,
) -> Result<OAuthToken, ProviderError> {
    let started = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(15 * 60);
    let grant = "urn:ietf:params:oauth:grant-type:device_code";
    loop {
        if started.elapsed() > timeout {
            return Err("device authorization timed out".into());
        }
        let form = [
            ("grant_type", grant),
            ("device_code", device.device_code.as_str()),
            ("client_id", cfg.client_id),
        ];
        match http.post_form(cfg.token_url, &form).await {
            Ok(value) => return parse_token_response(&value, now_epoch, None),
            Err(error) if is_rfc8628_pending(&error) => {
                let wait = if error.to_string().contains("slow_down") {
                    device.interval_secs.saturating_add(5)
                } else {
                    device.interval_secs
                };
                tokio::time::sleep(std::time::Duration::from_secs(wait.max(1))).await;
            }
            Err(error) => return Err(error),
        }
    }
}

fn json_headers() -> Vec<(String, String)> {
    vec![("Content-Type".to_string(), "application/json".to_string())]
}

/// 解析本地回调请求行/路径,提取 `(code, state)`(纯;G2 openai 本地回调用)。
/// 收整个 HTTP 请求首行(`GET /auth/callback?code=X&state=Y HTTP/1.1`)或裸 path 皆可。
/// code/state 为 URL-safe 令牌,不做 percent-decode。无 code → None。
pub fn parse_callback_path(line: &str) -> Option<(String, String)> {
    let path = line
        .strip_prefix("GET ")
        .map(|r| r.split(' ').next().unwrap_or(r))
        .unwrap_or(line);
    let query = path.split_once('?')?.1;
    let mut code = None;
    let mut state = String::new();
    for kv in query.split('&') {
        match kv.split_once('=') {
            Some(("code", v)) if !v.is_empty() => code = Some(v.to_string()),
            Some(("state", v)) => state = v.to_string(),
            _ => {}
        }
    }
    code.map(|c| (c, state))
}

/// Parse a browser callback URL or code#state, preserving validated state.
pub fn parse_authorization_input(
    input: &str,
    expected_state: &str,
) -> Result<String, ProviderError> {
    let input = input.trim();
    let (code, state) = parse_callback_path(input).unwrap_or_else(|| split_code_state(input));
    if code.is_empty() {
        return Err("OAuth authorization input has no code".into());
    }
    if state.is_empty() || state != expected_state {
        return Err("OAuth state mismatch; restart login".into());
    }
    Ok(format!("{code}#{state}"))
}

/// 回调页返回 `code#state`;拆成 (code, state)。无 `#` 则 state 空。
fn split_code_state(s: &str) -> (String, String) {
    match s.trim().split_once('#') {
        Some((c, st)) => (c.to_string(), st.to_string()),
        None => (s.trim().to_string(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::{is_rfc8628_pending, parse_rfc8628_device_response, XAI};
    use serde_json::json;

    #[test]
    fn rfc8628_device_response_reads_codes_and_interval() {
        let parsed = parse_rfc8628_device_response(&json!({
            "device_code": "dev-1",
            "user_code": "ABCD-EFGH",
            "verification_uri": "https://accounts.x.ai/oauth2/device",
            "interval": 7
        }))
        .expect("device response");
        assert_eq!(parsed.device_code, "dev-1");
        assert_eq!(parsed.user_code, "ABCD-EFGH");
        assert_eq!(parsed.interval_secs, 7);
        assert!(parsed.verification_uri.contains("accounts.x.ai"));
        assert_eq!(XAI.provider, "xai");
        assert!(is_rfc8628_pending(
            &"http 400: {\"error\":\"authorization_pending\"}".into()
        ));
    }
}
