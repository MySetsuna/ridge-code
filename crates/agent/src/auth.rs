use crate::config::{config_add_provider, config_set, ProviderProfile};

// ───────────────────────── 内置供应商 preset + auth 密钥库(iter-37)─────────────────────────

/// 一条内置供应商预设:选它 + 填一把 key 即接入,免手敲 base_url/kind。纯静态数据,编进二进制。
#[derive(Debug, Clone, Copy)]
pub struct ProviderPreset {
    /// 短 id(命令里用,如 `login deepseek`)。
    pub id: &'static str,
    /// 人读名。
    pub label: &'static str,
    /// `openai`(兼容端点)| `anthropic`。
    pub kind: &'static str,
    pub base_url: &'static str,
    /// 该家一个稳妥的默认模型(用户可随时 `--model` 或 `/model` 改)。
    pub default_model: &'static str,
    /// 约定的密钥环境变量名 —— 也是 `auth.json` 里存该家 key 的槽名。
    pub key_env: &'static str,
}

/// 内置供应商清单:世界顶级 + 中国顶级 + 知名聚合。绝大多数是 OpenAI 兼容端点,Claude 走原生。
/// **优先级即接入便捷度的落点**;`login <id>` 据此一键成档。
pub const PROVIDER_PRESETS: &[ProviderPreset] = &[
    // ── 世界顶级 ──
    ProviderPreset {
        id: "openai",
        label: "OpenAI",
        kind: "openai",
        base_url: "https://api.openai.com/v1",
        default_model: "gpt-4o",
        key_env: "OPENAI_API_KEY",
    },
    ProviderPreset {
        id: "anthropic",
        label: "Anthropic Claude",
        kind: "anthropic",
        base_url: "https://api.anthropic.com/v1",
        default_model: "claude-sonnet-4-6",
        key_env: "ANTHROPIC_API_KEY",
    },
    ProviderPreset {
        id: "gemini",
        label: "Google Gemini",
        kind: "openai",
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        default_model: "gemini-2.0-flash",
        key_env: "GEMINI_API_KEY",
    },
    ProviderPreset {
        id: "grok",
        label: "xAI Grok",
        kind: "openai",
        base_url: "https://api.x.ai/v1",
        default_model: "grok-2-latest",
        key_env: "XAI_API_KEY",
    },
    // ── 中国顶级 ──
    ProviderPreset {
        id: "glm",
        label: "Zhipu GLM (智谱)",
        kind: "openai",
        base_url: "https://open.bigmodel.cn/api/paas/v4",
        default_model: "glm-4.6",
        key_env: "ZHIPU_API_KEY",
    },
    ProviderPreset {
        id: "kimi",
        label: "Moonshot Kimi (月之暗面)",
        kind: "openai",
        base_url: "https://api.moonshot.cn/v1",
        default_model: "kimi-k2",
        key_env: "MOONSHOT_API_KEY",
    },
    ProviderPreset {
        id: "deepseek",
        label: "DeepSeek (深度求索)",
        kind: "openai",
        base_url: "https://api.deepseek.com/v1",
        default_model: "deepseek-chat",
        key_env: "DEEPSEEK_API_KEY",
    },
    ProviderPreset {
        id: "qwen",
        label: "Alibaba Qwen / DashScope (通义千问)",
        kind: "openai",
        base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        default_model: "qwen-max",
        key_env: "DASHSCOPE_API_KEY",
    },
    ProviderPreset {
        id: "hunyuan",
        label: "Tencent Hunyuan (腾讯混元)",
        kind: "openai",
        base_url: "https://api.hunyuan.cloud.tencent.com/v1",
        default_model: "hunyuan-turbo",
        key_env: "HUNYUAN_API_KEY",
    },
    ProviderPreset {
        id: "minimax",
        label: "MiniMax (稀宇)",
        kind: "openai",
        base_url: "https://api.minimax.chat/v1",
        default_model: "MiniMax-Text-01",
        key_env: "MINIMAX_API_KEY",
    },
    // ── 知名聚合 ──
    ProviderPreset {
        id: "openrouter",
        label: "OpenRouter (聚合)",
        kind: "openai",
        base_url: "https://openrouter.ai/api/v1",
        default_model: "anthropic/claude-3.5-sonnet",
        key_env: "OPENROUTER_API_KEY",
    },
    ProviderPreset {
        id: "siliconflow",
        label: "SiliconFlow (硅基流动)",
        kind: "openai",
        base_url: "https://api.siliconflow.cn/v1",
        default_model: "deepseek-ai/DeepSeek-V3",
        key_env: "SILICONFLOW_API_KEY",
    },
    ProviderPreset {
        id: "together",
        label: "Together AI (聚合)",
        kind: "openai",
        base_url: "https://api.together.xyz/v1",
        default_model: "meta-llama/Llama-3.3-70B-Instruct-Turbo",
        key_env: "TOGETHER_API_KEY",
    },
    ProviderPreset {
        id: "groq",
        label: "Groq (聚合/极速)",
        kind: "openai",
        base_url: "https://api.groq.com/openai/v1",
        default_model: "llama-3.3-70b-versatile",
        key_env: "GROQ_API_KEY",
    },
];

