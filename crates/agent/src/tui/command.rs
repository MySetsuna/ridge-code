use super::*;

fn profile_for_runtime<'a>(
    cfg: &'a Config,
    provider: &str,
    base_url: &str,
) -> Option<&'a agent::ProviderProfile> {
    let matches_runtime = |profile: &&agent::ProviderProfile| {
        profile.kind.eq_ignore_ascii_case(provider) && same_endpoint(&profile.base_url, base_url)
    };
    cfg.provider
        .as_deref()
        .and_then(|selected| {
            cfg.providers
                .iter()
                .find(|profile| profile.name.eq_ignore_ascii_case(selected))
        })
        .filter(matches_runtime)
        .or_else(|| cfg.providers.iter().find(matches_runtime))
}

pub(crate) fn named_profile_name(cfg: &Config, selection: &str) -> Option<String> {
    cfg.providers
        .iter()
        .find(|profile| profile.name.eq_ignore_ascii_case(selection.trim()))
        .map(|profile| profile.name.clone())
}

fn api_key_for_runtime(
    cfg: &Config,
    auth: &std::collections::BTreeMap<String, String>,
    provider: &str,
    base_url: &str,
) -> Option<String> {
    match profile_for_runtime(cfg, provider, base_url) {
        Some(profile) => profile.resolve_key_with(auth),
        None => resolve_top_level_key(cfg, auth),
    }
}

fn openai_oauth_token() -> Option<provider::oauth::OAuthToken> {
    let text = std::fs::read_to_string(oauth_path()).ok()?;
    agent::oauth_get(&text, "openai")
}

pub(crate) const CHATGPT_MODEL_GROUP: &str = "ChatGPT (Codex)";
pub(crate) const EFFORT_MODEL_GROUP: &str = "Effort";

fn current_effort(ui: &Ui) -> &str {
    ui.effort
        .as_deref()
        .and_then(provider::normalize_reasoning_effort)
        .unwrap_or(provider::DEFAULT_REASONING_EFFORT)
}

fn same_endpoint(left: &str, right: &str) -> bool {
    left.trim_end_matches('/')
        .eq_ignore_ascii_case(right.trim_end_matches('/'))
}

fn active_profile_name(provider: &str, base_url: &str) -> String {
    let cfg = Config::load(config_path());
    let matches_runtime = |profile: &&agent::ProviderProfile| {
        profile.kind.eq_ignore_ascii_case(provider) && same_endpoint(&profile.base_url, base_url)
    };
    cfg.provider
        .as_deref()
        .and_then(|selected| {
            cfg.providers
                .iter()
                .find(|profile| profile.name.eq_ignore_ascii_case(selected))
        })
        .filter(matches_runtime)
        .or_else(|| cfg.providers.iter().find(matches_runtime))
        .map(|profile| profile.name.clone())
        .unwrap_or_else(|| provider.to_string())
}

fn refresh_provider_label(meta: &mut ReplMeta) {
    meta.provider_label = active_profile_name(&meta.provider, &meta.base_url);
}

fn persist_default_selection(ui: &mut Ui, provider: &str, model: &str, base_url: &str) {
    let path = config_path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let selection = active_profile_name(provider, base_url);
    let updated = match agent::config_set_selection(&text, &selection, model, base_url) {
        Ok(updated) => updated,
        Err(error) => {
            ui.note(
                format!("model switched, but default selection was not saved: {error}"),
                Color::Yellow,
            );
            return;
        }
    };
    let result = (|| {
        if let Some(dir) = std::path::Path::new(&path).parent() {
            std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
        }
        std::fs::write(&path, updated).map_err(|error| error.to_string())
    })();
    if let Err(error) = result {
        ui.note(
            format!("model switched, but default selection was not saved: {error}"),
            Color::Yellow,
        );
    }
}

struct ModelTarget {
    name: String,
    kind: String,
    base_url: String,
    key: String,
    oauth: bool,
    account_id: Option<String>,
}

pub(crate) fn model_group_name(provider: &str, base_url: &str) -> String {
    let oauth_base = std::env::var("RIDGE_CHATGPT_BASE_URL")
        .unwrap_or_else(|_| oauth_defaults("openai").1.to_string());
    if provider == "openai" && base_url.trim_end_matches('/') == oauth_base.trim_end_matches('/') {
        CHATGPT_MODEL_GROUP.to_string()
    } else {
        provider.to_string()
    }
}

fn build_model_targets(
    cfg: &Config,
    auth: &std::collections::BTreeMap<String, String>,
    active_provider: &str,
    active_base_url: &str,
) -> Vec<ModelTarget> {
    let mut targets = Vec::new();
    let oauth_base = std::env::var("RIDGE_CHATGPT_BASE_URL")
        .unwrap_or_else(|_| oauth_defaults("openai").1.to_string());
    let active_is_oauth = active_provider == "openai"
        && active_base_url.trim_end_matches('/') == oauth_base.trim_end_matches('/');
    if active_is_oauth {
        if let Some(token) = openai_oauth_token() {
            targets.push(ModelTarget {
                name: CHATGPT_MODEL_GROUP.to_string(),
                kind: "openai".to_string(),
                base_url: active_base_url.to_string(),
                key: token.access_token,
                oauth: true,
                account_id: token.account_id,
            });
        }
    } else if let Some(key) = api_key_for_runtime(cfg, auth, active_provider, active_base_url) {
        targets.push(ModelTarget {
            name: active_provider.to_string(),
            kind: active_provider.to_string(),
            base_url: active_base_url.to_string(),
            key,
            oauth: false,
            account_id: None,
        });
    }
    for profile in &cfg.providers {
        if profile.use_oauth == Some(true) && profile.kind == "openai" {
            if !targets.iter().any(|target| target.oauth) {
                if let Some(token) = openai_oauth_token() {
                    targets.push(ModelTarget {
                        name: CHATGPT_MODEL_GROUP.to_string(),
                        kind: "openai".to_string(),
                        base_url: oauth_base.clone(),
                        key: token.access_token,
                        oauth: true,
                        account_id: token.account_id,
                    });
                }
            }
        } else if let Some(key) = profile.resolve_key_with(auth) {
            if !targets.iter().any(|target| target.name == profile.name) {
                targets.push(ModelTarget {
                    name: profile.name.clone(),
                    kind: profile.kind.clone(),
                    base_url: profile.base_url.clone(),
                    key,
                    oauth: false,
                    account_id: None,
                });
            }
        }
    }
    targets
}

