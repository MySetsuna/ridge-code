use std::io::{IsTerminal, Write};
use std::sync::Arc;

use agent::{
    agent_run_config, apply_login, auth_parse, auth_upsert, auto_signal_from_run, build_agent,
    build_llm_agent_full, builtin_tool_specs, compact_history, default_tool, est_tokens,
    expand_mentions, extract_signals_from_run, halt_reason, load_commands, load_signal_block,
    load_skills, null_token_bus, preset_by_id, render_todos, resolve_mcp, resolve_top_level_key,
    scripted, signal_extract_enabled, tool_output_failed, write_run, AgentState, Approver,
    AutoApprove, Color, Config, McpTools, RichOutput, Skill, SlashCommand, Todo, TokenBus,
    PROVIDER_PRESETS,
};
use langgraph::{CompiledGraph, StreamEvent};
use mcp::{McpClient, McpError, StdioTransport};
use provider::{
    AnthropicProvider, Completion, LlmProvider, Message, OpenAiProvider, ScriptedProvider,
    SwapProvider,
};

mod tui;
use login::*;
use run::*;

/// TUI 展示用元信息(`/tools` `/model` 命令用)。
struct ReplMeta {
    tools: Vec<String>,
    /// Runtime provider kind used by dispatch (`openai`/`anthropic`).
    provider: String,
    /// Human-facing provider/profile label; intentionally separate from kind.
    provider_label: String,
    model: String,
    base_url: String,
    /// 输入框下方自定义状态条模板(iter-31):config `status_bar` 或内置默认。
    status_bar: String,
    /// 当前模型上下文窗口(iter-31):ctx% 分母。默认 `DEFAULT_CTX_WINDOW`,`/models` 命中即刷新。
    ctx_window: u64,
}

/// ridgecode —— 通用 agent CLI(产品名 RidgeCode)。
///
/// 用法:
///   ridgecode                                # 交互式 TUI(有 key);管道/非 TTY 则 headless
///   ridgecode "修复编译错误"                  # 一次性任务
///   ridgecode --cwd /path/to/project "..."    # 在目标项目里跑
///   ridgecode --yolo "..."                    # skip-danger:工具自动放行不问 [y/N]
///
/// 配置(环境变量):RIDGE_API_KEY / RIDGE_PROVIDER(anthropic|openai)/ RIDGE_MODEL / RIDGE_BASE_URL / RIDGE_PROXY
/// `--help` / `--version` 帮助与版本(1.0 级 CLI 该有的),命中就打印并返回 true(不进主流程)。
fn handle_meta_flags() -> bool {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("ridgecode {}", env!("CARGO_PKG_VERSION"));
        return true;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "RidgeCode —— modular general-purpose agent CLI (binary: ridgecode)\n\n\
             Usage:\n  \
             ridgecode                      interactive TUI (no key required to open; use /login inside; non-TTY falls back to headless)\n  \
             ridgecode \"task\"               one-shot task\n  \
             ridgecode --resume             resume the last session (continue after kill-9 / reopen)\n\n\
             ridgecode goal ...             persist and advance one long-running goal\n  \
             Options:\n  \
             --cwd <dir>                    run inside the target project directory\n  \
             --every <30s|5m|1h>            time trigger: re-run the task on an interval (resident; reloads compounding signals each round, Ctrl-C to stop)\n  \
             --yolo/--skip-permissions      skip-danger: auto-approve tools without [y/N] (disaster commands still blocked)\n  \
             --read-only                    read-only mode: only offer read/search/research tools, reject all write/shell side effects\n  \
             --resume/--continue            resume the last session\n  \
             -h/--help, -V/--version        this help / version\n\n\
             In the TUI: slash commands /model /provider /config /agent /compact etc.; @path to reference a file, Ctrl-C interrupts; press twice within 2 seconds to exit.\
             Pipe/non-TTY: stdin lines are run as tasks (headless, no slash commands).\n\n\
             Config: ~/.ridge/config.json (provider/model/budget/multiple mcp/skills; env overrides);\
             /config set <key> <value> in the TUI persists changes. Key: RIDGE_API_KEY env, or a config profile's api_key (plaintext) / key_env (env var name).\
             ~/.ridge/skills/*/SKILL.md adds domain skills without touching source.\n  \
             RIDGE_EXTRACT_SIGNALS=1        opt-in: at run end, use one LLM pass to distill the trace into compounding signals (off by default, saves tokens)."
        );
        return true;
    }
    false
}

/// 真实终端默认判定不变；仅显式 `RIDGE_FORCE_TUI=1` 供隔离诊断 harness 进入 TUI。
fn tui_requested() -> bool {
    (std::env::var("RIDGE_FORCE_TUI").ok().as_deref() == Some("1"))
        || (std::io::stdin().is_terminal() && std::io::stdout().is_terminal())
}