/// 按 id 查 preset(大小写不敏感)。未知 → `None`。
pub fn preset_by_id(id: &str) -> Option<&'static ProviderPreset> {
    let id = id.trim().to_lowercase();
    PROVIDER_PRESETS.iter().find(|p| p.id == id)
}

/// preset → `ProviderProfile`(名与 model 可覆盖)。**api_key 恒 None** —— key 只进 auth.json,不入 config。
pub fn preset_to_profile(
    preset: &ProviderPreset,
    name: Option<&str>,
    model: Option<&str>,
) -> ProviderProfile {
    ProviderProfile {
        name: name
            .map(|s| s.to_string())
            .unwrap_or_else(|| preset.id.to_string()),
        kind: preset.kind.to_string(),
        model: model.unwrap_or(preset.default_model).to_string(),
        base_url: preset.base_url.to_string(),
        key_env: preset.key_env.to_string(),
        api_key: None,
        use_oauth: None,
        route: None,
    }
}

/// 解析 `~/.ridge/auth.json` 密钥库文本 → `key_env → key` 映射。坏/空/非对象 → 空表(不崩)。
/// 只收字符串值;OAuth 凭据(对象)另存独立 `oauth.json`(见 [`oauth_parse`]),此处仍跳过任何对象值。
pub fn auth_parse(text: &str) -> std::collections::BTreeMap<String, String> {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Object(m)) => m
            .into_iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
            .collect(),
        _ => std::collections::BTreeMap::new(),
    }
}

/// 往密钥库文本写入/覆盖一把 key(按 `key_env` 名),**保留其余槽**,返回美化 JSON。
/// 文本空/坏 → 从空对象起。纯变换,可单测;写盘 + 收权限由调用方做。
pub fn auth_upsert(text: &str, key_env: &str, key: &str) -> String {
    let mut map = auth_parse(text);
    map.insert(key_env.to_string(), key.to_string());
    let obj: serde_json::Map<String, serde_json::Value> = map
        .into_iter()
        .map(|(k, v)| (k, serde_json::Value::String(v)))
        .collect();
    serde_json::to_string_pretty(&serde_json::Value::Object(obj)).unwrap_or_else(|_| "{}".into())
}

/// 从密钥库文本取某槽的 key。
pub fn auth_get(text: &str, key_env: &str) -> Option<String> {
    auth_parse(text).remove(key_env)
}

/// OAuth 凭据库(iter-43)纯核:`~/.ridge/oauth.json` = `{ provider: OAuthToken }`。
/// **独立于** config.json(key 不进 config)与 auth.json(那是明文字符串 key)。坏/空 → 空表(不崩)。
pub fn oauth_parse(text: &str) -> std::collections::BTreeMap<String, provider::oauth::OAuthToken> {
    serde_json::from_str(text).unwrap_or_default()
}