async fn fetch_model_catalog(targets: Vec<ModelTarget>) -> (ModelCatalog, u32) {
    let jobs = targets
        .into_iter()
        .map(|target| {
            tokio::spawn(async move {
                let ModelTarget {
                    name,
                    kind,
                    base_url,
                    key,
                    oauth,
                    account_id,
                } = target;
                let http = provider::http::ReqwestClient::new();
                let result = tokio::time::timeout(Duration::from_secs(10), async {
                    if oauth {
                        provider::models::fetch_chatgpt_models(
                            &http,
                            &base_url,
                            &key,
                            account_id.as_deref(),
                        )
                        .await
                    } else {
                        provider::models::fetch_models(&http, &kind, &base_url, &key).await
                    }
                })
                .await;
                let models = match result {
                    Ok(Ok(models)) if !models.is_empty() => Some(models),
                    _ => None,
                };
                (name, models)
            })
        })
        .collect::<Vec<_>>();
    let mut grouped = Vec::new();
    let mut failures = 0;
    for job in jobs {
        match job.await {
            Ok((name, Some(models))) => grouped.push((name, models)),
            Ok((_, None)) | Err(_) => failures += 1,
        }
    }
    (grouped, failures)
}

pub(crate) fn start_model_catalog_preload(
    active_provider: &str,
    active_base_url: &str,
) -> tokio::sync::oneshot::Receiver<(ModelCatalog, u32)> {
    let cfg = Config::load(config_path());
    let auth = load_auth();
    let active_provider = active_provider.to_string();
    let active_base_url = active_base_url.to_string();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let targets = build_model_targets(&cfg, &auth, &active_provider, &active_base_url);
        let result = fetch_model_catalog(targets).await;
        let _ = sender.send(result);
    });
    receiver
}

pub(crate) fn auto_select_chatgpt_model(
    grouped: &ModelCatalog,
    meta: &mut ReplMeta,
    swap: &Arc<SwapProvider>,
    ui: &mut Ui,
) {
    let oauth_base = std::env::var("RIDGE_CHATGPT_BASE_URL")
        .unwrap_or_else(|_| oauth_defaults("openai").1.to_string());
    if meta.provider != "openai"
        || meta.base_url.trim_end_matches('/') != oauth_base.trim_end_matches('/')
        || meta.model != oauth_defaults("openai").0
    {
        return;
    }
    let Some(model) = grouped
        .iter()
        .find(|(name, _)| name == CHATGPT_MODEL_GROUP)
        .and_then(|(_, models)| models.first())
    else {
        return;
    };
    if model.id == meta.model {
        meta.ctx_window = model.context.unwrap_or(tui::DEFAULT_CTX_WINDOW);
    } else {
        if let Some(context) = model.context {
            meta.ctx_window = context;
        }
        swap_model(swap, meta, &model.id, ui);
    }
}

/// TUI `/login` 落盘核(与 CLI `run_login` 同语义,精简版):key → auth.json(收权限),
/// 档案 + 顶层默认 → config.json。**key 不回显、不进 config**。热切由调用方做。
pub(crate) fn tui_login(preset: &agent::ProviderPreset, key: &str) -> Result<(), String> {
    let apath = auth_path();
    if let Some(dir) = std::path::Path::new(&apath).parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let atext = std::fs::read_to_string(&apath).unwrap_or_default();
    std::fs::write(&apath, auth_upsert(&atext, preset.key_env, key)).map_err(|e| e.to_string())?;
    secure_file(&apath);
    let cpath = config_path();
    let ctext = std::fs::read_to_string(&cpath).unwrap_or_default();
    let out = apply_login(&ctext, preset, None, None, true)?;
    std::fs::write(&cpath, out).map_err(|e| e.to_string())
}

/// TUI 登录落地(iter-38):**先校验连通** → 成功则写 auth+config(`tui_login`)+ 热切 + note ✓ + 关页;
/// 失败 note ✗ 不落盘。共用于 `/login <id> <key>` 快捷路径与登录页提交。校验期短暂阻塞在 await
/// (有效 key 通常 <2s,15s 仅超时上限)。ponytail: 需非阻塞再引入后台校验任务通道。
pub(crate) async fn login_apply_verified(
    preset: &agent::ProviderPreset,
    key: &str,
    meta: &mut ReplMeta,
    swap: &Arc<SwapProvider>,
    ui: &mut Ui,
) {
    match verify_provider_key(preset.kind, preset.base_url, key).await {
        Ok(n) => match tui_login(preset, key) {
            Ok(()) => {
                swap.swap(make_provider(
                    preset.kind,
                    preset.default_model,
                    preset.base_url,
                    key.to_string(),
                ));
                meta.provider = preset.kind.to_string();
                meta.model = preset.default_model.to_string();
                meta.base_url = preset.base_url.to_string();
                refresh_provider_label(meta);
                ui.panel = None;
                ui.note(
                    format!(
                        "✓ connected to {} ({n} models) · now active (model {})",
                        preset.label, preset.default_model
                    ),
                    Color::Green,
                );
            }
            Err(e) => ui.note(format!("verified but write failed: {e}"), Color::Red),
        },
        Err(e) => ui.note(
            format!("✗ could not connect to {}: {e}", preset.label),
            Color::Red,
        ),
    }
}

