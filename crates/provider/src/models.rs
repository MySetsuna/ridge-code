use super::http::HttpClient;
use super::ProviderError;
use serde_json::Value;

/// 一个模型的最小档:id + (端点提供的)上下文窗口。context 缺失 → None(优雅降级)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: String,
    pub context: Option<u64>,
}

/// 从 `/models` 响应 JSON 抽出模型列表。**纯函数**,离线可测。
/// 兼容:OpenAI/OpenRouter/Anthropic 的 `{"data":[...]}`,以及顶层直接是数组。
/// 坏/空/缺 data → 空列表(不 panic)。
pub fn parse_model_list(v: &Value) -> Vec<ModelInfo> {
    let arr = v
        .get("data")
        .and_then(|d| d.as_array())
        .or_else(|| v.get("models").and_then(|m| m.as_array()))
        .or_else(|| v.as_array());
    let Some(arr) = arr else { return vec![] };
    arr.iter()
        .filter_map(|m| {
            // ChatGPT's Codex endpoint includes hidden rollout entries in the
            // same response. Only `visibility: list` belongs in the picker;
            // ordinary provider responses omit this field.
            if matches!(
                m.get("visibility").and_then(Value::as_str),
                Some(visibility) if visibility != "list"
            ) {
                return None;
            }
            let id = m
                .get("id")
                .or_else(|| m.get("slug"))
                .or_else(|| m.get("model"))
                .and_then(|x| x.as_str())?;
            Some(ModelInfo {
                id: id.to_string(),
                context: extract_context(m),
            })
        })
        .collect()
}

/// 上下文窗口大小的多路探测:各厂商键名不一,依次试;嵌套 `top_provider.context_length`
/// (OpenRouter)也捞。都无 → None。
fn extract_context(m: &Value) -> Option<u64> {
    const KEYS: &[&str] = &["context_length", "context_window", "max_context_length"];
    for k in KEYS {
        if let Some(n) = m.get(*k).and_then(|x| x.as_u64()) {
            return Some(n);
        }
    }
    m.get("top_provider")
        .and_then(|p| p.get("context_length"))
        .and_then(|x| x.as_u64())
}

/// 鉴权 header:openai 兼容用 Bearer;anthropic 用 x-api-key + version。
fn auth_headers(kind: &str, key: &str) -> Vec<(String, String)> {
    match kind {
        "anthropic" => vec![
            ("x-api-key".into(), key.into()),
            ("anthropic-version".into(), "2023-06-01".into()),
        ],
        _ => vec![("Authorization".into(), format!("Bearer {key}"))],
    }
}

/// 抓某 provider 的实时模型列表:GET `{base_url}/models` → [`parse_model_list`]。
/// 抓取走注入的 [`HttpClient`],测试可用替身零网络。
pub async fn fetch_models(
    http: &dyn HttpClient,
    kind: &str,
    base_url: &str,
    key: &str,
) -> Result<Vec<ModelInfo>, ProviderError> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let v = http.get_json(&url, &auth_headers(kind, key)).await?;
    Ok(parse_model_list(&v))
}

/// Fetch the account-scoped model catalog used by ChatGPT-authenticated Codex.
/// The endpoint is `/backend-api/codex/models`, and its response uses `models[]`
/// with `slug` identifiers rather than the public API's `data[].id` shape.
pub const DEFAULT_CHATGPT_CLIENT_VERSION: &str = "0.145.0";

pub async fn fetch_chatgpt_models(
    http: &dyn HttpClient,
    base_url: &str,
    access_token: &str,
    account_id: Option<&str>,
) -> Result<Vec<ModelInfo>, ProviderError> {
    let account_id = account_id.ok_or(
        "ChatGPT OAuth token has no chatgpt_account_id; run ridgecode login --codex again",
    )?;
    let client_version = std::env::var("RIDGE_CODEX_CLIENT_VERSION")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_CHATGPT_CLIENT_VERSION.to_string());
    let url = format!(
        "{}/models?client_version={client_version}",
        base_url.trim_end_matches('/')
    );
    let headers = vec![
        (
            "Authorization".to_string(),
            format!("Bearer {access_token}"),
        ),
        ("ChatGPT-Account-Id".to_string(), account_id.to_string()),
        ("Accept".to_string(), "application/json".to_string()),
        ("originator".to_string(), "codex_cli_rs".to_string()),
    ];
    let v = http.get_json(&url, &headers).await?;
    Ok(parse_model_list(&v))
}