/// 往 OAuth 库文本写入/覆盖某 provider 的 token,**保留其余**,返回美化 JSON(写盘 + 收权限由调用方做)。
pub fn oauth_upsert(text: &str, provider_id: &str, token: &provider::oauth::OAuthToken) -> String {
    let mut map = oauth_parse(text);
    map.insert(provider_id.to_string(), token.clone());
    serde_json::to_string_pretty(&map).unwrap_or_else(|_| "{}".into())
}

/// 从 OAuth 库文本取某 provider 的 token。
pub fn oauth_get(text: &str, provider_id: &str) -> Option<provider::oauth::OAuthToken> {
    oauth_parse(text).remove(provider_id)
}

/// `login` 的纯核:据 preset 把一个档案加/覆盖进 config 文本的 `providers[]`(经
/// [`config_add_provider`],**产物不含 key**),`make_default` 时再把顶层
/// `provider/model/base_url/key_env` 指向该 preset。key 的落盘由调用方写 auth.json,与此无关。
pub fn apply_login(
    config_text: &str,
    preset: &ProviderPreset,
    name: Option<&str>,
    model: Option<&str>,
    make_default: bool,
) -> Result<String, String> {
    let profile = preset_to_profile(preset, name, model);
    let mut text = config_add_provider(config_text, &profile)?;
    if make_default {
        // 顶层四键指向该 preset;key_env 让启动从 auth.json 取顶层 key(不写明文进 config)。
        text = config_set(&text, "provider", preset.kind)?;
        text = config_set(&text, "model", &profile.model)?;
        text = config_set(&text, "base_url", preset.base_url)?;
        let mut root = match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(serde_json::Value::Object(m)) => m,
            _ => serde_json::Map::new(),
        };
        // key_env 不在 CONFIG_KEYS 白名单(它非用户手调标量),直接对 JSON 对象写。
        root.insert(
            "key_env".to_string(),
            serde_json::Value::String(preset.key_env.to_string()),
        );
        // 抹掉顶层残留内联 api_key —— 否则旧 key 会配新 base_url 认证错乱;新 key 由 key_env→auth 唯一供给。
        root.remove("api_key");
        text = serde_json::to_string_pretty(&serde_json::Value::Object(root))
            .map_err(|e| e.to_string())?;
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::{
        apply_login, auth_get, auth_upsert, oauth_get, oauth_parse, oauth_upsert, preset_by_id,
        preset_to_profile, ProviderProfile, PROVIDER_PRESETS,
    };
    use crate::{resolve_top_level_key, Config};

    /// preset 表结构完好:字段非空、kind 合法、id 唯一、base_url https、含全部要求的 id、条数 ≥ 14。
    #[test]
    fn provider_presets_wellformed() {
        assert!(PROVIDER_PRESETS.len() >= 14);
        let mut ids = std::collections::BTreeSet::new();
        for p in PROVIDER_PRESETS {
            assert!(!p.id.is_empty() && !p.label.is_empty());
            assert!(!p.base_url.is_empty() && !p.default_model.is_empty() && !p.key_env.is_empty());
            assert!(
                p.kind == "openai" || p.kind == "anthropic",
                "bad kind {}",
                p.kind
            );
            assert!(p.base_url.starts_with("https://"), "bad url {}", p.base_url);
            assert!(ids.insert(p.id), "dup id {}", p.id);
        }
        for want in [
            "openai",
            "anthropic",
            "gemini",
            "grok",
            "glm",
            "kimi",
            "deepseek",
            "qwen",
            "openrouter",
            "siliconflow",
            "groq",
        ] {
            assert!(ids.contains(want), "missing preset {want}");
        }
    }

    /// id 查找大小写不敏感;未知 → None;preset → profile 字段对齐且 api_key 恒 None、name/model 可覆盖。
    #[test]
    fn preset_lookup_and_to_profile() {
        let ds = preset_by_id("DeepSeek").expect("deepseek");
        assert!(ds.base_url.contains("deepseek.com"));
        assert!(preset_by_id("nope-vendor").is_none());
        let prof = preset_to_profile(ds, None, None);
        assert_eq!(prof.name, "deepseek");
        assert_eq!(prof.kind, "openai");
        assert_eq!(prof.model, "deepseek-chat");
        assert_eq!(prof.key_env, "DEEPSEEK_API_KEY");
        assert!(prof.api_key.is_none());
        let prof2 = preset_to_profile(ds, Some("work"), Some("deepseek-reasoner"));
        assert_eq!(prof2.name, "work");
        assert_eq!(prof2.model, "deepseek-reasoner");
    }

    /// auth 密钥库往返:写入/覆盖保留余槽、坏文本从空起、产物合法 JSON、可取回。
    #[test]
    fn auth_store_roundtrip() {
        let t1 = auth_upsert("{}", "DEEPSEEK_API_KEY", "sk-a");
        assert_eq!(auth_get(&t1, "DEEPSEEK_API_KEY").as_deref(), Some("sk-a"));
        let t2 = auth_upsert(&t1, "OPENAI_API_KEY", "sk-b");
        assert_eq!(auth_get(&t2, "DEEPSEEK_API_KEY").as_deref(), Some("sk-a")); // 保留
        assert_eq!(auth_get(&t2, "OPENAI_API_KEY").as_deref(), Some("sk-b"));
        let t3 = auth_upsert(&t2, "DEEPSEEK_API_KEY", "sk-c"); // 覆盖
        assert_eq!(auth_get(&t3, "DEEPSEEK_API_KEY").as_deref(), Some("sk-c"));
        // 坏文本从空起,仍产出合法 JSON。
        let t4 = auth_upsert("not json!!", "K", "v");
        assert!(serde_json::from_str::<serde_json::Value>(&t4).is_ok());
        assert_eq!(auth_get(&t4, "K").as_deref(), Some("v"));
        assert!(auth_get(&t4, "MISSING").is_none());
    }

    /// key 解析优先级:内联 api_key > env[key_env] > auth[key_env];皆无 → None。
    /// 用唯一命名的 env 变量避免与并行测试互扰。
    #[test]
    fn resolve_key_precedence_with_auth() {
        use std::collections::BTreeMap;
        // 1) 内联 api_key 压倒一切(env/auth 都不看)。
        let inline = ProviderProfile {
            name: "z".into(),
            kind: "openai".into(),
            model: "m".into(),
            base_url: "u".into(),
            key_env: "RIDGE_ITER37_UNSET".into(),
            api_key: Some(" sk-inline ".into()),
            use_oauth: None,
            route: None,
        };
        let mut auth = BTreeMap::new();
        auth.insert("RIDGE_ITER37_UNSET".to_string(), "sk-auth".to_string());
        assert_eq!(inline.resolve_key_with(&auth).as_deref(), Some("sk-inline"));
        // 2) 无内联、env 未设 → 回落 auth。
        let prof = ProviderProfile {
            api_key: None,
            ..inline.clone()
        };
        assert_eq!(prof.resolve_key_with(&auth).as_deref(), Some("sk-auth"));
        // 3) env 设了(唯一名)→ env 压倒 auth。
        let mut prof2 = prof.clone();
        prof2.key_env = "RIDGE_ITER37_ENVWINS".into();
        let mut auth2 = BTreeMap::new();
        auth2.insert("RIDGE_ITER37_ENVWINS".to_string(), "sk-auth".to_string());
        std::env::set_var("RIDGE_ITER37_ENVWINS", "sk-env");
        assert_eq!(prof2.resolve_key_with(&auth2).as_deref(), Some("sk-env"));
        std::env::remove_var("RIDGE_ITER37_ENVWINS");
        // 4) 皆无 → None。
        assert_eq!(prof.resolve_key_with(&BTreeMap::new()), None);
    }

    /// iter-41:顶层 key 解析收敛核 —— 内联 api_key 优先于 key_env→auth;皆无 → None。
    #[test]
    fn resolve_top_level_key_precedence() {
        use std::collections::BTreeMap;
        // 顶层内联 api_key(trim)优先,不看 key_env/auth。
        let inline =
            Config::parse(r#"{ "api_key": "  sk-top  ", "key_env": "RIDGE_ITER41_UNSET" }"#);
        let mut auth = BTreeMap::new();
        auth.insert("RIDGE_ITER41_UNSET".to_string(), "sk-auth".to_string());
        assert_eq!(
            resolve_top_level_key(&inline, &auth).as_deref(),
            Some("sk-top")
        );
        // 无内联、key_env 指的槽在 auth → 取 auth(env 未设该唯一名)。
        let viaenv = Config::parse(r#"{ "key_env": "RIDGE_ITER41_UNSET" }"#);
        assert_eq!(
            resolve_top_level_key(&viaenv, &auth).as_deref(),
            Some("sk-auth")
        );
        // 无内联、无 key_env、无 env → None。
        let none = Config::parse(r#"{ "model": "m" }"#);
        assert_eq!(resolve_top_level_key(&none, &BTreeMap::new()), None);
    }

    /// iter-43:OAuth 凭据库 upsert→parse→get 身份;空/坏文本 → 空表(不崩)。
    #[test]
    fn oauth_store_roundtrips() {
        let tok = provider::oauth::OAuthToken {
            access_token: "acc".into(),
            refresh_token: "ref".into(),
            expires_at_epoch: 4600,
            id_token: None,
            account_id: None,
        };
        let text = oauth_upsert("", "anthropic", &tok);
        assert_eq!(oauth_get(&text, "anthropic"), Some(tok.clone()));
        // 覆盖同 provider、保留其余。
        let tok2 = provider::oauth::OAuthToken {
            access_token: "acc2".into(),
            ..tok.clone()
        };
        let text2 = oauth_upsert(&text, "anthropic", &tok2);
        assert_eq!(oauth_get(&text2, "anthropic").unwrap().access_token, "acc2");
        // 坏/空文本 → 空表,不 panic。
        assert!(oauth_parse("not json").is_empty());
        assert!(oauth_get("", "anthropic").is_none());
    }

    /// login 纯核:写档进 providers[]、make_default 时改顶层四键、**产物绝不含任何 key**、合法 JSON。
    #[test]
    fn apply_login_writes_profile_no_key() {
        let ds = preset_by_id("deepseek").unwrap();
        // make_default=true:providers 有档 + 顶层指向 deepseek。
        let out = apply_login("{}", ds, None, None, true).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["provider"], "openai");
        assert_eq!(v["model"], "deepseek-chat");
        assert_eq!(v["base_url"], "https://api.deepseek.com/v1");
        assert_eq!(v["key_env"], "DEEPSEEK_API_KEY");
        let prov = &v["providers"][0];
        assert_eq!(prov["name"], "deepseek");
        assert_eq!(prov["base_url"], "https://api.deepseek.com/v1");
        assert_eq!(prov["key_env"], "DEEPSEEK_API_KEY");
        assert!(!out.contains("api_key")); // 铁律:key 永不进 config
                                           // make_default=false:不动顶层,只加档。
        let out2 = apply_login("{}", ds, Some("work"), Some("deepseek-reasoner"), false).unwrap();
        let v2: serde_json::Value = serde_json::from_str(&out2).unwrap();
        assert!(v2.get("provider").is_none());
        assert_eq!(v2["providers"][0]["name"], "work");
        assert_eq!(v2["providers"][0]["model"], "deepseek-reasoner");
        // make_default 抹掉预存顶层 api_key(否则旧 key 配新端点认证错乱)。
        let prev = r#"{"provider":"openai","api_key":"stale-key","base_url":"https://old"}"#;
        let out3 = apply_login(prev, ds, None, None, true).unwrap();
        assert!(!out3.contains("stale-key"));
        assert!(!out3.contains("api_key"));
        let v3: serde_json::Value = serde_json::from_str(&out3).unwrap();
        assert_eq!(v3["key_env"], "DEEPSEEK_API_KEY");
    }
}