/// 订阅 OAuth 起步:先起本地回调 listener,再自动打开浏览器;收到回调后主循环自动换 token。
pub(crate) fn begin_oauth(ui: &mut Ui, ocfg: &provider::oauth::OAuthConfig) {
    let pkce = provider::oauth::Pkce::generate();
    let state = provider::oauth::random_token();
    let callback = if ocfg.token_wire == provider::oauth::TokenWire::Form {
        match start_local_callback(state.clone()) {
            Ok(callback) => Some(callback),
            Err(error) => {
                if ocfg.provider == "openai" {
                    ui.device_auth_status = Some("Requesting device code...".into());
                    ui.oauth_device = Some(start_device_oauth());
                    if let Some(panel) = ui.panel.as_mut() {
                        panel.editing = None;
                        panel.title =
                            "Codex OAuth · device auth · browser will open · Esc cancel".into();
                    }
                    ui.note(
                        format!(
                            "OAuth localhost callback unavailable: {error}. Switching to device auth; browser will open automatically."
                        ),
                        Color::Yellow,
                    );
                    return;
                }
                ui.note(
                    format!(
                        "OAuth local callback unavailable: {error}. Manual callback paste remains available."
                    ),
                    Color::Yellow,
                );
                None
            }
        }
    } else {
        None
    };
    let redirect_uri = callback
        .as_ref()
        .map(|c| c.redirect_uri.clone())
        .unwrap_or_else(|| ocfg.redirect_uri.to_string());
    let url =
        provider::oauth::authorize_url_with_redirect(ocfg, &pkce.challenge, &state, &redirect_uri);
    ui.oauth_callback = callback;
    if let Some(panel) = ui.panel.as_mut() {
        panel.oauth_verifier = Some(pkce.verifier);
        panel.oauth_state = Some(state.clone());
        panel.oauth_redirect_uri = Some(redirect_uri);
        panel.editing = Some(String::new());
        panel.title = format!(
            "{} OAuth · paste code · Enter connect · Esc cancel",
            ocfg.provider
        );
    }
    let hint = if ui.oauth_callback.is_some() {
        "2. Authorize in the browser; RidgeCode will receive the localhost callback and connect automatically."
    } else if ocfg.token_wire == provider::oauth::TokenWire::Form {
        "2. After authorizing, paste the FULL localhost callback URL here and press Enter (fallback)."
    } else {
        "2. Paste the returned code#state here and press Enter."
    };
    // 页面即触发:best-effort 自动开系统浏览器(失败仍显 URL 手动打开)。
    let opened = open_in_browser(&url);
    let lead = if opened {
        "1. Browser opened — authorize there (URL below as fallback):"
    } else {
        "1. Open this URL in your browser:"
    };
    ui.note(
        format!("{} OAuth:\n{lead}\n{url}\n{hint}", ocfg.provider),
        Color::Cyan,
    );
}

/// 用系统默认浏览器开 URL(best-effort;detach 不等待)。授权 URL 无空格/引号,直接传参安全。
fn open_in_browser(url: &str) -> bool {
    #[cfg(windows)]
    let r = std::process::Command::new("rundll32.exe")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn()
        .or_else(|_| std::process::Command::new("explorer.exe").arg(url).spawn());
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(url).spawn();
    r.is_ok()
}

pub(crate) async fn apply_oauth_code(
    ocfg: &provider::oauth::OAuthConfig,
    code: &str,
    meta: &mut ReplMeta,
    swap: &Arc<SwapProvider>,
    ui: &mut Ui,
) {
    let verifier = ui
        .panel
        .as_ref()
        .and_then(|p| p.oauth_verifier.clone())
        .unwrap_or_default();
    let expected_state = ui
        .panel
        .as_ref()
        .and_then(|p| p.oauth_state.clone())
        .unwrap_or_default();
    if verifier.is_empty() || expected_state.is_empty() {
        ui.note(
            "OAuth session expired; select the oauth row again",
            Color::Yellow,
        );
        return;
    }
    let input = code.trim();
    if input.is_empty() {
        ui.note("paste a non-empty code", Color::Yellow);
        return;
    }
    // 贴的是整个回调 URL(openai 流)→ 纯核提取 code;否则按 code / code#state 原样交换。
    let code_and_state = match provider::oauth::parse_authorization_input(input, &expected_state) {
        Ok(value) => value,
        Err(e) => {
            ui.note(format!("invalid OAuth callback: {e}"), Color::Yellow);
            return;
        }
    };
    let redirect_uri = ui
        .panel
        .as_ref()
        .and_then(|p| p.oauth_redirect_uri.as_deref())
        .unwrap_or(ocfg.redirect_uri)
        .to_string();
    ui.oauth_callback.take();
    ui.note(
        format!("exchanging {} OAuth code...", ocfg.provider),
        Color::Gray,
    );
    let http = provider::http::ReqwestClient::new();
    match provider::oauth::exchange_code_with_redirect(
        &http,
        ocfg,
        &code_and_state,
        &verifier,
        &redirect_uri,
        now_epoch(),
    )
    .await
    {
        Ok(token) => apply_oauth_token(ocfg, token, meta, swap, ui),
        Err(e) => ui.note(
            format!("{} OAuth exchange failed: {e}", ocfg.provider),
            Color::Red,
        ),
    }
}

pub(crate) fn apply_oauth_token(
    ocfg: &provider::oauth::OAuthConfig,
    token: provider::oauth::OAuthToken,
    meta: &mut ReplMeta,
    swap: &Arc<SwapProvider>,
    ui: &mut Ui,
) {
    match save_oauth_token(ocfg.provider, &token) {
        Ok(path) => {
            let (dm, db) = oauth_defaults(ocfg.provider);
            let cfg = Config::load(config_path());
            let configured_oauth_model = if ocfg.provider == "openai" {
                let is_chatgpt_selection = cfg
                    .base_url
                    .as_deref()
                    .is_some_and(|base| same_endpoint(base, db))
                    || cfg
                        .provider
                        .as_deref()
                        .is_some_and(|provider| provider.eq_ignore_ascii_case("chatgpt-plus"));
                is_chatgpt_selection.then_some(cfg.model).flatten()
            } else {
                cfg.model
            };
            let model = std::env::var("RIDGE_MODEL")
                .ok()
                .or(configured_oauth_model)
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| dm.to_string());
            let base_url = if ocfg.provider == "openai" {
                std::env::var("RIDGE_CHATGPT_BASE_URL").unwrap_or_else(|_| db.to_string())
            } else {
                std::env::var("RIDGE_BASE_URL")
                    .ok()
                    .or(cfg.base_url)
                    .unwrap_or_else(|| db.to_string())
            };
            swap.swap(oauth_swap_provider(
                ocfg.provider,
                base_url.clone(),
                model.clone(),
                token.access_token,
                token.account_id,
                current_effort(ui),
            ));
            meta.provider = ocfg.provider.to_string();
            meta.model = model;
            meta.base_url = base_url;
            if ocfg.provider == "openai" {
                ui.model_catalog = None;
                ui.model_catalog_reload = true;
            }
            ui.panel = None;
            register_oauth_profile(ocfg.provider);
            persist_default_selection(ui, &meta.provider, &meta.model, &meta.base_url);
            refresh_provider_label(meta);
            ui.note(
                format!(
                    "✓ {} OAuth connected · credential saved to {path}",
                    ocfg.provider
                ),
                Color::Green,
            );
        }
        Err(e) => ui.note(
            format!("OAuth token received but save failed: {e}"),
            Color::Red,
        ),
    }
}