/// 隔离 TUI 验收用的无网络 fixture；普通运行与非 TTY 完全不受影响。
fn tui_fixture_provider(fallback: Arc<dyn LlmProvider>) -> Arc<dyn LlmProvider> {
    if !tui_requested() {
        return fallback;
    }
    match std::env::var("RIDGE_TUI_FIXTURE").ok().as_deref() {
        Some("busy") => Arc::new(
            ScriptedProvider::new(vec![Completion {
                reasoning:
                    "fixture reasoning: waiting without network; queue and takeover remain available"
                        .into(),
                text: "fixture answer: busy state completed".into(),
                ..Default::default()
            }])
            .with_delay(std::time::Duration::from_secs(30)),
        ),
        Some("stress") => {
            let reasoning = format!(
                "STRESS_REASONING_BEGIN\n调查窗口：长 Markdown/CJK 流保持可读与可接管。\n{}\n{}",
                "思考片段：中文宽字符、emoji 🧪、`inline-token` 与窗口重排必须保持边界。 ".repeat(640),
                (0..160)
                    .map(|index| format!("- 检查 {index}: 终端宽度变化后仍保留语义轨与上下文。"))
                    .collect::<Vec<_>>()
                    .join("\n")
                    + "\nSTRESS_REASONING_END"
            );
            let text = format!(
                "STRESS_ANSWER_BEGIN\n## 压力夹具结论\n{}\n{}\n\n```rust\nfn cjk_boundary() {{ /* stable */ }}\n```",
                "回答片段：这是长 Markdown/CJK 内容，用于真实 ConPTY resize 与滚回验证。 ".repeat(640),
                (0..160)
                    .map(|index| format!("第 {index} 行：Answer 仍应可从历史与回看入口恢复。"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            let provider = ScriptedProvider::new(vec![Completion {
                reasoning,
                text,
                ..Default::default()
            }])
            .with_delay(std::time::Duration::from_millis(1500));
            let provider = if std::env::var("RIDGE_TUI_INSPECT_ANSWER").ok().as_deref() == Some("1")
            {
                provider.with_post_answer_delay(std::time::Duration::from_millis(1200))
            } else {
                provider
            };
            Arc::new(provider)
        }
        Some("complete") => Arc::new(ScriptedProvider::new(vec![Completion {
            reasoning: "fixture reasoning: completed path remains inspectable".into(),
            text: "fixture answer: final response reached scrollback".into(),
            ..Default::default()
        }])),
        _ => fallback,
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if handle_meta_flags() {
        return Ok(());
    }
    // `login` 子命令:内置供应商一键接入(在解析任务前拦下,免被当成任务串)。
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.first().map(|s| s.as_str()) == Some("login") {
        apply_config_proxy(&load_config()); // CLI login 的连通校验也走 config 里配的代理
        return run_login(&raw[1..]).await;
    }
    if raw.first().map(|s| s.as_str()) == Some("goal") {
        return match agent::goal_command(&raw[1..]) {
            Ok(text) => {
                println!("{text}");
                Ok(())
            }
            Err(error) => Err(anyhow::anyhow!(error)),
        };
    }
    init_tracing();
    let ParsedArgs {
        task,
        cwd,
        skip_danger: cli_skip_danger,
        resume,
        read_only,
        every,
    } = parse_args();
    if let Some(dir) = &cwd {
        std::env::set_current_dir(dir)?;
    }
    let cfg = load_config(); // ~/.ridge/config.json(env 仍覆盖)
    apply_config_proxy(&cfg); // 配了 proxy 则注入 HTTP(S)_PROXY,出站 HTTP 走它(墙内接 x.ai/claude 等)
    let auth = load_auth(); // ~/.ridge/auth.json 密钥库(login 存的 key)

    // key 优先(env/inline/key_env/providers[]);全无则回退 OAuth 订阅凭据(iter-43:login --claude)。
    let effort = resolve_reasoning_effort(&cfg);
    let configured_provider = real_provider(&cfg, &auth);
    let using_oauth = configured_provider.is_none();
    let resolved = match configured_provider {
        Some(p) => Some(p),
        None => resolve_claude_oauth_provider(&cfg, &effort).await,
    };
    match resolved {
        Some(p) => {
            let mcp = resolve_configured_mcp(&cfg).await; // config 多 server + 兼容旧 env 单 server
            let skills = load_configured_skills(&cfg); // 声明式技能(领域知识)
            let budget = cfg.budget_tokens.unwrap_or(0); // 0 = 不限
            let skip_danger = cli_skip_danger || cfg.skip_danger.unwrap_or(false);
            // 地址越狱(iter-34):启动从 config 置进程级开关,默认关(TUI 可 /jailbreak 实时切)。
            agent::set_allow_jailbreak(cfg.allow_jailbreak.unwrap_or(false));
            // Hook(iter-40):装 config 声明的 hooks + 通知开关,触发 session_start(内置审计留痕)。
            agent::set_hooks(cfg.hooks.clone());
            agent::set_notify(cfg.notify.unwrap_or(false));
            // 外置沙箱包裹(iter-46):配了则 run_shell 经它跑(真隔离交平台 docker/wsl)。
            agent::set_sandbox_cmd(cfg.sandbox_cmd.clone());
            agent::fire_session_hooks("session_start", "");
            let agents = Arc::new(build_agents(&cfg, &auth)); // sub-agent 注册表(内置 + 用户 + 命名 provider)
            match task {
                Some(t) => run_once(p, mcp, skills, &t, budget, agents, read_only, every).await, // 一次性 / --every 触发器
                None => {
                    // --resume:kill-9 / 关掉重开后恢复上一会话的多轮 history。
                    let initial = if resume {
                        load_session(&session_path())
                    } else {
                        Vec::new()
                    };
                    let (provider_kind, model, base_url) =
                        resolve_start_model_info(&cfg, &auth, using_oauth);
                    let provider_label = resolve_provider_label(&cfg, &provider_kind, &base_url);
                    let mut tools: Vec<String> = builtin_tool_specs()
                        .iter()
                        .map(|s| s.name.clone())
                        .collect();
                    tools.extend(mcp.tool_names()); // 读工具名(在 mcp 被移入交互循环前)
                    let meta = ReplMeta {
                        tools,
                        provider: provider_kind,
                        provider_label,
                        model,
                        base_url,
                        status_bar: cfg
                            .status_bar
                            .clone()
                            .filter(|s| !s.trim().is_empty())
                            .unwrap_or_else(|| tui::DEFAULT_STATUS_BAR.to_string()),
                        ctx_window: tui::DEFAULT_CTX_WINDOW,
                    };
                    // 包一层 SwapProvider,让 TUI 的 /model 能热切换底层模型而不重建图。
                    let swap = Arc::new(SwapProvider::new(tui_fixture_provider(p)));
                    // 终端采用 TUI；管道/非 TTY 退回 headless，避免破坏脚本/重定向调用。
                    if tui_requested() {
                        // 自定义斜杠命令(iter-39):从同一 skills 派生(含 skill-as-命令),再移交 skills 给图。
                        let commands = load_configured_commands(&cfg, &skills);
                        tui::run(
                            swap,
                            mcp,
                            skills,
                            skip_danger,
                            budget,
                            initial,
                            meta,
                            agents,
                            read_only,
                            commands,
                            effort.clone(),
                        )
                        .await
                    } else {
                        // 非 TTY(管道/CI/重定向):极简 headless,无 TUI、无斜杠命令。
                        headless(swap, mcp, skills, budget, initial, agents, read_only).await
                    }
                }
            }
        }
        None => {
            if task.is_none() && tui_requested() {
                let mcp = resolve_configured_mcp(&cfg).await;
                let skills = load_configured_skills(&cfg);
                let budget = cfg.budget_tokens.unwrap_or(0);
                let skip_danger = cli_skip_danger || cfg.skip_danger.unwrap_or(false);
                agent::set_allow_jailbreak(cfg.allow_jailbreak.unwrap_or(false));
                agent::set_hooks(cfg.hooks.clone());
                agent::set_notify(cfg.notify.unwrap_or(false));
                agent::set_sandbox_cmd(cfg.sandbox_cmd.clone());
                agent::fire_session_hooks("session_start", "");
                let agents = Arc::new(build_agents(&cfg, &auth));
                let initial = if resume {
                    load_session(&session_path())
                } else {
                    Vec::new()
                };
                let (provider_kind, model, base_url) = resolve_configured_model_info(&cfg, &auth);
                let provider_label = resolve_provider_label(&cfg, &provider_kind, &base_url);
                let mut tools: Vec<String> = builtin_tool_specs()
                    .iter()
                    .map(|s| s.name.clone())
                    .collect();
                tools.extend(mcp.tool_names());
                let meta = ReplMeta {
                    tools,
                    provider: provider_kind,
                    provider_label,
                    model,
                    base_url,
                    status_bar: cfg
                        .status_bar
                        .clone()
                        .filter(|s| !s.trim().is_empty())
                        .unwrap_or_else(|| tui::DEFAULT_STATUS_BAR.to_string()),
                    ctx_window: tui::DEFAULT_CTX_WINDOW,
                };
                let swap = Arc::new(SwapProvider::new(tui_fixture_provider(
                    missing_key_provider(),
                )));
                let commands = load_configured_commands(&cfg, &skills);
                return tui::run(
                    swap,
                    mcp,
                    skills,
                    skip_danger,
                    budget,
                    initial,
                    meta,
                    agents,
                    read_only,
                    commands,
                    effort,
                )
                .await;
            }
            eprintln!(
                "[ridgecode] no key found, running the offline scripted demo. Provide a key to use a real LLM / TUI, pick one:\n  \
                 · set the RIDGE_API_KEY env var; or\n  \
                 · put \"api_key\" in one of the providers profiles in ~/.ridge/config.json (plaintext, at your own risk),\n    \
                 or point \"key_env\" at an already-exported env var name. See config.example.json in the same directory.\n"
            );
            run_demo().await
        }
    }
}

fn missing_key_provider() -> Arc<dyn LlmProvider> {
    Arc::new(ScriptedProvider::new(vec![Completion {
        text: "No API key is configured yet. Use /login to choose a provider and save a key, then send your task again.".to_string(),
        ..Default::default()
    }]))
}

/// 配置文件路径:`RIDGE_CONFIG` env > `~/.ridge/config.json`。加载与 `/config` 回写共用。
fn config_path() -> String {
    std::env::var("RIDGE_CONFIG").unwrap_or_else(|_| format!("{}/config.json", ridge_home()))
}

/// 加载配置(JSON)。读不到/坏 → 默认空配置(回落 env)。
fn load_config() -> Config {
    let path = config_path();
    let cfg = Config::load(&path);
    if cfg.provider.is_some() || !cfg.mcp.is_empty() {
        eprintln!("[ridgecode] loaded config {path}");
    }
    cfg
}

/// 把代理串注入进程 `HTTP(S)_PROXY`(大小写各一,reqwest/curl 皆认)。空串 = 不动。
/// 此后**新建**的 reqwest 客户端(provider 补全 / 登录 verify / 联网抓取)出站即走它。
/// `/config set proxy` 的 live 应用与启动注入共用本函数(强制覆盖:用户既已明设,以其为准)。
pub(crate) fn apply_proxy_env(proxy: &str) {
    let p = proxy.trim();
    if p.is_empty() {
        return;
    }
    for var in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
        std::env::set_var(var, p);
    }
}

/// 启动时据 config 落代理:`RIDGE_PROXY` > config(`proxy`) > 通用 `HTTP(S)_PROXY` > 直连。
/// 专用配置优先于 shell 的通用代理；临时覆盖请用 `RIDGE_PROXY`。
/// 见 [`apply_proxy_env`]。
fn apply_config_proxy(cfg: &Config) {
    if let Some(v) = std::env::var("RIDGE_PROXY").ok().filter(|s| !s.is_empty()) {
        eprintln!("[ridgecode] proxy ← env RIDGE_PROXY: {v}");
        apply_proxy_env(&v);
        return;
    }
    if let Some(v) = cfg
        .proxy
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        eprintln!("[ridgecode] proxy ← config: {v}");
        apply_proxy_env(v);
        return;
    }
    if let Some(p) = std::env::var("HTTP_PROXY").ok().filter(|s| !s.is_empty()) {
        eprintln!("[ridgecode] proxy ← env(HTTP_PROXY): {p}");
        return;
    }
    if let Some(p) = std::env::var("HTTPS_PROXY").ok().filter(|s| !s.is_empty()) {
        eprintln!("[ridgecode] proxy ← env(HTTPS_PROXY): {p}");
        return;
    }
    eprintln!("[ridgecode] proxy: (直连)");
}

/// 把一个标量键持久化进 config.json(保留其余键;目录/文件不存在则新建)。
fn persist_config(key: &str, value: &str) -> Result<String, String> {
    let path = config_path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let updated = agent::config_set(&text, key, value)?;
    if let Some(dir) = std::path::Path::new(&path).parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, updated).map_err(|e| e.to_string())?;
    Ok(path)
}

/// `~/.ridge` 目录(env 配置与 skills 的家)。
fn ridge_home() -> String {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default();
    format!("{home}/.ridge")
}

/// `~/.ridge/auth.json` 密钥库路径(`login` 存的 API key 的家;**独立于 config,key 不进 config**)。
fn auth_path() -> String {
    std::env::var("RIDGE_AUTH").unwrap_or_else(|_| format!("{}/auth.json", ridge_home()))
}

/// 读密钥库 → `key_env → key` 映射(读不到/坏 → 空表)。供启动解析各 provider 的 key。
fn load_auth() -> std::collections::BTreeMap<String, String> {
    auth_parse(&std::fs::read_to_string(auth_path()).unwrap_or_default())
}

/// best-effort 收紧文件权限到仅属主可读写(unix 0600);非 unix no-op。
fn secure_file(path: &str) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

mod login;
mod run;

/// 会话持久化文件:`RIDGE_SESSION` 或 `~/.ridge/session.json`。存 TUI/headless 会话的对话 history,
/// 供 `--resume` 在 kill-9 / 关掉重开后**恢复多轮上下文**(像 Claude Code 的续接会话)。
fn session_path() -> String {
    std::env::var("RIDGE_SESSION").unwrap_or_else(|_| format!("{}/session.json", ridge_home()))
}

/// 把对话 history 落盘(best-effort,失败不打断使用)。
fn save_session(path: &str, history: &[Message]) {
    if let Some(dir) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string(history) {
        let _ = std::fs::write(path, json);
    }
}

/// 读回落盘的对话 history(读不到/坏 → 空)。
fn load_session(path: &str) -> Vec<Message> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

const MAX_PROMPT_HISTORY: usize = 200;

fn global_input_history_path() -> String {
    std::env::var("RIDGE_INPUT_HISTORY")
        .unwrap_or_else(|_| format!("{}/input-history.json", ridge_home()))
}

fn session_input_history_path() -> String {
    std::env::var("RIDGE_SESSION_INPUT_HISTORY")
        .unwrap_or_else(|_| format!("{}.inputs.json", session_path()))
}

fn load_prompt_history(path: &str) -> Vec<String> {
    let mut values: Vec<String> = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    values.retain(|value| !value.trim().is_empty());
    if values.len() > MAX_PROMPT_HISTORY {
        values.drain(..values.len() - MAX_PROMPT_HISTORY);
    }
    values
}

fn save_prompt_history(path: &str, values: &[String]) {
    if let Some(dir) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let values = values
        .iter()
        .filter(|value| !value.trim().is_empty())
        .rev()
        .take(MAX_PROMPT_HISTORY)
        .cloned()
        .collect::<Vec<_>>();
    let values = values.into_iter().rev().collect::<Vec<_>>();
    if let Ok(json) = serde_json::to_string(&values) {
        if std::fs::write(path, json).is_ok() {
            secure_file(path);
        }
    }
}

fn load_global_input_history() -> Vec<String> {
    load_prompt_history(&global_input_history_path())
}

fn save_global_input_history(values: &[String]) {
    save_prompt_history(&global_input_history_path(), values);
}

fn load_session_input_history() -> Vec<String> {
    load_prompt_history(&session_input_history_path())
}

fn save_session_input_history(values: &[String]) {
    save_prompt_history(&session_input_history_path(), values);
}

/// 接入 MCP 服务器:**config 里的多个 `mcp`** + 兼容旧的单个 env `RIDGE_MCP_CMD`。
/// 降级不崩:单个起不来 → 跳过;都没有 → 空,agent 只用内置工具。
fn spawn_mcp_transport(cmd: &str, args: &[String]) -> Result<StdioTransport, McpError> {
    match StdioTransport::spawn(cmd, args) {
        Ok(transport) => Ok(transport),
        Err(original) if cmd.eq_ignore_ascii_case("codegraph-mcp") && args.is_empty() => {
            // CodeGraph 1.4.x ships one `codegraph` executable; its MCP stdio
            // entry point is `codegraph serve --mcp`. Keep older user configs
            // working without rewriting ~/.ridge/config.json.
            #[cfg(windows)]
            let (fallback_cmd, fallback_args) = (
                "cmd.exe",
                vec![
                    "/d".to_string(),
                    "/s".to_string(),
                    "/c".to_string(),
                    "codegraph serve --mcp".to_string(),
                ],
            );
            #[cfg(not(windows))]
            let (fallback_cmd, fallback_args) =
                ("codegraph", vec!["serve".to_string(), "--mcp".to_string()]);
            match StdioTransport::spawn(fallback_cmd, &fallback_args) {
                Ok(transport) => {
                    eprintln!(
                        "[ridgecode] MCP fallback: {cmd} unavailable; using codegraph serve --mcp"
                    );
                    Ok(transport)
                }
                Err(fallback) => Err(McpError::Transport(format!(
                    "{original}; fallback {fallback_cmd} codegraph serve --mcp failed: {fallback}"
                ))),
            }
        }
        Err(error) => Err(error),
    }
}

async fn resolve_configured_mcp(cfg: &Config) -> McpTools {
    let mut clients = Vec::new();
    // config 声明的多 server。
    for m in &cfg.mcp {
        match spawn_mcp_transport(&m.cmd, &m.args) {
            Ok(t) => clients.push(Arc::new(McpClient::new(m.name.clone(), Box::new(t)))),
            Err(e) => eprintln!("[ridgecode] MCP startup failed {} ({}): {e}", m.name, m.cmd),
        }
    }
    // 兼容旧 env 单 server。
    if let Ok(cmd) = std::env::var("RIDGE_MCP_CMD") {
        if !cmd.is_empty() {
            let name = std::env::var("RIDGE_MCP_NAME").unwrap_or_else(|_| "mcp".to_string());
            match spawn_mcp_transport(&cmd, &[]) {
                Ok(t) => clients.push(Arc::new(McpClient::new(name, Box::new(t)))),
                Err(e) => eprintln!("[ridgecode] MCP startup failed {cmd}: {e}"),
            }
        }
    }
    if clients.is_empty() {
        return McpTools::empty();
    }
    let n = clients.len();
    let tools = resolve_mcp(clients).await;
    eprintln!("[ridgecode] connected {n} MCP server(s)");
    tools
}

/// 加载 Skills:`RIDGE_SKILLS_DIR` env > config `skills_dir` > 默认 `~/.ridge/skills`。
fn load_configured_skills(cfg: &Config) -> Vec<Skill> {
    let dir = std::env::var("RIDGE_SKILLS_DIR")
        .ok()
        .or_else(|| cfg.skills_dir.clone())
        .unwrap_or_else(|| format!("{}/skills", ridge_home()));
    let mut skills = load_skills(&dir);
    skills.extend(agent::builtin_skills()); // 内置 skill:agent-creator / skill-creator
    let global_rules = std::path::PathBuf::from(format!("{}/AGENTS.md", ridge_home()));
    if let Some(rules) = agent::load_project_rules(Some(&global_rules)) {
        skills.push(rules); // ~/.ridge/AGENTS.md 全局规则 + cwd 的 CLAUDE.md / AGENTS.md 注入
    }
    if !skills.is_empty() {
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        eprintln!(
            "[ridgecode] loaded {} skill(s): {}",
            skills.len(),
            names.join(", ")
        );
    }
    skills
}

/// 加载自定义斜杠命令(iter-39):`RIDGE_COMMANDS_DIR` env > config `commands_dir` > `~/.ridge/commands`;
/// 目录里 `*.md` 各成 `/名字` + 每个 skill 也暴露为同名命令。供 TUI 斜杠命令扩展。
fn load_configured_commands(cfg: &Config, skills: &[Skill]) -> Vec<SlashCommand> {
    let dir = std::env::var("RIDGE_COMMANDS_DIR")
        .ok()
        .or_else(|| cfg.commands_dir.clone())
        .unwrap_or_else(|| format!("{}/commands", ridge_home()));
    let cmds = load_commands(&dir, skills);
    if !cmds.is_empty() {
        let names: Vec<String> = cmds.iter().map(|c| format!("/{}", c.name)).collect();
        eprintln!(
            "[ridgecode] loaded {} command(s): {}",
            cmds.len(),
            names.join(" ")
        );
    }
    cmds
}

/// 解析参数:非 flag 拼成任务(无 → TUI/headless);`--cwd <dir>` 切换工作目录;
/// `--yolo` / `--skip-permissions` / `--dangerously-skip-permissions` 或 env `RIDGE_SKIP_PERMISSIONS=1`
/// 开 skip-danger 模式(工具自动放行,不再 [y/N])。
fn parse_args() -> ParsedArgs {
    let mut task = String::new();
    let mut cwd = None;
    let mut resume = false;
    let mut skip_danger = std::env::var("RIDGE_SKIP_PERMISSIONS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let mut read_only = std::env::var("RIDGE_READ_ONLY")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let mut every = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--cwd" => cwd = args.next(),
            "--every" => every = args.next().as_deref().and_then(parse_duration),
            "-yolo" | "--yolo" | "--skip-permissions" | "--dangerously-skip-permissions" => {
                skip_danger = true
            }
            "--read-only" | "--readonly" => read_only = true,
            "--resume" | "--continue" => resume = true,
            _ => {
                if !task.is_empty() {
                    task.push(' ');
                }
                task.push_str(&a);
            }
        }
    }
    let task = if task.is_empty() { None } else { Some(task) };
    ParsedArgs {
        task,
        cwd,
        skip_danger,
        resume,
        read_only,
        every,
    }
}

