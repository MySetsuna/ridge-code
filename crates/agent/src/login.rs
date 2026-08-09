//! 供应商登录/认证/OAuth:key 校验、login 子命令、Claude 订阅 OAuth(iter-43)。
use crate::*;

/// 校验核:经 `fetch_models` 打 `{base_url}/models` 鉴权 GET → Ok(模型数) / Err(原因)。
/// 走注入的 HttpClient(接缝),测试可零网络。`get_json` 非 2xx 返 Err → 错 key/坏端点如实失败。
pub(crate) async fn verify_key_via(
    http: &dyn provider::http::HttpClient,
    kind: &str,
    base_url: &str,
    key: &str,
) -> Result<usize, String> {
    match provider::models::fetch_models(http, kind, base_url, key).await {
        Ok(list) => Ok(list.len()),
        Err(e) => Err(format!("{e}")),
    }
}

/// 校验一把 key 对某 provider 是否真连通(真 `ReqwestClient` + 15s 超时)。供 CLI/TUI 登录共用。
pub(crate) async fn verify_provider_key(
    kind: &str,
    base_url: &str,
    key: &str,
) -> Result<usize, String> {
    let http = provider::http::ReqwestClient::new();
    match tokio::time::timeout(
        std::time::Duration::from_secs(15),
        verify_key_via(&http, kind, base_url, key),
    )
    .await
    {
        Ok(r) => r,
        Err(_) => Err("timed out (15s)".into()),
    }
}

/// 打印内置供应商 preset 表(`login` 无参 / `--list`)。
pub(crate) fn print_presets() {
    println!("Built-in providers (ridgecode login <id> [KEY]):\n");
    for p in PROVIDER_PRESETS {
        println!(
            "  {:<12} {:<34} {} · {}",
            p.id, p.label, p.default_model, p.base_url
        );
    }
    println!(
        "\nExample:  ridgecode login deepseek sk-...            (verifies connection, registers + sets as default)\n\
         \x20         ridgecode login kimi --no-default          (add as a switchable profile)\n\
         \x20         ridgecode login openai sk-... --no-verify  (skip the connection check)\n\
         OAuth fallback: ridgecode login --codex --device-auth (no localhost callback port required).\n\
         Login verifies the key against the endpoint before saving. Key goes to ~/.ridge/auth.json\n\
         (never into config.json). Omit KEY to be prompted on stdin."
    );
}

