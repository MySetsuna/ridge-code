use super::*;

/// 当前 API key 解析(供 `/models` 抓取、`/model` 热切用):走 iter-41 收敛的
/// [`resolve_top_level_key`](env `RIDGE_API_KEY` > 顶层内联 `api_key` > 顶层 `key_env`→env/auth)。都无 → None。
pub(crate) fn current_api_key() -> Option<String> {
    resolve_top_level_key(&Config::load(config_path()), &load_auth())
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

/// 热切换模型(iter-32 共用路径):密钥经 `current_api_key`(env 优先,回落 config 内联)——
/// `/model <name>` 文本命令与模型选择器浮窗同走此路,顺带修「内联 key 无法切模型」根因。
pub(crate) fn swap_model(swap: &Arc<SwapProvider>, meta: &mut ReplMeta, model: &str, ui: &mut Ui) {
    match current_api_key() {
        Some(key) => {
            swap.swap(make_provider(&meta.provider, model, &meta.base_url, key));
            meta.model = model.to_string();
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
        Some(p) => match p.resolve_key_with(&load_auth()) {
            Some(key) => {
                swap.swap(make_provider(&p.kind, &p.model, &p.base_url, key));
                meta.provider = p.kind;
                meta.model = p.model;
                meta.base_url = p.base_url;
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
            if key == "provider" {
                meta.provider = val.to_string();
            } else {
                meta.base_url = val.to_string();
            }
            if let Some(k) = current_api_key() {
                swap.swap(make_provider(
                    &meta.provider,
                    &meta.model,
                    &meta.base_url,
                    k,
                ));
            }
        }
        "status_bar" => {
            meta.status_bar = if val.trim().is_empty() {
                DEFAULT_STATUS_BAR.to_string()
            } else {
                val.to_string()
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
        // 模型页:切到选中模型 + 缓存其真实上下文窗口。
        (PanelKind::Models, _) => {
            if let Some(id) = sel_key {
                if let Some(w) = sel_ctx {
                    meta.ctx_window = w;
                }
                swap_model(swap, meta, &id, ui);
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
                p.editing = Some(String::new());
                p.title =
                    format!("Login · enter API key for {id} · Enter verify & connect · Esc cancel");
            }
        }
        (PanelKind::Login, Some(_)) => {}
        // 只读页:Enter 关页。
        (PanelKind::Tools, _)
        | (PanelKind::Agent, _)
        | (PanelKind::Mcp, _)
        | (PanelKind::Skills, _) => ui.panel = None,
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
    match input {
        "/exit" | "/quit" => return Ok(true),
        "/help" => ui.note("/exit /reset /compact /cost /tools /login [list|<id> <key>] /model [<name>] (no arg = live model picker) /provider [list|use <name>|add ...] /agent /mcp /init (generate AGENTS.md) /skills /commands (custom /name from ~/.ridge/commands/*.md + skills; $ARGS) /config [set key value] /jailbreak [on|off]; @path to reference a file; Ctrl-C to interrupt; scroll/select history with the terminal's native keys; approval prompt: y/Enter approve, n/Esc reject, ↑↓ scroll details.", Color::Gray),
        "/tools" => ui.panel = Some(tools_panel(&meta.tools)),
        "/reset" => { history.clear(); save_session(&session_path(), history); ui.note("context cleared", Color::Yellow); }
        "/compact" => { let n = history.len(); *history = compact_history(std::mem::take(history), 4); ui.note(format!("context compacted: {n} → {} messages", history.len()), Color::Yellow); }
        "/cost" => ui.note(format!("session total: {tokens} tokens · {turns} tasks"), Color::Gray),
        // /model 单命令(iter-37 合并):无参 → 实时模型页(↑↓ 选、Enter 切);`/model <name>` → 直接热切。
        // `/models` `/model pick` 保留为别名(旧肌肉记忆),补全表只呈现 `/model`。
        _ if input == "/model" || input == "/models" || input == "/model pick" => {
            match current_api_key() {
                Some(key) => {
                    let http = provider::http::ReqwestClient::new();
                    let fut = provider::models::fetch_models(&http, &meta.provider, &meta.base_url, &key);
                    match tokio::time::timeout(Duration::from_secs(15), fut).await {
                        Ok(Ok(list)) if !list.is_empty() => {
                            // 命中当前模型即缓存其真实上下文窗口 → 顶/底栏 ctx% 分母转真值(iter-31)。
                            if let Some(n) = list.iter().find(|m| m.id == meta.model).and_then(|m| m.context) {
                                meta.ctx_window = n;
                            }
                            ui.panel = Some(models_panel(&list, &meta.model));
                        }
                        Ok(Ok(_)) => ui.note("endpoint returned an empty model list", Color::Yellow),
                        Ok(Err(e)) => ui.note(format!("failed to fetch models: {e}"), Color::Red),
                        Err(_) => ui.note("fetching models timed out (15s)", Color::Red),
                    }
                }
                None => ui.note("no API key resolved (set RIDGE_API_KEY or api_key at config.json top level)", Color::Red),
            }
        }
        _ if input.starts_with("/model ") => swap_model(swap, meta, input[7..].trim(), ui),
        _ if input == "/jailbreak" => {
            let on = agent::allow_jailbreak();
            ui.note(if on { "jailbreak: ON ⚠ (can write outside cwd subtree; disaster commands / protected paths / read-only still blocked). Disable: /jailbreak off" } else { "jailbreak: OFF (writes limited to cwd subtree). Enable: /jailbreak on —— top status bar turns red when on" }, if on { Color::Red } else { Color::Gray });
        }
        _ if input == "/jailbreak on" => { agent::set_allow_jailbreak(true); ui.note("⚠ jailbreak ON: can write outside cwd subtree (disaster commands / protected paths / read-only still hard-blocked). Session only; to persist: /config set allow_jailbreak true", Color::Red); }
        _ if input == "/jailbreak off" => { agent::set_allow_jailbreak(false); ui.note("jailbreak OFF: writes limited back to cwd subtree", Color::Green); }
        _ if input == "/config" => ui.panel = Some(config_panel()),
        _ if input.starts_with("/config set ") => { let parts: Vec<_> = input.splitn(4, ' ').collect(); if parts.len() == 4 { match persist_config(parts[2], parts[3]) { Ok(path) => ui.note(format!("wrote {path}; takes effect next start"), Color::Green), Err(e) => ui.note(format!("write failed: {e}"), Color::Red) } } else { ui.note("usage: /config set <key> <value>", Color::Yellow); } }
        _ if input == "/provider" || input == "/provider list" => {
            let cfg = Config::load(config_path());
            if cfg.providers.is_empty() { ui.note("no provider profiles. Add: /provider add <name> <kind> <model> <base_url> [key_env]", Color::Gray); }
            else { ui.panel = Some(provider_panel()); }
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
        _ if input.starts_with("/provider use ") => switch_provider(input[14..].trim(), meta, swap, ui),
        _ if input == "/login" => ui.panel = Some(login_panel()),
        _ if input == "/login list" => {
            let ids: Vec<&str> = PROVIDER_PRESETS.iter().map(|p| p.id).collect();
            ui.note(format!("built-in providers: {}\ninteractive: /login  ·  quick: /login <id> <API_KEY> (verified; key → ~/.ridge/auth.json, not config)", ids.join(", ")), Color::Gray);
        }
        _ if input.starts_with("/login ") => {
            let rest = input["/login ".len()..].trim();
            let mut it = rest.split_whitespace();
            match (it.next(), it.next()) {
                (Some(id), Some(key)) => match preset_by_id(id) {
                    Some(preset) => login_apply_verified(preset, key, meta, swap, ui).await,
                    None => ui.note(format!("unknown provider \"{id}\"; see /login list"), Color::Yellow),
                },
                _ => ui.note("usage: /login <id> <API_KEY>, or just /login to pick interactively", Color::Yellow),
            }
        }
        _ if input == "/agent" => {
            if agents.defs.is_empty() { ui.note("no sub-agents available", Color::Gray); }
            else { ui.panel = Some(agent_panel(&agents.defs)); }
        }
        _ if input == "/mcp" => {
            let cfg = Config::load(config_path());
            if cfg.mcp.is_empty() { ui.note("no MCP servers configured. Add them under \"mcp\": [ ... ] in ~/.ridge/config.json (each: name + cmd [+ args]).", Color::Gray); }
            else { ui.panel = Some(mcp_panel()); }
        }
        _ if input == "/skills" => {
            if skills.is_empty() { ui.note("no skills loaded. Add ~/.ridge/skills/<name>/SKILL.md (frontmatter name/description + body); loaded skills are injected into the system prompt.", Color::Gray); }
            else { ui.panel = Some(skills_panel(skills)); }
        }
        _ if input == "/commands" => {
            if commands.is_empty() { ui.note("no custom commands. Add ~/.ridge/commands/<name>.md (body = prompt, $ARGS = args); skills also appear here.", Color::Gray); }
            else {
                let lines: Vec<String> = commands.iter().map(|c| if c.description.is_empty() { format!("/{}", c.name) } else { format!("/{}  —— {}", c.name, c.description) }).collect();
                ui.note(format!("commands ({}):\n{}", commands.len(), lines.join("\n")), Color::Gray);
            }
        }
        // 自定义 / skill 命令(iter-39):/name [args] → 展开 body(替 $ARGS)为任务(置 run_task,主环起任务)。
        _ if input.starts_with('/') => {
            let rest = &input[1..];
            let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            match agent::resolve_command(name, commands) {
                Some(cmd) => ui.run_task = Some(agent::expand_command(&cmd.body, args.trim())),
                None => ui.note(format!("unknown command: {input} (/help · /commands)"), Color::Yellow),
            }
        }
        _ => return Ok(false),
    }
    Ok(false)
}