/// 解析简单时长:`30s` / `5m` / `1h` / 裸数字当秒。坏/零 → `None`(用于 `--every` 时间触发器间隔)。
fn parse_duration(s: &str) -> Option<std::time::Duration> {
    let s = s.trim();
    let (num, mult) = if let Some(n) = s.strip_suffix('s') {
        (n, 1)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60)
    } else if let Some(n) = s.strip_suffix('h') {
        (n, 3600)
    } else {
        (s, 1)
    };
    let n: u64 = num.trim().parse().ok()?;
    (n > 0).then(|| std::time::Duration::from_secs(n * mult))
}

/// 命令行解析结果。
struct ParsedArgs {
    task: Option<String>,
    cwd: Option<String>,
    skip_danger: bool,
    resume: bool,
    read_only: bool,
    /// `--every <dur>`:设了 → 时间触发器,按此间隔重跑任务(仅一次性任务模式)。
    every: Option<std::time::Duration>,
}

/// 解析实际生效的 `(provider 类型, model, base_url)`:**env > config > 默认**。
/// provider 装配与 `/model` 命令共用,保证显示的就是真在用的。
fn configured_profile<'a>(cfg: &'a Config, selector: &str) -> Option<&'a agent::ProviderProfile> {
    let selector = selector.trim();
    if selector.is_empty() {
        return None;
    }
    cfg.providers
        .iter()
        .find(|profile| profile.name.eq_ignore_ascii_case(selector))
}