/// 据订阅 provider 构造 bearer provider(TUI 热切用;与 login.rs `oauth_provider` 同逻辑)。
fn oauth_swap_provider(
    provider_id: &str,
    base: String,
    model: String,
    access: String,
    account_id: Option<String>,
    effort: &str,
) -> Arc<dyn provider::LlmProvider> {
    if provider_id == "anthropic" {
        Arc::new(AnthropicProvider::new_oauth(base, model, access))
    } else {
        Arc::new(
            provider::ChatGptProvider::new(base, model, access, account_id)
                .with_reasoning_effort(effort),
        )
    }
}

fn apply_effort(swap: &Arc<SwapProvider>, meta: &ReplMeta, value: &str, ui: &mut Ui) -> bool {
    let Some(effort) = provider::normalize_reasoning_effort(value) else {
        ui.note(
            format!(
                "invalid effort; choose one of: {}",
                provider::REASONING_EFFORTS.join(", ")
            ),
            Color::Yellow,
        );
        return false;
    };
    let effort = effort.to_string();
    ui.effort = Some(effort.clone());
    if let Err(error) = persist_config("effort", &effort) {
        ui.note(
            format!("effort applied, but save failed: {error}"),
            Color::Yellow,
        );
    }

    let oauth_base = std::env::var("RIDGE_CHATGPT_BASE_URL")
        .unwrap_or_else(|_| oauth_defaults("openai").1.to_string());
    if meta.provider == "openai"
        && meta.base_url.trim_end_matches('/') == oauth_base.trim_end_matches('/')
    {
        if let Some(token) = openai_oauth_token() {
            swap.swap(oauth_swap_provider(
                "openai",
                meta.base_url.clone(),
                meta.model.clone(),
                token.access_token,
                token.account_id,
                &effort,
            ));
        }
    }
    ui.note(format!("reasoning effort={effort}"), Color::Green);
    true
}

/// 热切换模型(iter-32 共用路径):密钥经 `current_api_key`(env 优先,回落 config 内联)——
/// `/model <name>` 文本命令与模型选择器浮窗同走此路,顺带修「内联 key 无法切模型」根因。
pub(crate) fn swap_model(swap: &Arc<SwapProvider>, meta: &mut ReplMeta, model: &str, ui: &mut Ui) {
    let oauth_base = std::env::var("RIDGE_CHATGPT_BASE_URL")
        .unwrap_or_else(|_| oauth_defaults("openai").1.to_string());
    if meta.provider == "openai"
        && meta.base_url.trim_end_matches('/') == oauth_base.trim_end_matches('/')
    {
        if let Some(token) = openai_oauth_token() {
            swap.swap(oauth_swap_provider(
                "openai",
                meta.base_url.clone(),
                model.to_string(),
                token.access_token,
                token.account_id,
                current_effort(ui),
            ));
            meta.model = model.to_string();
            persist_default_selection(ui, &meta.provider, model, &meta.base_url);
            ui.note(format!("switched model={model}"), Color::Green);
            return;
        }
    }
    let cfg = Config::load(config_path());
    let auth = load_auth();
    match api_key_for_runtime(&cfg, &auth, &meta.provider, &meta.base_url) {
        Some(key) => {
            swap.swap(make_provider(&meta.provider, model, &meta.base_url, key));
            meta.model = model.to_string();
            persist_default_selection(ui, &meta.provider, model, &meta.base_url);
            ui.note(format!("switched model={model}"), Color::Green);
        }
        None => ui.note(
            "no API key resolved (set RIDGE_API_KEY or api_key at config.json top level); cannot switch model",
            Color::Red,
        ),
    }
}

/// 热切换到命名 provider 档(iter-35 抽取):`/provider use` 与 Provider 页共用。
/// 密钥走档案 `key_env`(env);缺则红字提示,不切。
pub(crate) fn switch_provider(
    name: &str,
    meta: &mut ReplMeta,
    swap: &Arc<SwapProvider>,
    ui: &mut Ui,
) {
    match Config::load(config_path())
        .providers
        .into_iter()
        .find(|p| p.name == name)
    {
        // iter-48 G6:订阅档(use_oauth)→ 凭据走 oauth.json,bearer 热切。
        // ponytail:此处不刷新 token(同步路径);过期由 API 报错提示,启动路径有自动刷新。
        Some(p) if p.use_oauth == Some(true) => {
            let token = std::fs::read_to_string(oauth_path())
                .ok()
                .and_then(|t| agent::oauth_get(&t, &p.kind));
            match token {
                Some(token) => {
                    if token.needs_refresh(now_epoch()) {
                        ui.note(
                            "subscription token near expiry; restart ridgecode to auto-refresh if calls fail",
                            Color::Yellow,
                        );
                    }
                    let base_url = if p.kind == "openai" {
                        oauth_defaults("openai").1.to_string()
                    } else {
                        p.base_url.clone()
                    };
                    swap.swap(oauth_swap_provider(
                        &p.kind,
                        base_url.clone(),
                        p.model.clone(),
                        token.access_token,
                        token.account_id,
                        current_effort(ui),
                    ));
                    meta.provider = p.kind;
                    meta.model = p.model;
                    meta.base_url = base_url;
                    persist_default_selection(ui, &meta.provider, &meta.model, &meta.base_url);
                    refresh_provider_label(meta);
                    ui.note(format!("switched provider {name} (oauth)"), Color::Green);
                }
                None => ui.note(
                    format!(
                        "no oauth credential for {} ({}); run: ridgecode login --claude / --codex",
                        p.name, p.kind
                    ),
                    Color::Red,
                ),
            }
        }
        Some(p) => match p.resolve_key_with(&load_auth()) {
            Some(key) => {
                swap.swap(make_provider(&p.kind, &p.model, &p.base_url, key));
                meta.provider = p.kind;
                meta.model = p.model;
                meta.base_url = p.base_url;
                persist_default_selection(ui, &meta.provider, &meta.model, &meta.base_url);
                refresh_provider_label(meta);
                ui.note(format!("switched provider {name}"), Color::Green);
            }
            None => ui.note(
                format!(
                    "no key for {} ({}); run: ridgecode login",
                    p.name, p.key_env
                ),
                Color::Red,
            ),
        },
        None => ui.note(format!("no such provider: {name}"), Color::Red),
    }
}