/// `ridgecode login` 子命令:内置供应商一键接入。**key 只进 auth.json,绝不进 config、绝不回显。**
///   ridgecode login | login --list                     列出内置供应商
///   ridgecode login <id> [KEY] [--model M] [--name N] [--no-default]
/// 缺 KEY 则从 stdin 读一行(避免落进 shell 历史 / 进程参数)。默认设为启动默认档;`--no-default` 只登记。
pub(crate) async fn run_login(args: &[String]) -> anyhow::Result<()> {
    let mut positional: Vec<&str> = Vec::new();
    let mut model: Option<String> = None;
    let mut name: Option<String> = None;
    let mut make_default = true;
    let mut list = false;
    let mut no_verify = false;
    let mut oauth_claude = false;
    let mut oauth_codex = false;
    let mut device_auth = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--list" | "-l" => list = true,
            "--no-default" => make_default = false,
            "--default" => make_default = true,
            "--no-verify" => no_verify = true,
            "--device-auth" => device_auth = true,
            "--claude" => oauth_claude = true, // iter-43:OAuth 订阅登录(接 Claude Pro/Max)
            "--codex" => oauth_codex = true,   // iter-48:OAuth 订阅登录(接 ChatGPT Plus/Pro)
            "--model" => model = it.next().cloned(),
            "--name" => name = it.next().cloned(),
            _ => positional.push(a),
        }
    }
    // OAuth(PKCE)订阅登录,与 api-key 登录分道(iter-43 claude / iter-48 codex)。
    if oauth_claude {
        if device_auth {
            anyhow::bail!("--device-auth is only supported with `--codex`");
        }
        return run_login_oauth(&provider::oauth::ANTHROPIC, no_verify).await;
    }
    if oauth_codex {
        if device_auth {
            return run_login_device_auth(no_verify).await;
        }
        return run_login_oauth(&provider::oauth::OPENAI, no_verify).await;
    }
    if list || positional.is_empty() {
        print_presets();
        return Ok(());
    }
    let id = positional[0];
    let Some(preset) = preset_by_id(id) else {
        anyhow::bail!("unknown provider \"{id}\". Run `ridgecode login --list` to see built-ins.");
    };

    // key:参数给了用参数;否则从 stdin 读一行(不回显保证不了,但不落 argv/历史)。
    let key = match positional.get(1) {
        Some(k) => k.to_string(),
        None => {
            eprint!("Paste API key for {} ({}): ", preset.label, preset.key_env);
            std::io::stderr().flush().ok();
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            line.trim().to_string()
        }
    };
    if key.is_empty() {
        anyhow::bail!("no API key provided; aborted (nothing written).");
    }
    // 0) 连接校验(默认;--no-verify 跳过):打端点验 key 真连通,失败则不落盘。
    if !no_verify {
        eprint!("  verifying {} …", preset.id);
        std::io::stderr().flush().ok();
        match verify_provider_key(preset.kind, preset.base_url, &key).await {
            Ok(n) => eprintln!(" ✓ connected ({n} models)"),
            Err(e) => {
                eprintln!(" ✗");
                anyhow::bail!("could not connect to {} ({e}); nothing written. Retry, or `--no-verify` to skip the check.", preset.label);
            }
        }
    }
    // 1) key → auth.json(独立密钥库,收紧权限)。
    let auth_file = {
        let path = auth_path();
        if let Some(dir) = std::path::Path::new(&path).parent() {
            std::fs::create_dir_all(dir).map_err(|e| anyhow::anyhow!(e))?;
        }
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        std::fs::write(&path, auth_upsert(&text, preset.key_env, &key))
            .map_err(|e| anyhow::anyhow!(e))?;
        secure_file(&path);
        path
    };
    // 2) 档案 → config.json(经纯核;产物不含 key)。
    let cfg_path = config_path();
    let cfg_text = std::fs::read_to_string(&cfg_path).unwrap_or_default();
    let updated = apply_login(
        &cfg_text,
        preset,
        name.as_deref(),
        model.as_deref(),
        make_default,
    )
    .map_err(|e| anyhow::anyhow!(e))?;
    if let Some(dir) = std::path::Path::new(&cfg_path).parent() {
        std::fs::create_dir_all(dir).map_err(|e| anyhow::anyhow!(e))?;
    }
    std::fs::write(&cfg_path, updated).map_err(|e| anyhow::anyhow!(e))?;

    let prof_name = name.as_deref().unwrap_or(preset.id);
    let model_used = model.as_deref().unwrap_or(preset.default_model);
    println!("[OK] logged in to {} ({})", preset.label, preset.id);
    println!(
        "     key saved  -> {auth_file}  (slot {}, chmod 600 where supported)",
        preset.key_env
    );
    println!("     profile    -> {cfg_path}  (name \"{prof_name}\", model {model_used})");
    if make_default {
        println!("     set as the startup default provider. Just run: ridgecode");
    } else {
        println!("     registered as a profile. Switch in the TUI with: /provider use {prof_name}");
    }
    Ok(())
}

/// `~/.ridge/oauth.json` OAuth 凭据库路径(`RIDGE_OAUTH` 可覆盖;独立于 config/auth)。
pub(crate) fn oauth_path() -> String {
    std::env::var("RIDGE_OAUTH").unwrap_or_else(|_| format!("{}/oauth.json", ridge_home()))
}