/// Resolve the human-facing provider/profile name without changing the runtime kind.
/// A Zai profile still uses the OpenAI-compatible wire kind, but the status bar must
/// say `Zai`; otherwise users see the transport implementation instead of their choice.
fn resolve_provider_label(cfg: &Config, provider: &str, base_url: &str) -> String {
    let same_endpoint = |left: &str, right: &str| {
        left.trim_end_matches('/')
            .eq_ignore_ascii_case(right.trim_end_matches('/'))
    };
    let selected = std::env::var("RIDGE_PROVIDER")
        .ok()
        .or_else(|| cfg.provider.clone());
    if let Some(profile) = selected
        .as_deref()
        .and_then(|selector| configured_profile(cfg, selector))
        .filter(|profile| profile.kind.eq_ignore_ascii_case(provider))
    {
        return profile.name.clone();
    }
    cfg.providers
        .iter()
        .find(|profile| {
            profile.kind.eq_ignore_ascii_case(provider)
                && same_endpoint(&profile.base_url, base_url)
        })
        .map(|profile| profile.name.clone())
        .unwrap_or_else(|| provider.to_string())
}

fn resolve_model_info(cfg: &Config) -> (String, String, String) {
    let selector = std::env::var("RIDGE_PROVIDER")
        .ok()
        .or_else(|| cfg.provider.clone())
        .unwrap_or_else(|| "openai".to_string());
    let model = std::env::var("RIDGE_MODEL").ok();
    let base = std::env::var("RIDGE_BASE_URL").ok();
    if let Some(profile) = configured_profile(cfg, &selector) {
        return (
            profile.kind.clone(),
            model.unwrap_or_else(|| profile.model.clone()),
            base.unwrap_or_else(|| profile.base_url.clone()),
        );
    }
    let model = model.or_else(|| cfg.model.clone());
    let base = base.or_else(|| cfg.base_url.clone());
    let (def_model, def_base) = if selector == "anthropic" {
        ("claude-sonnet-4-6", "https://api.anthropic.com/v1")
    } else {
        ("gpt-4o", "https://api.openai.com/v1")
    };
    (
        selector,
        model.unwrap_or_else(|| def_model.to_string()),
        base.unwrap_or_else(|| def_base.to_string()),
    )
}

