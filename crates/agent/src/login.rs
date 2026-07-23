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
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--list" | "-l" => list = true,
            "--no-default" => make_default = false,
            "--default" => make_default = true,
            "--no-verify" => no_verify = true,
            "--claude" => oauth_claude = true, // iter-43:OAuth 订阅登录(接 Claude Pro/Max)
            "--codex" => oauth_codex = true,   // iter-48:OAuth 订阅登录(接 ChatGPT Plus/Pro)
            "--model" => model = it.next().cloned(),
            "--name" => name = it.next().cloned(),
            _ => positional.push(a),
        }
    }
    // OAuth(PKCE)订阅登录,与 api-key 登录分道(iter-43 claude / iter-48 codex)。
    if oauth_claude {
        return run_login_oauth(&provider::oauth::ANTHROPIC, no_verify).await;
    }
    if oauth_codex {
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
        "openai" => ("gpt-5", "https://api.openai.com/v1"),
        _ => ("claude-sonnet-4-6", "https://api.anthropic.com/v1"),
    }
}

/// 据订阅凭据构造 bearer-mode provider(anthropic 走专用 OAuth wire,其余走 OpenAI bearer)。
fn oauth_provider(
    provider_id: &str,
    base: String,
    model: String,
    access: String,
) -> Arc<dyn LlmProvider> {
    if provider_id == "anthropic" {
        Arc::new(AnthropicProvider::new_oauth(base, model, access))
    } else {
        Arc::new(OpenAiProvider::new(base, model, access))
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
    let url = oauth::authorize_url(ocfg, &pkce.challenge, &state);
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
    println!("1) 在浏览器打开以下 URL,用你的订阅账号授权:\n\n{url}\n");
    // 取 code:openai 重定向到本地回调端口,起临时 listener 接;anthropic 回调页显码,用户回贴。
    let code = if ocfg.token_wire == oauth::TokenWire::Form {
        println!("2) 等待浏览器授权回调(监听 localhost:1455;Ctrl-C 取消)…");
        let expect = state.clone();
        tokio::task::spawn_blocking(move || read_local_callback("127.0.0.1:1455", &expect))
            .await??
    } else {
        println!("2) 授权后页面会显示形如 `code#state` 的码。粘贴到此处并回车:");
        eprint!("code: ");
        std::io::stderr().flush().ok();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        line.trim().to_string()
    };
    if code.is_empty() {
        anyhow::bail!("no authorization code provided; aborted (nothing written).");
    }
    let http = provider::http::ReqwestClient::new();
    let token = oauth::exchange_code(&http, ocfg, &code, &pkce.verifier, now_epoch())
        .await
        .map_err(|e| anyhow::anyhow!("token exchange failed: {e}"))?;
    let path = save_oauth_token(ocfg.provider, &token)?;
    // iter-48 G3:顺手登记 use_oauth 命名档 —— /provider 页可列可切,订阅成一等公民。
    if let Some(name) = register_oauth_profile(ocfg.provider) {
        println!(
            "     profile    -> {}  (name \"{name}\", oauth)",
            config_path()
        );
    }
    println!("\n[OK] 订阅已接入({})。", ocfg.provider);
    println!("     credential saved -> {path}  (access+refresh, chmod 600 where supported)");
    println!("     just run: ridgecode   (启动自动用订阅凭据,过期自动刷新)");
    // best-effort 校验(默认;--no-verify 跳过):bearer 打一次最小调用证明能到模型。
    // 失败仅告警不判失败 —— 活 OAuth wire(端点/system 前缀/beta 头)无法离线核验。
    if !no_verify {
        eprint!("verifying subscription reaches the model … ");
        std::io::stderr().flush().ok();
        let (dm, db) = oauth_defaults(ocfg.provider);
        let model = std::env::var("RIDGE_MODEL").unwrap_or_else(|_| dm.to_string());
        let base = std::env::var("RIDGE_BASE_URL").unwrap_or_else(|_| db.to_string());
        let p = oauth_provider(ocfg.provider, base, model, token.access_token.clone());
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

/// iter-48 G3:把订阅登记为 `use_oauth` 命名档(同名覆盖,best-effort)。
/// 成功返回档名;失败静默 None(凭据已落 oauth.json,档只是列切便利)。
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

/// 起临时本地 TCP listener 接 OAuth 回调(阻塞;spawn_blocking 里跑)。
/// 循环 accept 直到某请求首行含 code(浏览器杂请求如 favicon 跳过);校验 state 防 CSRF。
fn read_local_callback(addr: &str, expected_state: &str) -> anyhow::Result<String> {
    use std::io::{BufRead, BufReader, Write as _};
    let listener = std::net::TcpListener::bind(addr)
        .map_err(|e| anyhow::anyhow!("cannot listen on {addr}: {e} (port in use?)"))?;
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let mut first = String::new();
        if BufReader::new(&stream).read_line(&mut first).is_err() {
            continue;
        }
        let Some((code, state)) = provider::oauth::parse_callback_path(first.trim()) else {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n");
            continue;
        };
        if state != expected_state {
            let _ =
                stream.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\nstate mismatch; restart login");
            anyhow::bail!("OAuth state mismatch (possible CSRF); aborted.");
        }
        let _ = stream.write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/html\r\n\r\n<h2>Login successful. You can close this window.</h2>",
        );
        return Ok(code);
    }
    anyhow::bail!("listener closed before callback arrived")
}

/// key 全无时的回退(iter-43;iter-48 泛化) —— oauth.json 依次找 anthropic → openai
/// 订阅 token,命中则构造 bearer-mode provider。过期(含 60s 余量)先刷新并落盘;
/// 刷新失败退回旧 token 让 API 定夺。
pub(crate) async fn resolve_claude_oauth_provider(cfg: &Config) -> Option<Arc<dyn LlmProvider>> {
    let text = std::fs::read_to_string(oauth_path()).ok()?;
    for ocfg in [&provider::oauth::ANTHROPIC, &provider::oauth::OPENAI] {
        let Some(mut token) = agent::oauth_get(&text, ocfg.provider) else {
            continue;
        };
        let now = now_epoch();
        if token.needs_refresh(now) {
            let http = provider::http::ReqwestClient::new();
            match provider::oauth::refresh(&http, ocfg, &token.refresh_token, now).await {
                Ok(fresh) => {
                    let _ = save_oauth_token(ocfg.provider, &fresh);
                    token = fresh;
                }
                Err(e) => eprintln!("[ridgecode] OAuth refresh failed: {e}; using existing token"),
            }
        }
        let (dm, db) = oauth_defaults(ocfg.provider);
        let model = std::env::var("RIDGE_MODEL")
            .ok()
            .or_else(|| cfg.model.clone())
            .unwrap_or_else(|| dm.to_string());
        let base = std::env::var("RIDGE_BASE_URL")
            .ok()
            .or_else(|| cfg.base_url.clone())
            .unwrap_or_else(|| db.to_string());
        eprintln!(
            "[ridgecode] starting with {} subscription (OAuth) · {model}",
            ocfg.provider
        );
        return Some(oauth_provider(
            ocfg.provider,
            base,
            model,
            token.access_token,
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
}