/// 当前 epoch 秒。刷新判定等**纯逻辑**把 now 当参数传(可测),此仅在运行时取真值。
pub(crate) fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 把某 provider 的 OAuth token 写入 oauth.json(保留其余,收紧权限 0600)。
pub(crate) fn save_oauth_token(
    provider_id: &str,
    token: &provider::oauth::OAuthToken,
) -> anyhow::Result<String> {
    let path = oauth_path();
    if let Some(dir) = std::path::Path::new(&path).parent() {
        std::fs::create_dir_all(dir).map_err(|e| anyhow::anyhow!(e))?;
    }
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    std::fs::write(&path, agent::oauth_upsert(&text, provider_id, token))
        .map_err(|e| anyhow::anyhow!(e))?;
    secure_file(&path);
    Ok(path)
}

/// 订阅 OAuth 各家默认 (model, base_url)(env RIDGE_MODEL/RIDGE_BASE_URL 或 config 可覆盖)。
pub(crate) fn oauth_defaults(provider_id: &str) -> (&'static str, &'static str) {
    match provider_id {
        "openai" => ("gpt-5", "https://chatgpt.com/backend-api/codex"),
        _ => ("claude-sonnet-4-6", "https://api.anthropic.com/v1"),
    }
}

fn oauth_model_and_base(cfg: &Config, provider_id: &str) -> (String, String) {
    let (default_model, default_base) = oauth_defaults(provider_id);
    let model_from_config = if provider_id == "openai" {
        let active_chatgpt_profile = cfg
            .provider
            .as_deref()
            .is_some_and(|provider| provider.eq_ignore_ascii_case("chatgpt-plus"));
        let active_chatgpt_base = cfg
            .base_url
            .as_deref()
            .is_some_and(|base| base.trim_end_matches('/') == default_base);
        (active_chatgpt_profile || active_chatgpt_base)
            .then_some(cfg.model.clone())
            .flatten()
    } else {
        cfg.model.clone()
    };
    let model = std::env::var("RIDGE_MODEL")
        .ok()
        .or(model_from_config)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default_model.to_string());
    let base = if provider_id == "openai" {
        std::env::var("RIDGE_CHATGPT_BASE_URL").unwrap_or_else(|_| default_base.to_string())
    } else {
        std::env::var("RIDGE_BASE_URL")
            .ok()
            .or_else(|| cfg.base_url.clone())
            .unwrap_or_else(|| default_base.to_string())
    };
    (model, base)
}

/// Return the active-looking model identity for a stored OAuth credential.
/// This keeps the TUI metadata aligned with the provider selected at startup,
/// even when config.json still contains an unrelated API provider.
pub(crate) fn oauth_model_info(cfg: &Config) -> Option<(String, String, String)> {
    let text = std::fs::read_to_string(oauth_path()).ok()?;
    for ocfg in [&provider::oauth::ANTHROPIC, &provider::oauth::OPENAI] {
        if agent::oauth_get(&text, ocfg.provider).is_some() {
            let (model, base) = oauth_model_and_base(cfg, ocfg.provider);
            return Some((ocfg.provider.to_string(), model, base));
        }
    }
    None
}

/// 据订阅凭据构造 bearer-mode provider(anthropic 走专用 OAuth wire,其余走 OpenAI bearer)。
fn oauth_provider(
    provider_id: &str,
    base: String,
    model: String,
    access: String,
    account_id: Option<String>,
    effort: &str,
) -> Arc<dyn LlmProvider> {
    if provider_id == "anthropic" {
        Arc::new(AnthropicProvider::new_oauth(base, model, access))
    } else {
        Arc::new(
            provider::ChatGptProvider::new(base, model, access, account_id)
                .with_reasoning_effort(effort),
        )
    }
}