fn resolve_configured_model_info(
    cfg: &Config,
    auth: &std::collections::BTreeMap<String, String>,
) -> (String, String, String) {
    let selector = std::env::var("RIDGE_PROVIDER")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| cfg.provider.clone());
    if let Some(selector) = selector.as_deref() {
        if configured_profile(cfg, selector).is_some() {
            return resolve_model_info(cfg);
        }
    }
    if agent::resolve_top_level_key(cfg, auth).is_none() {
        if let Some(profile) = cfg
            .providers
            .iter()
            .find(|profile| profile.resolve_key_with(auth).is_some())
        {
            return (
                profile.kind.clone(),
                cfg.model
                    .as_deref()
                    .filter(|model| !model.trim().is_empty())
                    .unwrap_or(&profile.model)
                    .to_string(),
                profile.base_url.clone(),
            );
        }
    }
    resolve_model_info(cfg)
}

fn resolve_start_model_info(
    cfg: &Config,
    auth: &std::collections::BTreeMap<String, String>,
    using_oauth: bool,
) -> (String, String, String) {
    if using_oauth {
        if let Some(info) = oauth_model_info(cfg) {
            return info;
        }
    }
    resolve_configured_model_info(cfg, auth)
}

/// 从零件造一个真实 provider(供启动装配与 `/model` 热切换共用)。
fn resolve_reasoning_effort(cfg: &Config) -> String {
    std::env::var("RIDGE_EFFORT")
        .ok()
        .or_else(|| std::env::var("RIDGE_REASONING_EFFORT").ok())
        .or_else(|| cfg.effort.clone())
        .and_then(|value| provider::normalize_reasoning_effort(&value).map(str::to_owned))
        .unwrap_or_else(|| provider::DEFAULT_REASONING_EFFORT.to_string())
}

