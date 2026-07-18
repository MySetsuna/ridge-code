use std::io::{IsTerminal, Write};
use std::sync::Arc;

use agent::{
    auto_signal_from_run, build_agent, build_llm_agent_full, builtin_tool_specs, compact_history,
    default_tool, est_tokens, expand_mentions, extract_signals_from_run, halt_reason,
    load_signal_block, load_skills, null_token_bus, render_todos, resolve_mcp, scripted,
    signal_extract_enabled, write_run, AgentState, Approver, AutoApprove, Color, Config, McpTools,
    RichOutput, Skill, Todo, TokenBus,
};
use langgraph::{CompiledGraph, RunConfig, StreamEvent};
use mcp::{McpClient, StdioTransport};
use provider::{AnthropicProvider, LlmProvider, Message, OpenAiProvider, SwapProvider};

mod tui;

/// TUI 展示用元信息(`/tools` `/model` 命令用)。
struct ReplMeta {
    tools: Vec<String>,
    provider: String,
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
/// 配置(环境变量):RIDGE_API_KEY / RIDGE_PROVIDER(anthropic|openai)/ RIDGE_MODEL / RIDGE_BASE_URL
/// `--help` / `--version` 帮助与版本(1.0 级 CLI 该有的),命中就打印并返回 true(不进主流程)。
fn handle_meta_flags() -> bool {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("ridgecode {}", env!("CARGO_PKG_VERSION"));
        return true;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "RidgeCode —— 模块化通用 agent CLI(二进制 ridgecode)\n\n\
             用法:\n  \
             ridgecode                      交互式 TUI(需密钥:RIDGE_API_KEY 或 config 里 api_key/key_env;非 TTY 则 headless)\n  \
             ridgecode \"任务\"               一次性任务\n  \
             ridgecode --resume             恢复上次会话(kill-9/关掉重开后续接)\n\n\
             选项:\n  \
             --cwd <dir>                    在目标项目目录里跑\n  \
             --every <30s|5m|1h>            时间触发器:按间隔重跑该任务(常驻;每轮重载信号复利,Ctrl-C 止)\n  \
             --yolo/--skip-permissions      skip-danger:工具自动放行不问 [y/N](灾难命令仍拦)\n  \
             --read-only                    只读模式:只 offer 读/查/研究工具,拒一切写/shell 副作用\n  \
             --resume/--continue            恢复上次会话\n  \
             -h/--help、-V/--version        本帮助 / 版本\n\n\
             TUI 内:斜杠命令 /model /provider /config /agent /compact 等;@path 引用文件、Ctrl-C 中断。\
             管道/非 TTY:逐行 stdin 当任务(headless,无斜杠命令)。\n\n\
             配置:~/.ridge/config.json(provider/model/预算/多 mcp/skills;env 覆盖);\
             TUI 内 /config set <key> <value> 可持久化。密钥:RIDGE_API_KEY env,或 config 档案的 api_key(明文)/key_env(环境变量名)。\
             ~/.ridge/skills/*/SKILL.md 加领域技能不改源码。\n  \
             RIDGE_EXTRACT_SIGNALS=1        opt-in:run 收尾用一次 LLM 把轨迹提炼成复利信号(默认关,省 token)。"
        );
        return true;
    }
    false
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if handle_meta_flags() {
        return Ok(());
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

    match real_provider(&cfg) {
        Some(p) => {
            let mcp = resolve_configured_mcp(&cfg).await; // config 多 server + 兼容旧 env 单 server
            let skills = load_configured_skills(&cfg); // 声明式技能(领域知识)
            let budget = cfg.budget_tokens.unwrap_or(0); // 0 = 不限
            let skip_danger = cli_skip_danger || cfg.skip_danger.unwrap_or(false);
            // 地址越狱(iter-34):启动从 config 置进程级开关,默认关(TUI 可 /jailbreak 实时切)。
            agent::set_allow_jailbreak(cfg.allow_jailbreak.unwrap_or(false));
            let agents = Arc::new(build_agents(&cfg)); // sub-agent 注册表(内置 + 用户 + 命名 provider)
            match task {
                Some(t) => run_once(p, mcp, skills, &t, budget, agents, read_only, every).await, // 一次性 / --every 触发器
                None => {
                    // --resume:kill-9 / 关掉重开后恢复上一会话的多轮 history。
                    let initial = if resume {
                        load_session(&session_path())
                    } else {
                        Vec::new()
                    };
                    let (provider_kind, model, base_url) = resolve_model_info(&cfg);
                    let mut tools: Vec<String> = builtin_tool_specs()
                        .iter()
                        .map(|s| s.name.clone())
                        .collect();
                    tools.extend(mcp.tool_names()); // 读工具名(在 mcp 被移入交互循环前)
                    let meta = ReplMeta {
                        tools,
                        provider: provider_kind,
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
                    let swap = Arc::new(SwapProvider::new(p));
                    // 终端采用 TUI；管道/非 TTY 退回 headless，避免破坏脚本/重定向调用。
                    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
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
            eprintln!(
                "[ridgecode] 未取到密钥,跑离线脚本 demo。给密钥即用真实 LLM / TUI,任选一:\n  \
                 · 设 RIDGE_API_KEY 环境变量;或\n  \
                 · 在 ~/.ridge/config.json 的某个 providers 档案里填 \"api_key\"(明文,自担风险),\n    \
                 或把 \"key_env\" 指向一个已 export 的环境变量名。见同目录 config.example.json。\n"
            );
            run_demo().await
        }
    }
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
        eprintln!("[ridgecode] 已加载 config {path}");
    }
    cfg
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

/// 接入 MCP 服务器:**config 里的多个 `mcp`** + 兼容旧的单个 env `RIDGE_MCP_CMD`。
/// 降级不崩:单个起不来 → 跳过;都没有 → 空,agent 只用内置工具。
async fn resolve_configured_mcp(cfg: &Config) -> McpTools {
    let mut clients = Vec::new();
    // config 声明的多 server。
    for m in &cfg.mcp {
        match StdioTransport::spawn(&m.cmd, &m.args) {
            Ok(t) => clients.push(Arc::new(McpClient::new(m.name.clone(), Box::new(t)))),
            Err(e) => eprintln!("[ridgecode] MCP 启动失败 {} ({}): {e}", m.name, m.cmd),
        }
    }
    // 兼容旧 env 单 server。
    if let Ok(cmd) = std::env::var("RIDGE_MCP_CMD") {
        if !cmd.is_empty() {
            let name = std::env::var("RIDGE_MCP_NAME").unwrap_or_else(|_| "mcp".to_string());
            match StdioTransport::spawn(&cmd, &[]) {
                Ok(t) => clients.push(Arc::new(McpClient::new(name, Box::new(t)))),
                Err(e) => eprintln!("[ridgecode] MCP 启动失败 {cmd}: {e}"),
            }
        }
    }
    if clients.is_empty() {
        return McpTools::empty();
    }
    let n = clients.len();
    let tools = resolve_mcp(clients).await;
    eprintln!("[ridgecode] 已接入 {n} 个 MCP server");
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
    if let Some(rules) = agent::load_project_rules() {
        skills.push(rules); // cwd 的 CLAUDE.md / AGENTS.md 作为项目规则注入
    }
    if !skills.is_empty() {
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        eprintln!(
            "[ridgecode] 已加载 {} 个 skill:{}",
            skills.len(),
            names.join(", ")
        );
    }
    skills
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
            "--yolo" | "--skip-permissions" | "--dangerously-skip-permissions" => {
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
fn resolve_model_info(cfg: &Config) -> (String, String, String) {
    let kind = std::env::var("RIDGE_PROVIDER")
        .ok()
        .or_else(|| cfg.provider.clone())
        .unwrap_or_else(|| "openai".to_string());
    let model = std::env::var("RIDGE_MODEL")
        .ok()
        .or_else(|| cfg.model.clone());
    let base = std::env::var("RIDGE_BASE_URL")
        .ok()
        .or_else(|| cfg.base_url.clone());
    let (def_model, def_base) = if kind == "anthropic" {
        ("claude-sonnet-4-6", "https://api.anthropic.com/v1")
    } else {
        ("gpt-4o", "https://api.openai.com/v1")
    };
    (
        kind,
        model.unwrap_or_else(|| def_model.to_string()),
        base.unwrap_or_else(|| def_base.to_string()),
    )
}

/// 从零件造一个真实 provider(供启动装配与 `/model` 热切换共用)。
fn make_provider(kind: &str, model: &str, base_url: &str, key: String) -> Arc<dyn LlmProvider> {
    match kind {
        "anthropic" => Arc::new(AnthropicProvider::new(base_url, model, key)),
        _ => Arc::new(OpenAiProvider::new(base_url, model, key)),
    }
}

/// 组装 sub-agent 注册表:**内置 agent**(fastcontext/explorer/reviewer)+ 用户 `agents` 目录
/// (同名覆盖内置)+ 命名 provider 档案(能从各自 KEY_ENV 取到密钥的那些,供 agent 的 `provider:` 引用)。
fn build_agents(cfg: &Config) -> agent::Agents {
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
    for p in &cfg.providers {
        if let Some(key) = p.resolve_key() {
            providers.insert(
                p.name.clone(),
                make_provider(&p.kind, &p.model, &p.base_url, key),
            );
        }
    }
    if !defs.is_empty() {
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        eprintln!(
            "[ridgecode] 已加载 {} 个 sub-agent:{}",
            defs.len(),
            names.join(", ")
        );
    }
    agent::Agents { defs, providers }
}

/// 装配真实 provider。密钥来源(任一命中即用,否则 None → demo)。密钥绝不打印:
/// 1. **`RIDGE_API_KEY` env**(传统/最高优先)→ 配 env>config 解析出的 provider 身份;
/// 2. **config `providers[]` 档案**:取第一个能解析出密钥的档案(内联 `api_key` 或 `key_env`→env),
///    直接用它的 kind/model/base_url 启动 —— **config.json 即可跑,无需 `RIDGE_API_KEY`**。
fn real_provider(cfg: &Config) -> Option<Arc<dyn LlmProvider>> {
    if let Some(key) = std::env::var("RIDGE_API_KEY")
        .ok()
        .filter(|k| !k.is_empty())
    {
        let (kind, model, base) = resolve_model_info(cfg);
        return Some(make_provider(&kind, &model, &base, key));
    }
    // 顶层内联 api_key:用顶层 provider/model/base_url 身份启动(用户设的默认 model 生效)。
    if let Some(key) = cfg
        .api_key
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        let (kind, model, base) = resolve_model_info(cfg);
        return Some(make_provider(&kind, &model, &base, key));
    }
    for p in &cfg.providers {
        if let Some(key) = p.resolve_key() {
            eprintln!(
                "[ridgecode] 用 config provider 档案「{}」启动({} · {})",
                p.name, p.kind, p.model
            );
            return Some(make_provider(&p.kind, &p.model, &p.base_url, key));
        }
    }
    None
}

/// 一次性任务:一律放行,跑完写 run 留痕 + 打印结果。
/// `every=Some(dur)`:**时间触发器**(rung-3 延迟阶梯)—— app 只建一次,按间隔重跑同一任务,
/// 每轮重载 `.ridge/signals`(信号复利)、失败自动落信号,直到 Ctrl-C。是「常驻助手」的最小形态。
#[allow(clippy::too_many_arguments)]
async fn run_once(
    provider: Arc<dyn LlmProvider>,
    mcp: McpTools,
    skills: Vec<Skill>,
    task: &str,
    budget: usize,
    agents: Arc<agent::Agents>,
    read_only: bool,
    every: Option<std::time::Duration>,
) -> anyhow::Result<()> {
    let bus = null_token_bus();
    // opt-in 自动 signal 抽取器:建 app 前留一把 provider(Arc 克隆廉价),供 run 收尾提炼复利信号。
    let extractor = signal_extract_enabled().then(|| provider.clone());
    let app = build_llm_agent_full(
        provider,
        mcp,
        Arc::new(AutoApprove),
        skills,
        bus.clone(),
        agents,
        read_only,
    )?;
    if let Some(dur) = every {
        eprintln!(
            "[ridgecode] 时间触发器:每 {}s 跑一次「{task}」(Ctrl-C 止;每轮重载信号复利)",
            dur.as_secs()
        );
    }
    loop {
        // `@path` 引用 → 注入文件正文。继承上个会话的未决信号(信号复利);触发器模式每轮都重载。
        let state = AgentState::new(expand_mentions(task))
            .with_budget(budget)
            .with_signals(load_signal_block());
        match run_streamed(&app, state, &bus).await {
            Ok(out) => {
                let source = trace_and_report(&out);
                maybe_extract_signals(extractor.as_ref(), &out, &source).await;
            }
            // 触发器(常驻)模式下单轮出错不该掀翻整个循环;一次性模式仍向上抛(非零退出)。
            Err(e) if every.is_some() => eprintln!("[ridgecode] 本轮出错:{e}"),
            Err(e) => return Err(e),
        }
        match every {
            Some(dur) => tokio::time::sleep(dur).await,
            None => return Ok(()),
        }
    }
}

/// 非 TTY(管道/CI/重定向):无 TUI、无斜杠命令。逐行读 stdin,每行当一个任务串行跑,跨行携带 history。
/// 非交互无法 [y/N] 确认,故一律 [`AutoApprove`](灾难命令仍被 `is_dangerous_command` 硬拦截)。
/// ponytail: headless 恒自动放行;要严格权限门请用 TTY 交互(TUI)。
async fn headless(
    provider: Arc<dyn LlmProvider>,
    mcp: McpTools,
    skills: Vec<Skill>,
    budget: usize,
    mut history: Vec<Message>,
    agents: Arc<agent::Agents>,
    read_only: bool,
) -> anyhow::Result<()> {
    let bus = null_token_bus();
    let extractor = signal_extract_enabled().then(|| provider.clone());
    let app = build_llm_agent_full(
        provider,
        mcp,
        Arc::new(AutoApprove),
        skills,
        bus.clone(),
        agents,
        read_only,
    )?;
    for line in std::io::stdin().lines() {
        let line = line?; // 读到 EOF 迭代自然结束;IO 错误照旧上抛
        let input = line.trim();
        if input.is_empty() {
            continue;
        }
        // `@path` 引用 → 注入文件正文;跨行携带 history 实现多轮。
        history.push(Message::user(expand_mentions(input)));
        let state = AgentState::new(input)
            .with_history(history.clone())
            .with_budget(budget)
            .with_signals(load_signal_block());
        match run_streamed(&app, state, &bus).await {
            Ok(out) => {
                history = out.history.clone();
                save_session(&session_path(), &history); // 每轮落盘 → --resume 可恢复
                let source = trace_and_report(&out);
                maybe_extract_signals(extractor.as_ref(), &out, &source).await;
            }
            Err(e) => eprintln!("[ridgecode] 出错:{e}"),
        }
    }
    Ok(())
}

/// 把一轮落成标准存储库的一条 run(`.ridge/runs/<id>/` 含 manifest.json + trace.json,best-effort),
/// 打印结果、播报停机原因。每 run 独立目录,审计历史不再互相覆盖。
/// 返回本 run 的 source id(run 目录名),供自动 signal 抽取器复用同一溯源标签。
fn trace_and_report(out: &AgentState) -> String {
    let run_dir = run_artifacts_dir();
    match write_run(out, &run_dir) {
        Ok(()) => eprintln!("[ridgecode] 运行留痕已写 {}", run_dir.display()),
        Err(e) => eprintln!("[ridgecode] 写运行留痕失败: {e}"),
    }
    let source = run_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("run")
        .to_string();
    let reason = halt_reason(out);
    if !reason.is_success() {
        // 响亮失败:护栏熔断/未验证时明确播报,别让「悄悄停」被当成成功(loop engineering:fail loudly)。
        eprintln!("[ridgecode] 停机原因:{}(未通过确定性验证)", reason.as_str());
        // 自动产者:失败落 failure 信号(preserve mistakes),下个会话/下一轮触发自动继承。source=本 run id。
        if let Some(id) = auto_signal_from_run(out, agent::SIGNALS_DIR, &source) {
            eprintln!("[ridgecode] 已记失败信号 {id}(下个会话将继承)");
        }
    }
    print_report(out);
    source
}

/// 自动 signal 抽取器(opt-in,复利环产者的「发现/待办」侧):run 收尾用 provider 一次性把轨迹
/// 提炼成可复用信号,喂 `.ridge/signals`。best-effort —— 失败/无所得静默,绝不掀翻主流程。
async fn maybe_extract_signals(
    extractor: Option<&Arc<dyn LlmProvider>>,
    out: &AgentState,
    source: &str,
) {
    let Some(p) = extractor else { return };
    let ids = extract_signals_from_run(p.as_ref(), out, agent::SIGNALS_DIR, source).await;
    if !ids.is_empty() {
        eprintln!(
            "[ridgecode] 抽取 {} 条复利信号(下个会话将继承):{}",
            ids.len(),
            ids.join(", ")
        );
    }
}

/// 本次 run 的留痕目录:`.ridge/runs/<纳秒时间戳>`(cwd 本地,像 `.git` 随项目走)。
/// ponytail: 纳秒时间戳做 id,顺序 CLI 调用间实际不会撞;要严格唯一再引 uuid。
fn run_artifacts_dir() -> std::path::PathBuf {
    let id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::path::Path::new(".ridge")
        .join("runs")
        .join(id.to_string())
}

/// 跑 agent 并**实时把内容渲染到终端**:等待时转 spinner,每个超步一合并就把新产生的
/// 推理 / 工具调用 / 结果 / 校验**彩色打出来** —— 让用户直接在 shell 里看到输出,而非去翻 trace.json。
/// 非 TTY(管道/重定向)时不转 spinner,只顺序输出内容。
async fn run_streamed(
    app: &CompiledGraph<AgentState>,
    state: AgentState,
    token_bus: &TokenBus,
) -> anyhow::Result<AgentState> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<StreamEvent<AgentState>>();
    // 逐字流式:注册一个 token sender 到总线;reason 节点边收边发,printer 边收边显。
    let (ttx, mut trx) = tokio::sync::mpsc::unbounded_channel::<String>();
    *token_bus.lock().unwrap() = Some(ttx);
    let tty = std::io::stderr().is_terminal();
    let printer = tokio::spawn(async move {
        const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let spin = RichOutput::new().with_color(Color::BrightBlue);
        let dim = RichOutput::new().with_color(Color::Cyan);
        let answer = RichOutput::new().with_color(Color::BrightWhite).bold();
        let mut frame = 0usize;
        let mut printed = 0usize; // 已打印到第几条 message
        let mut status = String::from("推理中");
        let mut streaming = false; // 本超步是否正在逐字流式(流式期间不转 spinner、末尾不重复打)
        let mut last_todos: Vec<Todo> = Vec::new(); // 任务清单变了才重渲染
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(90));
        loop {
            tokio::select! {
                _ = ticker.tick(), if tty && !streaming => {
                    frame = (frame + 1) % FRAMES.len();
                    eprint!("\r\x1b[K{} {}", spin.format(FRAMES[frame]), dim.format(&status));
                    std::io::stderr().flush().ok();
                }
                Some(tok) = trx.recv() => {
                    if !streaming {
                        if tty { eprint!("\r\x1b[K"); } // 清 spinner,起头
                        eprint!("{}", answer.format("🤖 "));
                        streaming = true;
                    }
                    eprint!("{}", answer.format(&tok)); // 逐字追加(加粗白)
                    std::io::stderr().flush().ok();
                }
                ev = rx.recv() => match ev {
                    Some(StreamEvent::NodeFinished { node, .. }) => status = node_label(&node),
                    Some(StreamEvent::Superstep { state, .. }) => {
                        for m in state.messages.iter().skip(printed) {
                            // 若本超步已逐字流式打过最终答案,则不再重复整段打,只补个换行收尾。
                            if streaming && m.contains("(final) ") {
                                eprintln!();
                            } else {
                                if tty { eprint!("\r\x1b[K"); }
                                eprintln!("{}", format_event(m));
                            }
                        }
                        printed = state.messages.len();
                        streaming = false; // 超步收尾 → 下个超步 spinner 恢复
                        // 任务清单有变化 → 渲染 [x]/[~]/[ ] 给用户看进度。
                        if state.todos != last_todos {
                            if !state.todos.is_empty() {
                                eprintln!("{}", render_todos(&state.todos));
                            }
                            last_todos = state.todos.clone();
                        }
                    }
                    None => break,
                }
            }
        }
        if tty {
            eprint!("\r\x1b[K");
            std::io::stderr().flush().ok();
        }
    });

    let out = app
        .invoke_with(state, &RunConfig::default(), None, Some(&tx))
        .await?;
    drop(tx);
    *token_bus.lock().unwrap() = None; // 关闭 token 通道 → printer 收尾
    let _ = printer.await;
    Ok(out)
}

/// spinner 旁边显示的当前阶段。
fn node_label(node: &str) -> String {
    match node {
        "reason" => "推理中",
        "act" => "执行工具",
        "verify" => "校验中",
        other => other,
    }
    .to_string()
}

/// 把一条内部事件 message 渲染成彩色终端行(按前缀分类上色)。
fn format_event(m: &str) -> String {
    let ro = |c: Color| RichOutput::new().with_color(c);
    if let Some((_, ans)) = m.split_once("(final) ") {
        // 模型的最终回答 —— 高亮加粗,最显眼。
        return ro(Color::BrightWhite)
            .bold()
            .format(&format!("\n🤖 {ans}\n"));
    }
    if m.starts_with("reason#") {
        // 推理 / 发起工具调用 —— 暗色旁白。
        let body = m.split_once(": ").map_or(m, |x| x.1);
        return ro(Color::Cyan).format(&format!("  ⋯ {body}"));
    }
    if let Some(rest) = m.strip_prefix("act: ") {
        return ro(Color::Yellow).format(&format!("  ▸ {}", truncate(rest, 500)));
    }
    if m.starts_with("verify: PASS") {
        return ro(Color::Green).bold().format(&format!("  ✓ {m}"));
    }
    if m.starts_with("verify: FAIL") {
        return ro(Color::Red).format(&format!("  ✗ {m}"));
    }
    ro(Color::White).format(m)
}

/// 按字符截断长文本(工具输出可能很长,别刷屏)。
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max).collect();
        format!("{head}… (截断)")
    }
}