/// OAuth(PKCE)订阅登录(iter-43 claude 贴码流 / iter-48 codex 本地回调流,共用纯核)。
/// **本程序不碰账号密码**:生成授权 URL,用户本人在浏览器授权;
/// 换到的 access+refresh token 落 oauth.json(0600),补全侧走 `Authorization: Bearer`。
pub(crate) async fn run_login_oauth(
    ocfg: &provider::oauth::OAuthConfig,
    no_verify: bool,
) -> anyhow::Result<()> {
    use provider::oauth;
    let pkce = oauth::Pkce::generate();
    let state = oauth::random_token();
    let callback = if ocfg.token_wire == oauth::TokenWire::Form {
        match start_local_callback(state.clone()) {
            Ok(callback) => Some(callback),
            Err(e) => {
                if ocfg.provider == "openai" {
                    eprintln!("  local callback unavailable ({e}); switching to device auth");
                    return run_login_device_auth(no_verify).await;
                }
                eprintln!("  local callback unavailable ({e}); switching to copy/paste fallback");
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
    let url = oauth::authorize_url_with_redirect(ocfg, &pkce.challenge, &state, &redirect_uri);
    let flag = if ocfg.provider == "openai" {
        "--codex"
    } else {
        "--claude"
    };
    println!("\n== ridgecode login {flag} (OAuth 订阅登录) ==\n");
    println!(
        "注:授权站点可能有地域限制。所在区域若无法直连,浏览器需走代理打开下方 URL;\n   \
         并给本进程设 HTTP_PROXY/HTTPS_PROXY 环境变量 —— token 交换/刷新会自动走它。\n"
    );
    if open_in_browser(&url) {
        println!("1) 已在默认浏览器打开授权页（下方 URL 仍可手动复制）：\n");
    } else {
        println!("1) 请在浏览器打开下方 URL：\n");
    }
    println!("{url}\n");
    // 取 code:openai 由已启动的 listener 自动接收;anthropic 回调页显码,用户回贴。
    let code = if let Some(callback) = callback {
        println!(
            "2) 等待浏览器授权回调(监听 localhost:{};Ctrl-C 取消)…",
            callback.port
        );
        let value = callback.wait().await.map_err(|e| anyhow::anyhow!(e))?;
        value
    } else if ocfg.token_wire == oauth::TokenWire::Form {
        read_pasted_authorization(&state)?
    } else {
        println!("2) 授权后页面会显示形如 code#state 的码。粘贴到此处并回车:");
        eprint!("code: ");
        std::io::stderr().flush().ok();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        provider::oauth::parse_authorization_input(&line, &state)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?
    };
    if code.is_empty() {
        anyhow::bail!("no authorization code provided; aborted (nothing written).");
    }
    let http = provider::http::ReqwestClient::new();
    let token = oauth::exchange_code_with_redirect(
        &http,
        ocfg,
        &code,
        &pkce.verifier,
        &redirect_uri,
        now_epoch(),
    )
    .await
    .map_err(|e| anyhow::anyhow!("token exchange failed: {e}"))?;
    finish_oauth_login(ocfg, token, no_verify).await
}

/// OpenAI device-auth 登录:不占用本机回调端口,适配 1455 被系统排除的主机。
pub(crate) async fn run_login_device_auth(no_verify: bool) -> anyhow::Result<()> {
    use provider::oauth;

    println!("\n== ridgecode login --codex --device-auth ==\n");
    println!("Requesting device code…");
    std::io::stdout().flush().ok();
    let http = provider::http::ReqwestClient::new();
    let device = oauth::request_device_code(&http, oauth::OPENAI.client_id)
        .await
        .map_err(|e| anyhow::anyhow!("device auth request failed: {e}"))?;
    let opened = open_in_browser(oauth::OPENAI_DEVICE_VERIFICATION_URL);
    println!(
        "1) {}:\n{}\n",
        if opened {
            "已在默认浏览器打开设备授权页（下方 URL 仍可手动复制）"
        } else {
            "请在浏览器打开设备授权页"
        },
        oauth::OPENAI_DEVICE_VERIFICATION_URL
    );
    println!("2) 在页面输入一次性设备码：{}", device.user_code);
    println!("   等待浏览器完成授权（最多 15 分钟）…");
    std::io::stdout().flush().ok();

    let authorization = oauth::poll_device_code(&http, &device)
        .await
        .map_err(|e| anyhow::anyhow!("device auth polling failed: {e}"))?;
    let token = oauth::exchange_code_with_redirect(
        &http,
        &oauth::OPENAI,
        &authorization.authorization_code,
        &authorization.code_verifier,
        oauth::OPENAI_DEVICE_REDIRECT_URI,
        now_epoch(),
    )
    .await
    .map_err(|e| anyhow::anyhow!("token exchange failed: {e}"))?;
    finish_oauth_login(&oauth::OPENAI, token, no_verify).await
}

pub(crate) enum DeviceOAuthEvent {
    Ready { user_code: String, opened: bool },
    Complete(Result<provider::oauth::OAuthToken, String>),
}

pub(crate) struct DeviceOAuthFlow {
    pub(crate) receiver: tokio::sync::mpsc::UnboundedReceiver<DeviceOAuthEvent>,
    cancel: tokio::task::AbortHandle,
}

impl Drop for DeviceOAuthFlow {
    fn drop(&mut self) {
        self.cancel.abort();
    }
}

pub(crate) fn start_device_oauth() -> DeviceOAuthFlow {
    let (tx, receiver) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        let result = async {
            let http = provider::http::ReqwestClient::new();
            let device =
                provider::oauth::request_device_code(&http, provider::oauth::OPENAI.client_id)
                    .await
                    .map_err(|e| format!("device auth request failed: {e}"))?;
            let opened = open_in_browser(provider::oauth::OPENAI_DEVICE_VERIFICATION_URL);
            let _ = tx.send(DeviceOAuthEvent::Ready {
                user_code: device.user_code.clone(),
                opened,
            });
            let authorization = provider::oauth::poll_device_code(&http, &device)
                .await
                .map_err(|e| format!("device auth polling failed: {e}"))?;
            provider::oauth::exchange_code_with_redirect(
                &http,
                &provider::oauth::OPENAI,
                &authorization.authorization_code,
                &authorization.code_verifier,
                provider::oauth::OPENAI_DEVICE_REDIRECT_URI,
                now_epoch(),
            )
            .await
            .map_err(|e| format!("token exchange failed: {e}"))
        }
        .await;
        let _ = tx.send(DeviceOAuthEvent::Complete(result));
    });
    DeviceOAuthFlow {
        receiver,
        cancel: task.abort_handle(),
    }
}

async fn finish_oauth_login(
    ocfg: &provider::oauth::OAuthConfig,
    token: provider::oauth::OAuthToken,
    no_verify: bool,
) -> anyhow::Result<()> {
    let path = save_oauth_token(ocfg.provider, &token)?;
    let profile_name = register_oauth_profile(ocfg.provider);
    if let Some(name) = profile_name.as_deref() {
        println!(
            "     profile    -> {}  (name \"{name}\", oauth)",
            config_path()
        );
        let cfg_path = config_path();
        let cfg = Config::load(&cfg_path);
        let (model, base) = oauth_model_and_base(&cfg, ocfg.provider);
        let text = std::fs::read_to_string(&cfg_path).unwrap_or_default();
        match agent::config_set_selection(&text, name, &model, &base) {
            Ok(updated) => {
                if let Err(error) = std::fs::write(&cfg_path, updated) {
                    eprintln!(
                        "warning: OAuth credential saved, active selection not saved: {error}"
                    );
                }
            }
            Err(error) => {
                eprintln!("warning: OAuth credential saved, active selection not saved: {error}")
            }
        }
    }
    println!("\n[OK] 订阅已接入({})。", ocfg.provider);
    println!(
        "     credential saved -> {path}  (OAuth tokens + account metadata, chmod 600 where supported)"
    );
    println!("     just run: ridgecode   (启动自动用订阅凭据,过期自动刷新)");
    if !no_verify {
        eprint!("verifying subscription reaches the model … ");
        std::io::stderr().flush().ok();
        let cfg = Config::load(config_path());
        let (model, base) = oauth_model_and_base(&cfg, ocfg.provider);
        let p = oauth_provider(
            ocfg.provider,
            base,
            model,
            token.access_token.clone(),
            token.account_id.clone(),
            provider::DEFAULT_REASONING_EFFORT,
        );
        let req = provider::CompletionRequest {
            messages: vec![Message::user("ping")],
            tools: vec![],
        };
        match p.complete(&req).await {
            Ok(_) => eprintln!("✓ ok"),
            Err(e) => eprintln!(
                "⚠ 测试调用失败({e})。凭据已保存;若 chat 报错,可能需校准活 OAuth wire(见 provider::oauth 验证边界)。"
            ),
        }
    }
    Ok(())
}

fn open_in_browser(url: &str) -> bool {
    #[cfg(windows)]
    let result = std::process::Command::new("rundll32.exe")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn()
        .or_else(|_| std::process::Command::new("explorer.exe").arg(url).spawn());
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = std::process::Command::new("xdg-open").arg(url).spawn();
    result.is_ok()
}

/// A short-lived localhost OAuth callback listener shared by CLI and TUI.
/// The listener starts before the browser opens, serves a real HTML response,
/// and tries the registered Codex fallback port when 1455 is unavailable.
pub(crate) struct LocalOAuthCallback {
    pub(crate) port: u16,
    pub(crate) redirect_uri: String,
    pub(crate) receiver: tokio::sync::oneshot::Receiver<Result<String, String>>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl LocalOAuthCallback {
    pub(crate) async fn wait(mut self) -> Result<String, String> {
        (&mut self.receiver)
            .await
            .map_err(|_| "local OAuth callback listener stopped".to_string())?
    }
}

impl Drop for LocalOAuthCallback {
    fn drop(&mut self) {
        self.cancel
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

pub(crate) fn start_local_callback(expected_state: String) -> Result<LocalOAuthCallback, String> {
    const PORTS: &[u16] = &[1455, 1457];
    let mut errors = Vec::new();
    let (listener, port) = PORTS
        .iter()
        .find_map(
            |port| match std::net::TcpListener::bind(("127.0.0.1", *port)) {
                Ok(listener) => Some((listener, *port)),
                Err(error) => {
                    errors.push(format!("cannot listen on 127.0.0.1:{port}: {error}"));
                    None
                }
            },
        )
        .ok_or_else(|| {
            format!(
                "{}; tried registered ports 1455 and 1457",
                errors.join("; ")
            )
        })?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("cannot configure localhost:{port}: {e}"))?;
    let (tx, receiver) = tokio::sync::oneshot::channel();
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let thread_cancel = cancel.clone();
    std::thread::spawn(move || {
        let mut sender = Some(tx);
        while !thread_cancel.load(std::sync::atomic::Ordering::Acquire) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    if let Some(result) = handle_local_callback_stream(&mut stream, &expected_state)
                    {
                        if let Some(tx) = sender.take() {
                            let _ = tx.send(result);
                        }
                        break;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(40));
                }
                Err(error) => {
                    if let Some(tx) = sender.take() {
                        let _ = tx.send(Err(format!("local OAuth listener failed: {error}")));
                    }
                    break;
                }
            }
        }
    });
    Ok(LocalOAuthCallback {
        port,
        redirect_uri: format!("http://localhost:{port}/auth/callback"),
        receiver,
        cancel,
    })
}

