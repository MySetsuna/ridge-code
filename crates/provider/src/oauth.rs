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
}

impl OAuthToken {
    /// 需要刷新?(含 60s 余量;now 传参保证可测)。
    pub fn needs_refresh(&self, now_epoch: u64) -> bool {
        now_epoch + 60 >= self.expires_at_epoch
    }
}

/// 构造 Anthropic 订阅授权 URL(纯)。用户浏览器打开它授权,回调页给出 `code#state`。
pub fn authorize_url(challenge: &str, state: &str) -> String {
    use anthropic_oauth::*;
    let scope_enc = SCOPES.replace(' ', "%20");
    let redirect_enc = REDIRECT_URI.replace(':', "%3A").replace('/', "%2F");
    format!(
        "{AUTHORIZE_URL}?code=true&client_id={CLIENT_ID}&response_type=code\
             &redirect_uri={redirect_enc}&scope={scope_enc}\
             &code_challenge={challenge}&code_challenge_method=S256&state={state}"
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
    Ok(OAuthToken {
        access_token,
        refresh_token,
        expires_at_epoch: now_epoch + expires_in,
    })
}

/// 授权码换 token(HTTP 走接缝)。`code_and_state` 为回调页给的 `code#state`,拆开回填。
pub async fn exchange_code(
    http: &dyn HttpClient,
    code_and_state: &str,
    verifier: &str,
    now_epoch: u64,
) -> Result<OAuthToken, ProviderError> {
    use anthropic_oauth::*;
    let (code, state) = split_code_state(code_and_state);
    let body = json!({
        "grant_type": "authorization_code",
        "code": code,
        "state": state,
        "client_id": CLIENT_ID,
        "redirect_uri": REDIRECT_URI,
        "code_verifier": verifier,
    });
    let v = http.post_json(TOKEN_URL, &json_headers(), &body).await?;
    parse_token_response(&v, now_epoch, None)
}

/// refresh_token 换新 token(HTTP 走接缝)。
pub async fn refresh(
    http: &dyn HttpClient,
    refresh_token: &str,
    now_epoch: u64,
) -> Result<OAuthToken, ProviderError> {
    use anthropic_oauth::*;
    let body = json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "client_id": CLIENT_ID,
    });
    let v = http.post_json(TOKEN_URL, &json_headers(), &body).await?;
    parse_token_response(&v, now_epoch, Some(refresh_token))
}

fn json_headers() -> Vec<(String, String)> {
    vec![("Content-Type".to_string(), "application/json".to_string())]
}

/// 回调页返回 `code#state`;拆成 (code, state)。无 `#` 则 state 空。
fn split_code_state(s: &str) -> (String, String) {
    match s.trim().split_once('#') {
        Some((c, st)) => (c.to_string(), st.to_string()),
        None => (s.trim().to_string(), String::new()),
    }
}