fn make_provider(kind: &str, model: &str, base_url: &str, key: String) -> Arc<dyn LlmProvider> {
    match kind {
        "anthropic" => Arc::new(AnthropicProvider::new(base_url, model, key)),
        _ => Arc::new(OpenAiProvider::new(base_url, model, key)),
    }
}

/// 组装 sub-agent 注册表:**内置 agent**(fastcontext/explorer/reviewer)+ 用户 `agents` 目录
/// (同名覆盖内置)+ 命名 provider 档案(能从各自 KEY_ENV 取到密钥的那些,供 agent 的 `provider:` 引用)。
fn build_agents(cfg: &Config, auth: &std::collections::BTreeMap<String, String>) -> agent::Agents {
    let dir =
        std::env::var("RIDGE_AGENTS_DIR").unwrap_or_else(|_| format!("{}/agents", ridge_home()));
    let mut defs = agent::builtin_agents();
    for a in agent::load_agents(&dir) {
        match defs.iter_mut().find(|d| d.name == a.name) {
            Some(slot) => *slot = a, // 用户同名文件覆盖内置
            None => defs.push(a),
        }
    }
    let mut providers = std::collections::HashMap::new();
    let mut route_candidates = Vec::new();
    for p in &cfg.providers {
        if let Some(key) = p.resolve_key_with(auth) {
            let provider = make_provider(&p.kind, &p.model, &p.base_url, key);
            providers.insert(p.name.clone(), provider.clone());
            route_candidates.push(agent::AgentProvider {
                profile: p.route_model_profile(),
                provider,
            });
        }
    }
    if !defs.is_empty() {
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        eprintln!(
            "[ridgecode] loaded {} sub-agent(s): {}",
            defs.len(),
            names.join(", ")
        );
    }
    agent::Agents {
        defs,
        providers,
        route_candidates,
    }
}