fn handle_local_callback_stream(
    stream: &mut std::net::TcpStream,
    expected_state: &str,
) -> Option<Result<String, String>> {
    use std::io::Read;
    let mut request = [0u8; 8192];
    let size = stream.read(&mut request).ok()?;
    let first = String::from_utf8_lossy(&request[..size])
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    let path = first
        .strip_prefix("GET ")
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or(&first);
    if path == "/" {
        let _ = write_http_response(
            stream,
            "200 OK",
            "text/html; charset=utf-8",
            "<h2>RidgeCode is waiting for the OAuth callback.</h2><p>Keep this window open while you authorize.</p>",
        );
        return None;
    }
    if path == "/favicon.ico" {
        let _ = write_http_response(stream, "204 No Content", "text/plain", "");
        return None;
    }
    if !path.starts_with("/auth/callback") {
        let _ = write_http_response(stream, "404 Not Found", "text/plain", "Not Found");
        return None;
    }
    let Some((code, state)) = provider::oauth::parse_callback_path(&first) else {
        let _ = write_http_response(
            stream,
            "400 Bad Request",
            "text/html; charset=utf-8",
            "<h2>OAuth callback did not contain a code.</h2><p>Restart login in RidgeCode.</p>",
        );
        return Some(Err("OAuth callback did not contain a code".into()));
    };
    if state != expected_state {
        let _ = write_http_response(
            stream,
            "400 Bad Request",
            "text/html; charset=utf-8",
            "<h2>OAuth state mismatch.</h2><p>Restart login in RidgeCode.</p>",
        );
        return Some(Err("OAuth state mismatch; restart login".into()));
    }
    let _ = write_http_response(
        stream,
        "200 OK",
        "text/html; charset=utf-8",
        "<h2>RidgeCode login successful.</h2><p>You can close this window.</p>",
    );
    Some(Ok(format!("{code}#{state}")))
}