/// 配置页就地编辑后 live 应用(iter-35):可即时生效的键立即作用于运行态,余键仅持久化(下次启动生效)。
pub(crate) fn apply_config_live(
    key: &str,
    val: &str,
    meta: &mut ReplMeta,
    swap: &Arc<SwapProvider>,
    ui: &mut Ui,
) {
    match key {
        "allow_jailbreak" => agent::set_allow_jailbreak(val == "true"),
        "model" => swap_model(swap, meta, val, ui),
        "provider" | "base_url" => {
            let cfg = Config::load(config_path());
            if key == "provider" {
                // Config stores the selected profile name (for example
                // `Zai`), while the runtime needs its wire kind (`openai`).
                // Route named selections through the same atomic switch path
                // as `/provider use` so endpoint, model, credential and label
                // cannot drift apart.
                if let Some(profile_name) = named_profile_name(&cfg, val) {
                    switch_provider(&profile_name, meta, swap, ui);
                    return;
                }
                meta.provider = val.to_string();
            } else {
                meta.base_url = val.to_string();
            }
            let auth = load_auth();
            if let Some(k) = api_key_for_runtime(&cfg, &auth, &meta.provider, &meta.base_url) {
                swap.swap(make_provider(
                    &meta.provider,
                    &meta.model,
                    &meta.base_url,
                    k,
                ));
            }
            refresh_provider_label(meta);
        }
        "status_bar" => {
            meta.status_bar = if val.trim().is_empty() {
                DEFAULT_STATUS_BAR.to_string()
            } else {
                val.to_string()
            }
        }
        "effort" => {
            if let Some(effort) = provider::normalize_reasoning_effort(val) {
                ui.effort = Some(effort.to_string());
                let oauth_base = std::env::var("RIDGE_CHATGPT_BASE_URL")
                    .unwrap_or_else(|_| oauth_defaults("openai").1.to_string());
                if meta.provider == "openai"
                    && meta.base_url.trim_end_matches('/') == oauth_base.trim_end_matches('/')
                {
                    if let Some(token) = openai_oauth_token() {
                        swap.swap(oauth_swap_provider(
                            "openai",
                            meta.base_url.clone(),
                            meta.model.clone(),
                            token.access_token,
                            token.account_id,
                            effort,
                        ));
                    }
                }
            }
        }
        // 代理即时注入 env:下一次登录 verify / 新建 provider 立即走它,无需重启。
        "proxy" => crate::apply_proxy_env(val),
        _ => {} // budget_tokens/skills_dir/skip_danger:仅持久化,下次启动生效。
    }
}