/// 装配真实 provider。密钥来源(任一命中即用,否则 None → demo)。密钥绝不打印:
/// 1. **`RIDGE_API_KEY` env**(传统/最高优先)→ 配 env>config 解析出的 provider 身份;
/// 2. **config `providers[]` 档案**:取第一个能解析出密钥的档案(内联 `api_key` 或 `key_env`→env),
///    直接用它的 kind/model/base_url 启动 —— **config.json 即可跑,无需 `RIDGE_API_KEY`**。
fn real_provider(
    cfg: &Config,
    auth: &std::collections::BTreeMap<String, String>,
) -> Option<Arc<dyn LlmProvider>> {
    // A named profile is the active selection.  Resolve its credential and
    // endpoint before the legacy top-level key so switching profiles cannot
    // send one provider's key to another provider's endpoint.
    let selector = std::env::var("RIDGE_PROVIDER")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| cfg.provider.clone());
    if let Some(selector) = selector.as_deref() {
        if let Some(profile) = configured_profile(cfg, selector) {
            if profile.use_oauth == Some(true) {
                return None;
            }
            if let Some(key) = profile.resolve_key_with(auth) {
                eprintln!(
                    "[ridgecode] starting with config provider profile \"{}\" ({} · {})",
                    profile.name, profile.kind, profile.model
                );
                return Some(make_provider(
                    &profile.kind,
                    &profile.model,
                    &profile.base_url,
                    key,
                ));
            }
            return None;
        }
    }
    // 顶层 key(iter-41 收敛):RIDGE_API_KEY env → 顶层内联 api_key → 顶层 key_env→(env/auth)。
    // 命中即用顶层 provider/model/base_url 身份启动(用户设的默认 model 生效)。
    if let Some(key) = agent::resolve_top_level_key(cfg, auth) {
        let (kind, model, base) = resolve_model_info(cfg);
        return Some(make_provider(&kind, &model, &base, key));
    }
    for p in &cfg.providers {
        if let Some(key) = p.resolve_key_with(auth) {
            let model = cfg
                .model
                .as_deref()
                .filter(|model| !model.trim().is_empty())
                .unwrap_or(&p.model);
            eprintln!(
                "[ridgecode] starting with config provider profile \"{}\" ({} · {})",
                p.name, p.kind, model
            );
            return Some(make_provider(&p.kind, model, &p.base_url, key));
        }
    }
    None
}