fn write_http_response(
    stream: &mut std::net::TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) -> std::io::Result<()> {
    use std::io::Write;
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

/// iter-48 G3:把订阅登记为 `use_oauth` 命名档(同名覆盖,best-effort)。
/// 成功返回档名;失败静默 None(凭据已落 oauth.json,档只是列切便利)。
fn read_pasted_authorization(expected_state: &str) -> anyhow::Result<String> {
    println!("   授权后复制浏览器地址栏中的完整 localhost 回调 URL，或粘贴 code#state:",);
    eprint!("callback: ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    provider::oauth::parse_authorization_input(&line, expected_state)
        .map_err(|e| anyhow::anyhow!(e.to_string()))
}

pub(crate) fn register_oauth_profile(provider_id: &str) -> Option<String> {
    let (dm, db) = oauth_defaults(provider_id);
    let prof = agent::ProviderProfile {
        name: if provider_id == "anthropic" {
            "claude-max".to_string()
        } else {
            "chatgpt-plus".to_string()
        },
        kind: provider_id.to_string(),
        model: dm.to_string(),
        base_url: db.to_string(),
        key_env: String::new(),
        api_key: None,
        use_oauth: Some(true),
        route: None,
    };
    let cfg_path = config_path();
    let cfg_text = std::fs::read_to_string(&cfg_path).unwrap_or_default();
    let updated = agent::config_add_provider(&cfg_text, &prof).ok()?;
    if let Some(dir) = std::path::Path::new(&cfg_path).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(&cfg_path, updated).ok()?;
    Some(prof.name)
}

/// key 全无时的回退(iter-43;iter-48 泛化) —— oauth.json 依次找 anthropic → openai
/// 订阅 token,命中则构造 bearer-mode provider。过期(含 60s 余量)先刷新并落盘;
/// 刷新失败退回旧 token 让 API 定夺。
pub(crate) async fn resolve_claude_oauth_provider(
    cfg: &Config,
    effort: &str,
) -> Option<Arc<dyn LlmProvider>> {
    let text = std::fs::read_to_string(oauth_path()).ok()?;
    for ocfg in [&provider::oauth::ANTHROPIC, &provider::oauth::OPENAI] {
        let Some(mut token) = agent::oauth_get(&text, ocfg.provider) else {
            continue;
        };
        let now = now_epoch();
        if token.needs_refresh(now) {
            let http = provider::http::ReqwestClient::new();
            match provider::oauth::refresh(&http, ocfg, &token.refresh_token, now).await {
                Ok(mut fresh) => {
                    fresh.preserve_chatgpt_metadata_from(&token);
                    let _ = save_oauth_token(ocfg.provider, &fresh);
                    token = fresh;
                }
                Err(e) => eprintln!("[ridgecode] OAuth refresh failed: {e}; using existing token"),
            }
        }
        let (mut model, base) = oauth_model_and_base(cfg, ocfg.provider);
        if ocfg.provider == "openai" {
            // ChatGPT subscriptions expose an account-scoped Codex catalog;
            // the public/API default (for example `gpt-5`) may be rejected by
            // the subscription endpoint. Prefer the configured model only
            // when it is present in that live catalog, otherwise use its first
            // visible entry and keep startup usable in headless mode too.
            let http = provider::http::ReqwestClient::new();
            match tokio::time::timeout(
                std::time::Duration::from_secs(10),
                provider::models::fetch_chatgpt_models(
                    &http,
                    &base,
                    &token.access_token,
                    token.account_id.as_deref(),
                ),
            )
            .await
            {
                Ok(Ok(models)) if !models.is_empty() => {
                    let selected = models
                        .iter()
                        .find(|candidate| candidate.id.eq_ignore_ascii_case(&model))
                        .or_else(|| models.first());
                    if let Some(selected) = selected {
                        if selected.id != model {
                            eprintln!(
                                "[ridgecode] ChatGPT model {model} unavailable; using account model {}",
                                selected.id
                            );
                            model = selected.id.clone();
                        }
                    }
                }
                Ok(Err(error)) => eprintln!(
                    "[ridgecode] ChatGPT model catalog unavailable: {error}; keeping model {model}"
                ),
                Err(_) => eprintln!(
                    "[ridgecode] ChatGPT model catalog timed out (10s); keeping model {model}"
                ),
                Ok(Ok(_)) => {}
            }
        }
        eprintln!(
            "[ridgecode] starting with {} subscription (OAuth) · {model}",
            ocfg.provider
        );
        return Some(oauth_provider(
            ocfg.provider,
            base,
            model,
            token.access_token,
            token.account_id,
            effort,
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 连接校验核(iter-38):stub HttpClient 零网络 —— 模型 JSON → Ok(数);get 失败 → Err。
    #[tokio::test]
    async fn verify_key_via_maps_result() {
        struct StubHttp(Result<serde_json::Value, String>);
        #[async_trait::async_trait]
        impl provider::http::HttpClient for StubHttp {
            async fn post_json(
                &self,
                _u: &str,
                _h: &[(String, String)],
                _b: &serde_json::Value,
            ) -> Result<serde_json::Value, provider::ProviderError> {
                Err("GET only".into())
            }
            async fn get_json(
                &self,
                _u: &str,
                _h: &[(String, String)],
            ) -> Result<serde_json::Value, provider::ProviderError> {
                match &self.0 {
                    Ok(v) => Ok(v.clone()),
                    Err(e) => Err(e.as_str().into()),
                }
            }
        }
        let ok = StubHttp(Ok(serde_json::json!({"data":[{"id":"m1"},{"id":"m2"}]})));
        assert_eq!(verify_key_via(&ok, "openai", "https://x", "k").await, Ok(2));
        let bad = StubHttp(Err("http 401: unauthorized".into()));
        assert!(verify_key_via(&bad, "openai", "https://x", "k")
            .await
            .is_err());
    }

    #[test]
    fn local_callback_serves_waiting_page_and_returns_code() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let expected = "state-123".to_string();
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            handle_local_callback_stream(&mut stream, &expected)
        });
        let mut client = std::net::TcpStream::connect(addr).unwrap();
        client
            .write_all(b"GET /auth/callback?code=code-abc&state=state-123 HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        let result = thread.join().unwrap();
        assert_eq!(result, Some(Ok("code-abc#state-123".into())));
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("RidgeCode login successful"));
    }
}