/// 离线 demo:脚本大脑 + 假工具,零联网跑通闭环。
async fn run_demo() -> anyhow::Result<()> {
    let app = build_agent(scripted(), default_tool())?;
    let out = app
        .invoke(AgentState::new("make the test suite pass"))
        .await?;
    if let Some(last) = out.messages.last() {
        println!("\n{last}");
    }
    print_report(&out);
    Ok(())
}

fn print_report(out: &AgentState) {
    let status = if out.approved {
        RichOutput::new()
            .with_color(Color::Green)
            .bold()
            .format("✓ approved")
    } else {
        RichOutput::new()
            .with_color(Color::Red)
            .bold()
            .format("✗ not approved")
    };
    let stats = RichOutput::new()
        .with_color(Color::Cyan)
        .format(&format!("steps={} tokens={}", out.steps, out.total_tokens));
    println!("\n{status}  {stats}");
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

    #[test]
    fn format_event_colorizes_by_kind() {
        let fin = format_event("reason#2: (final) 你好世界");
        assert!(fin.contains("你好世界") && fin.contains("🤖") && fin.contains("\x1b[0m"));
        assert!(format_event("act: web_search -> ok").contains("\x1b[33m")); // 黄
        assert!(format_event("verify: PASS (deterministic gate)").contains("\x1b[32m"));
        // 绿
    }

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
    fn truncate_caps_long_text() {
        assert_eq!(truncate("abc", 10), "abc");
        assert!(truncate(&"x".repeat(50), 10).ends_with("… (截断)"));
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
}