/// 全链路可观测:`RUST_LOG=langgraph=debug,agent=debug ridge ...`。默认只报 warn。
fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 时间触发器间隔解析:s/m/h 后缀 + 裸数字当秒;坏/零 → None。
    #[test]
    fn parse_duration_units() {
        use std::time::Duration;
        assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("5m"), Some(Duration::from_secs(300)));
        assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_duration("90"), Some(Duration::from_secs(90)));
        assert_eq!(parse_duration("0s"), None, "零间隔无意义");
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration(""), None);
    }

    #[test]
    fn provider_label_keeps_named_profile_for_compatible_endpoint() {
        let cfg = Config::parse(
            r#"{
                "provider": "Zai",
                "providers": [{
                    "name": "Zai",
                    "kind": "openai",
                    "model": "glm-4.6",
                    "base_url": "https://open.bigmodel.cn/api/paas/v4"
                }]
            }"#,
        );
        assert_eq!(
            resolve_provider_label(&cfg, "openai", "https://open.bigmodel.cn/api/paas/v4/"),
            "Zai"
        );
    }

    /// 会话持久化:存 history → 读回内容一致(kill-9 后 --resume 的基础)。缺文件 → 空。
    #[test]
    fn session_roundtrips_history() {
        let p = std::env::temp_dir().join("ridge_session_test.json");
        let p = p.to_str().unwrap();
        let history = vec![Message::user("你好"), Message::assistant("在的")];
        save_session(p, &history);
        let loaded = load_session(p);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].content, "你好");
        assert_eq!(loaded[1].content, "在的");
        let _ = std::fs::remove_file(p);
        assert!(load_session("C:/no/such/ridge-session-xyz.json").is_empty());
    }

    #[test]
    fn prompt_history_persistence_filters_blanks_and_keeps_latest_bound() {
        let path =
            std::env::temp_dir().join(format!("ridge-input-history-{}.json", std::process::id()));
        let mut values = vec![" ".to_string()];
        values.extend((0..205).map(|index| format!("item-{index}")));
        save_prompt_history(path.to_str().unwrap(), &values);
        let loaded = load_prompt_history(path.to_str().unwrap());
        assert_eq!(loaded.len(), MAX_PROMPT_HISTORY);
        assert_eq!(loaded.first().map(String::as_str), Some("item-5"));
        assert_eq!(loaded.last().map(String::as_str), Some("item-204"));
        std::fs::write(&path, "not json").unwrap();
        assert!(load_prompt_history(path.to_str().unwrap()).is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn config_resolution_prefers_named_profile_and_defaults_are_stable() {
        let cfg = Config::parse(
            r#"{
                "provider": "Anthropic",
                "providers": [{
                    "name": "Anthropic",
                    "kind": "anthropic",
                    "model": "claude-test",
                    "base_url": "https://example.test/v1",
                    "api_key": "sk-test"
                }]
            }"#,
        );
        let auth = std::collections::BTreeMap::new();
        assert_eq!(
            configured_profile(&cfg, " anthropic ").unwrap().model,
            "claude-test"
        );
        assert_eq!(
            resolve_provider_label(&cfg, "anthropic", "https://other.test"),
            "Anthropic"
        );
        assert_eq!(
            resolve_model_info(&cfg),
            (
                "anthropic".into(),
                "claude-test".into(),
                "https://example.test/v1".into()
            )
        );
        assert_eq!(
            resolve_configured_model_info(&cfg, &auth),
            resolve_model_info(&cfg)
        );
        assert_eq!(
            resolve_start_model_info(&cfg, &auth, false),
            resolve_model_info(&cfg)
        );
        assert!(real_provider(&cfg, &auth).is_some());
        assert!(missing_key_provider()
            .complete(&provider::CompletionRequest::default())
            .await
            .is_ok());
    }

    #[test]
    fn proxy_application_and_history_path_helpers_are_stable() {
        let names = ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"];
        let old = names
            .iter()
            .map(|name| (*name, std::env::var_os(name)))
            .collect::<Vec<_>>();
        apply_proxy_env("  http://127.0.0.1:9  ");
        for name in names {
            assert_eq!(std::env::var(name).unwrap(), "http://127.0.0.1:9");
        }
        apply_proxy_env(" ");
        for (name, value) in old {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
        assert!(global_input_history_path().ends_with("input-history.json"));
        assert!(session_input_history_path().ends_with(".inputs.json"));
    }
}