/// 交互页 Enter 动作分派(iter-35):先把选中项数据 clone 出(释放对 `ui.panel` 的不可变借用),
/// 再按 kind/编辑态改 `ui`/`meta`/热切换。Config=进/提交编辑;Models=切模型;Provider=切档;Tools/Agent=只读关页。
pub(crate) fn panel_enter(ui: &mut Ui, meta: &mut ReplMeta, swap: &Arc<SwapProvider>) {
    let Some(panel) = ui.panel.as_ref() else {
        return;
    };
    let kind = panel.kind;
    let editing = panel.editing.clone();
    let sel_key = panel.selected().map(|r| r.key.clone());
    let sel_val = panel.selected().map(|r| r.value.clone());
    let sel_ctx = panel.selected().and_then(|r| r.ctx);
    match (kind, editing) {
        // 配置页:提交编辑 → 持久化 + live 应用 + 刷新页(退编辑态)。
        (PanelKind::Config, Some(newval)) => {
            let Some(key) = sel_key else { return };
            let val = newval.trim().to_string();
            match persist_config(&key, &val) {
                Ok(_) => {
                    apply_config_live(&key, &val, meta, swap, ui);
                    ui.note(format!("saved {key}={val}"), Color::Green);
                    ui.panel = Some(config_panel());
                }
                Err(e) => {
                    ui.note(format!("write failed: {e}"), Color::Red);
                    if let Some(p) = ui.panel.as_mut() {
                        p.editing = None;
                    }
                }
            }
        }
        // 配置页:进入编辑 → 预填当前值(「(未设)」视为空)。
        (PanelKind::Config, None) => {
            let cur = sel_val.map(|v| if v == "(unset)" { String::new() } else { v });
            if let (Some(p), Some(cur)) = (ui.panel.as_mut(), cur) {
                p.editing = Some(cur);
            }
        }
        // 模型页:切到选中模型(跨 provider 时先切 provider 再切模型)。
        (PanelKind::Models, _) => {
            if let Some(key) = sel_key {
                // key 格式: "provider · model_id"
                let (provider_name, model) = key.split_once(" · ").unwrap_or(("", &key));
                if provider_name == EFFORT_MODEL_GROUP {
                    apply_effort(swap, meta, model, ui);
                    ui.panel = None;
                    return;
                } else if provider_name == CHATGPT_MODEL_GROUP {
                    if let Some(token) = openai_oauth_token() {
                        let base_url = std::env::var("RIDGE_CHATGPT_BASE_URL")
                            .unwrap_or_else(|_| oauth_defaults("openai").1.to_string());
                        swap.swap(oauth_swap_provider(
                            "openai",
                            base_url.clone(),
                            model.to_string(),
                            token.access_token,
                            token.account_id,
                            current_effort(ui),
                        ));
                        meta.provider = "openai".to_string();
                        meta.model = model.to_string();
                        meta.base_url = base_url;
                        persist_default_selection(ui, &meta.provider, model, &meta.base_url);
                        refresh_provider_label(meta);
                        if let Some(w) = sel_ctx {
                            meta.ctx_window = w;
                        }
                        ui.note(
                            format!("switched to {CHATGPT_MODEL_GROUP} / {model}"),
                            Color::Green,
                        );
                        ui.panel = None;
                        return;
                    }
                    ui.note(
                        "no ChatGPT OAuth credential; run /login --codex first",
                        Color::Red,
                    );
                } else if !provider_name.is_empty() && provider_name != meta.provider {
                    // 跨 provider:查档案,全量切换
                    let cfg = Config::load(config_path());
                    let auth = load_auth();
                    if let Some(p) = cfg.providers.into_iter().find(|p| p.name == provider_name) {
                        if p.use_oauth == Some(true) && p.kind == "openai" {
                            if let Some(token) = openai_oauth_token() {
                                let base_url = std::env::var("RIDGE_CHATGPT_BASE_URL")
                                    .unwrap_or_else(|_| oauth_defaults("openai").1.to_string());
                                swap.swap(oauth_swap_provider(
                                    "openai",
                                    base_url.clone(),
                                    model.to_string(),
                                    token.access_token,
                                    token.account_id,
                                    current_effort(ui),
                                ));
                                meta.provider = p.kind;
                                meta.model = model.to_string();
                                meta.base_url = base_url;
                                persist_default_selection(
                                    ui,
                                    &meta.provider,
                                    model,
                                    &meta.base_url,
                                );
                                refresh_provider_label(meta);
                                if let Some(w) = sel_ctx {
                                    meta.ctx_window = w;
                                }
                                ui.note(
                                    format!("switched to {} / {model}", provider_name),
                                    Color::Green,
                                );
                                ui.panel = None;
                                return;
                            }
                        } else if let Some(k) = p.resolve_key_with(&auth) {
                            swap.swap(make_provider(&p.kind, model, &p.base_url, k));
                            meta.provider = p.kind;
                            meta.model = model.to_string();
                            meta.base_url = p.base_url;
                            persist_default_selection(ui, &meta.provider, model, &meta.base_url);
                            refresh_provider_label(meta);
                            if let Some(w) = sel_ctx {
                                meta.ctx_window = w;
                            }
                            ui.note(
                                format!("switched to {} / {model}", provider_name),
                                Color::Green,
                            );
                            ui.panel = None;
                            return;
                        }
                    }
                    ui.note(format!("no key for provider {provider_name}"), Color::Red);
                } else {
                    // 同 provider:只切模型
                    if let Some(w) = sel_ctx {
                        meta.ctx_window = w;
                    }
                    swap_model(swap, meta, if model.is_empty() { &key } else { model }, ui);
                }
                ui.panel = None;
            }
        }
        // Provider 页:切到选中档。
        (PanelKind::Provider, _) => {
            if let Some(name) = sel_key {
                switch_provider(&name, meta, swap, ui);
                ui.panel = None;
            }
        }
        // 登录页:Enter 选中 preset → 起 key 输入态(标题提示选了哪家)。Some 分支(提交 key)由主环
        // 异步处理(校验联网),不达此。
        (PanelKind::Login, None) => {
            if let (Some(p), Some(id)) = (ui.panel.as_mut(), sel_key) {
                if id == CLAUDE_OAUTH_ROW {
                    begin_oauth(ui, &provider::oauth::ANTHROPIC);
                } else if id == CODEX_OAUTH_ROW {
                    begin_oauth(ui, &provider::oauth::OPENAI);
                } else {
                    p.editing = Some(String::new());
                    p.title = format!(
                        "Login · enter API key for {id} · Enter verify & connect · Esc cancel"
                    );
                }
            }
        }
        (PanelKind::Login, Some(_)) => {}
        (
            PanelKind::Activity
            | PanelKind::ToolHistory
            | PanelKind::ReasoningHistory
            | PanelKind::AnswerHistory,
            _,
        ) => {
            if let Some(p) = ui.panel.as_mut() {
                p.toggle_detail();
            }
        }
        (PanelKind::LiveHistory, _) => {
            ui.sync_live_panel_focus();
            ui.toggle_live_panel_detail();
        }
        // 只读页:Enter 关页。
        (PanelKind::Tools, _)
        | (PanelKind::Agent, _)
        | (PanelKind::Mcp, _)
        | (PanelKind::Skills, _)
        | (PanelKind::Queue, _) => ui.panel = None,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_command(
    input: &str,
    ui: &mut Ui,
    history: &mut Vec<Message>,
    meta: &mut ReplMeta,
    swap: &Arc<SwapProvider>,
    agents: &agent::Agents,
    commands: &[agent::SlashCommand],
    skills: &[agent::Skill],
    tokens: usize,
    turns: usize,
) -> anyhow::Result<bool> {
    if input == "/help" {
        ui.note(
            "/exit /model /provider /config /effort /find [query] /goal [status|create|start|advance|resume|complete|block|cancel] /activity /inspect /transcript /audit /reasoning /answers /queue /tools /history /login /agent /mcp /skills /commands; Enter queues while busy; Ctrl+Enter front-queues without interrupting; Ctrl+F opens non-blocking live search; Ctrl+Q opens the queue and Delete removes a pending item; Ctrl+I/Alt+I inspects live blocks in Transcript Audit; Ctrl+A opens the Answer archive; Ctrl+R toggles live reasoning or opens Reasoning history; Ctrl+O toggles live tool details or opens Tool history; Ctrl+T opens recent Agent activity; Ctrl-C hands input back.",
            Color::Gray,
        );
        return Ok(false);
    }
    match input {
        "/exit" | "/quit" => return Ok(true),
        "/tools" => ui.panel = Some(tools_panel(&meta.tools)),
        "/activity" => ui.open_activity_panel(),
        "/inspect" | "/live" | "/transcript" | "/audit" => {
            if !ui.open_live_history() {
                ui.note("no live blocks to inspect", Color::Gray);
            }
        }
        "/find" => {
            if !ui.open_live_search("") {
                ui.note("no live blocks to search", Color::Gray);
            }
        }
        _ if input.starts_with("/find ") => {
            if !ui.open_live_search(input[6..].trim()) {
                ui.note("no live blocks to search", Color::Gray);
            }
        }
        "/reasoning" | "/thinking" => {
            if !ui.open_reasoning_history() {
                ui.note("no completed reasoning history", Color::Gray);
            }
        }
        "/answer" | "/answers" => {
            if !ui.open_answer_history() {
                ui.note("no recoverable answer history", Color::Gray);
            }
        }
        "/queue" => ui.open_queue_panel(),
        "/history" => {
            if !ui.open_tool_history() {
                ui.note("no completed tool history", Color::Gray);
            }
        }
        "/reset" => {
            history.clear();
            ui.reasoning_history.clear();
            ui.answer_history.clear();
            ui.panel = None;
            save_session(&session_path(), history);
            ui.note("context cleared", Color::Yellow);
        }
        "/compact" => {
            let n = history.len();
            *history = compact_history(std::mem::take(history), 4);
            ui.note(
                format!("context compacted: {n} → {} messages", history.len()),
                Color::Yellow,
            );
        }
        "/cost" => ui.note(
            format!("session total: {tokens} tokens · {turns} tasks"),
            Color::Gray,
        ),
        "/effort" => ui.note(
            format!(
                "reasoning effort={} · options: {}",
                current_effort(ui),
                provider::REASONING_EFFORTS.join(", ")
            ),
            Color::Gray,
        ),
        _ if input.starts_with("/effort ") => {
            apply_effort(swap, meta, input[8..].trim(), ui);
        }
        // /model 单命令:无参 → 跨 provider 模型页(↑↓ 选、Enter 切);`/model <name>` → 直接热切。
        // `/models` `/model pick` 保留为别名(旧肌肉记忆),补全表只呈现 `/model`。
        _ if input == "/model" || input == "/models" || input == "/model pick" => {
            let Some(grouped) = ui.model_catalog.as_ref() else {
                ui.note(
                    "model catalog is still loading; try /model again shortly",
                    Color::Yellow,
                );
                return Ok(false);
            };
            if grouped.is_empty() {
                ui.note(
                    "no models returned (providers unreachable or authentication failed)",
                    Color::Yellow,
                );
            } else {
                let current_group = model_group_name(&meta.provider, &meta.base_url);
                meta.ctx_window = grouped
                    .iter()
                    .find(|(name, _models)| name == &current_group)
                    .and_then(|(_, models)| {
                        models
                            .iter()
                            .find(|model| model.id == meta.model)
                            .and_then(|model| model.context)
                    })
                    .unwrap_or(tui::DEFAULT_CTX_WINDOW);
                ui.panel = Some(models_panel_with_effort(
                    grouped,
                    &current_group,
                    &meta.model,
                    current_effort(ui),
                ));
            }
        }
        _ if input.starts_with("/model ") => swap_model(swap, meta, input[7..].trim(), ui),
        _ if input == "/jailbreak" => {
            let on = agent::allow_jailbreak();
            ui.note(if on { "jailbreak: ON ⚠ (can write outside cwd subtree; disaster commands / protected paths / read-only still blocked). Disable: /jailbreak off" } else { "jailbreak: OFF (writes limited to cwd subtree). Enable: /jailbreak on —— top status bar turns red when on" }, if on { Color::Red } else { Color::Gray });
        }
        _ if input == "/jailbreak on" => {
            agent::set_allow_jailbreak(true);
            ui.note("⚠ jailbreak ON: can write outside cwd subtree (disaster commands / protected paths / read-only still hard-blocked). Session only; to persist: /config set allow_jailbreak true", Color::Red);
        }
        _ if input == "/jailbreak off" => {
            agent::set_allow_jailbreak(false);
            ui.note(
                "jailbreak OFF: writes limited back to cwd subtree",
                Color::Green,
            );
        }
        _ if input == "/config" => ui.panel = Some(config_panel()),
        _ if input.starts_with("/config set ") => {
            let parts: Vec<_> = input.splitn(4, ' ').collect();
            if parts.len() == 4 {
                match persist_config(parts[2], parts[3]) {
                    Ok(path) => ui.note(
                        format!("wrote {path}; takes effect next start"),
                        Color::Green,
                    ),
                    Err(e) => ui.note(format!("write failed: {e}"), Color::Red),
                }
            } else {
                ui.note("usage: /config set <key> <value>", Color::Yellow);
            }
        }
        _ if input == "/provider" || input == "/provider list" => {
            let cfg = Config::load(config_path());
            if cfg.providers.is_empty() {
                ui.note("no provider profiles. Add: /provider add <name> <kind> <model> <base_url> [key_env]", Color::Gray);
            } else {
                ui.panel = Some(provider_panel());
            }
        }
        _ if input.starts_with("/provider add ") => {
            match agent::parse_provider_add(input["/provider add ".len()..].trim()) {
                Ok(profile) => {
                    let path = config_path();
                    let text = std::fs::read_to_string(&path).unwrap_or_default();
                    match agent::config_add_provider(&text, &profile) {
                        Ok(out) => match std::fs::write(&path, out) {
                            Ok(_) => ui.note(format!("added provider \"{}\" → {} (switch: /provider use {}; set the API key in env var {})", profile.name, path, profile.name, profile.key_env), Color::Green),
                            Err(e) => ui.note(format!("failed to write config: {e}"), Color::Red),
                        },
                        Err(e) => ui.note(format!("config transform failed: {e}"), Color::Red),
                    }
                }
                Err(e) => ui.note(e, Color::Yellow),
            }
        }
        _ if input.starts_with("/provider use ") => {
            switch_provider(input[14..].trim(), meta, swap, ui)
        }
        _ if input == "/login" => ui.panel = Some(login_panel()),
        _ if input == "/login list" => {
            let ids: Vec<&str> = PROVIDER_PRESETS.iter().map(|p| p.id).collect();
            ui.note(format!("built-in providers: {}\nOAuth: claude-oauth (/login --claude) · codex-oauth (/login --codex)\n端口受限时执行: ridgecode login --codex --device-auth\ninteractive: /login  ·  quick: /login <id> <API_KEY> (verified; key → ~/.ridge/auth.json, not config)", ids.join(", ")), Color::Gray);
        }
        _ if input == "/login --claude" || input == "/login claude-oauth" => {
            ui.panel = Some(login_panel());
            if let Some(panel) = ui.panel.as_mut() {
                if let Some(pos) = panel
                    .view
                    .iter()
                    .position(|&i| panel.rows[i].key == CLAUDE_OAUTH_ROW)
                {
                    panel.sel = pos;
                }
            }
            begin_oauth(ui, &provider::oauth::ANTHROPIC);
        }
        _ if input == "/login --codex" || input == "/login codex-oauth" => {
            ui.panel = Some(login_panel());
            if let Some(panel) = ui.panel.as_mut() {
                if let Some(pos) = panel
                    .view
                    .iter()
                    .position(|&i| panel.rows[i].key == CODEX_OAUTH_ROW)
                {
                    panel.sel = pos;
                }
            }
            begin_oauth(ui, &provider::oauth::OPENAI);
        }
        _ if input.starts_with("/login ") => {
            let rest = input["/login ".len()..].trim();
            let mut it = rest.split_whitespace();
            match (it.next(), it.next()) {
                (Some(id), Some(key)) => match preset_by_id(id) {
                    Some(preset) => login_apply_verified(preset, key, meta, swap, ui).await,
                    None => ui.note(
                        format!("unknown provider \"{id}\"; see /login list"),
                        Color::Yellow,
                    ),
                },
                _ => ui.note(
                    "usage: /login <id> <API_KEY>, or just /login to pick interactively",
                    Color::Yellow,
                ),
            }
        }
        _ if input == "/goal" || input.starts_with("/goal ") => {
            let args = input
                .strip_prefix("/goal")
                .unwrap_or_default()
                .split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>();
            match agent::goal_command(&args) {
                Ok(text) => ui.note(text, Color::Cyan),
                Err(error) => ui.note(format!("goal error: {error}"), Color::Red),
            }
        }
        _ if input == "/agent" => {
            if agents.defs.is_empty() {
                ui.note("no sub-agents available", Color::Gray);
            } else {
                ui.panel = Some(agent_panel(&agents.defs));
            }
        }
        _ if input == "/mcp" => {
            let cfg = Config::load(config_path());
            if cfg.mcp.is_empty() {
                ui.note("no MCP servers configured. Add them under \"mcp\": [ ... ] in ~/.ridge/config.json (each: name + cmd [+ args]).", Color::Gray);
            } else {
                ui.panel = Some(mcp_panel());
            }
        }
        _ if input == "/skills" => {
            if skills.is_empty() {
                ui.note("no skills loaded. Add ~/.ridge/skills/<name>/SKILL.md (frontmatter name/description + body); loaded skills are injected into the system prompt.", Color::Gray);
            } else {
                ui.panel = Some(skills_panel(skills));
            }
        }
        _ if input == "/commands" => {
            if commands.is_empty() {
                ui.note("no custom commands. Add ~/.ridge/commands/<name>.md (body = prompt, $ARGS = args); skills also appear here.", Color::Gray);
            } else {
                let lines: Vec<String> = commands
                    .iter()
                    .map(|c| {
                        if c.description.is_empty() {
                            format!("/{}", c.name)
                        } else {
                            format!("/{}  —— {}", c.name, c.description)
                        }
                    })
                    .collect();
                ui.note(
                    format!("commands ({}):\n{}", commands.len(), lines.join("\n")),
                    Color::Gray,
                );
            }
        }
        // 自定义 / skill 命令(iter-39):/name [args] → 展开 body(替 $ARGS)为任务(置 run_task,主环起任务)。
        _ if input.starts_with('/') => {
            let rest = &input[1..];
            let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            match agent::resolve_command(name, commands) {
                Some(cmd) => ui.run_task = Some(agent::expand_command(&cmd.body, args.trim())),
                None => ui.note(
                    format!("unknown command: {input} (/help · /commands)"),
                    Color::Yellow,
                ),
            }
        }
        _ => return Ok(false),
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> ReplMeta {
        ReplMeta {
            tools: vec!["read_file".into(), "run_shell".into()],
            provider: "openai".into(),
            provider_label: "openai".into(),
            model: "gpt-4o".into(),
            base_url: "https://api.openai.com/v1".into(),
            status_bar: "{provider} {model}".into(),
            ctx_window: tui::DEFAULT_CTX_WINDOW,
        }
    }

    #[tokio::test]
    async fn command_router_covers_read_only_panels_and_prompt_commands() {
        let mut ui = Ui::default();
        let mut history = vec![Message::user("hello")];
        let mut meta = meta();
        let swap = Arc::new(SwapProvider::new(Arc::new(ScriptedProvider::new(vec![]))));
        let agents = agent::Agents::default();
        let skills = Vec::new();
        let commands = vec![agent::SlashCommand {
            name: "ship".into(),
            description: "ship it".into(),
            body: "deploy $ARGS".into(),
        }];

        for input in [
            "/help",
            "/tools",
            "/activity",
            "/inspect",
            "/find",
            "/find needle",
            "/reasoning",
            "/answers",
            "/queue",
            "/history",
            "/compact",
            "/cost",
            "/effort",
            "/provider",
            "/provider use missing",
            "/login",
            "/login list",
            "/login openai",
            "/mcp",
            "/skills",
            "/commands",
            "/jailbreak",
            "/model",
            "/model missing",
            "/unknown",
        ] {
            assert!(!run_command(
                input,
                &mut ui,
                &mut history,
                &mut meta,
                &swap,
                &agents,
                &commands,
                &skills,
                42,
                3,
            )
            .await
            .unwrap());
        }

        ui.model_catalog = Some(vec![(
            "openai".into(),
            vec![provider::models::ModelInfo {
                id: "gpt-4o".into(),
                context: Some(128_000),
            }],
        )]);
        run_command(
            "/model",
            &mut ui,
            &mut history,
            &mut meta,
            &swap,
            &agents,
            &commands,
            &skills,
            42,
            3,
        )
        .await
        .unwrap();

        run_command(
            "/ship src/lib.rs",
            &mut ui,
            &mut history,
            &mut meta,
            &swap,
            &agents,
            &commands,
            &skills,
            42,
            3,
        )
        .await
        .unwrap();
        assert_eq!(ui.run_task.as_deref(), Some("deploy src/lib.rs"));
        assert!(run_command(
            "/exit",
            &mut ui,
            &mut history,
            &mut meta,
            &swap,
            &agents,
            &commands,
            &skills,
            42,
            3,
        )
        .await
        .unwrap());
    }

    #[test]
    fn model_target_helpers_preserve_profile_identity_and_endpoint_rules() {
        let cfg = Config::parse(
            r#"{
                "provider": "Zai",
                "providers": [{
                    "name": "Zai",
                    "kind": "openai",
                    "model": "glm-4.6",
                    "base_url": "https://open.bigmodel.cn/api/paas/v4",
                    "api_key": "sk-zai"
                }]
            }"#,
        );
        let auth = std::collections::BTreeMap::new();
        assert_eq!(named_profile_name(&cfg, " zai ").as_deref(), Some("Zai"));
        assert_eq!(
            model_group_name("openai", "https://api.openai.com/v1"),
            "openai"
        );
        assert!(same_endpoint(
            "https://example.test/",
            "HTTPS://EXAMPLE.TEST"
        ));
        assert_eq!(
            profile_for_runtime(&cfg, "openai", "https://open.bigmodel.cn/api/paas/v4/")
                .unwrap()
                .name,
            "Zai"
        );
        assert_eq!(
            api_key_for_runtime(
                &cfg,
                &auth,
                "openai",
                "https://open.bigmodel.cn/api/paas/v4"
            ),
            Some("sk-zai".into())
        );
        let targets = build_model_targets(
            &cfg,
            &auth,
            "openai",
            "https://open.bigmodel.cn/api/paas/v4",
        );
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].name, "openai");
        assert_eq!(targets[1].name, "Zai");
        assert!(!targets[0].oauth);
    }
}
