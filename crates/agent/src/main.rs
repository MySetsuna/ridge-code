use std::io::{IsTerminal, Write};
use std::sync::Arc;

use agent::{
    build_agent, build_llm_agent_full, builtin_tool_specs, compact_history, default_tool,
    expand_mentions, halt_reason, load_signal_block, load_skills, null_token_bus, render_todos,
    resolve_mcp, scripted, write_run, AgentState, Approver, AutoApprove, Color, Config, McpTools,
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
             ridgecode                      交互式 TUI(需 RIDGE_API_KEY;非 TTY 则 headless 逐行任务)\n  \
             ridgecode \"任务\"               一次性任务\n  \
             ridgecode --resume             恢复上次会话(kill-9/关掉重开后续接)\n\n\
             选项:\n  \
             --cwd <dir>                    在目标项目目录里跑\n  \
             --yolo/--skip-permissions      skip-danger:工具自动放行不问 [y/N](灾难命令仍拦)\n  \
             --read-only                    只读模式:只 offer 读/查/研究工具,拒一切写/shell 副作用\n  \
             --resume/--continue            恢复上次会话\n  \
             -h/--help、-V/--version        本帮助 / 版本\n\n\
             TUI 内:斜杠命令 /model /provider /config /agent /compact 等;@path 引用文件、Ctrl-C 中断。\
             管道/非 TTY:逐行 stdin 当任务(headless,无斜杠命令)。\n\n\
             配置:~/.ridge/config.json(provider/model/预算/多 mcp/skills;env 覆盖);\
             TUI 内 /config set <key> <value> 可持久化。密钥只走 RIDGE_API_KEY env。\
             ~/.ridge/skills/*/SKILL.md 加领域技能不改源码。"
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
            let agents = Arc::new(build_agents(&cfg)); // sub-agent 注册表(内置 + 用户 + 命名 provider)
            match task {
                Some(t) => run_once(p, mcp, skills, &t, budget, agents, read_only).await, // 一次性
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
                "[ridgecode] 未检测到 RIDGE_API_KEY,跑离线脚本 demo(设置密钥即用真实 LLM / TUI)。\n"
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
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--cwd" => cwd = args.next(),
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
    }
}

/// 命令行解析结果。
struct ParsedArgs {
    task: Option<String>,
    cwd: Option<String>,
    skip_danger: bool,
    resume: bool,
    read_only: bool,
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
        if let Some(key) = std::env::var(&p.key_env).ok().filter(|k| !k.is_empty()) {
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

/// 装配真实 provider:没有 key(只从 env 读)就返回 None(走 demo)。密钥绝不打印。
fn real_provider(cfg: &Config) -> Option<Arc<dyn LlmProvider>> {
    let key = std::env::var("RIDGE_API_KEY")
        .ok()
        .filter(|k| !k.is_empty())?;
    let (kind, model, base) = resolve_model_info(cfg);
    Some(make_provider(&kind, &model, &base, key))
}

/// 一次性任务:一律放行,跑完写 trace.json + 打印结果。
async fn run_once(
    provider: Arc<dyn LlmProvider>,
    mcp: McpTools,
    skills: Vec<Skill>,
    task: &str,
    budget: usize,
    agents: Arc<agent::Agents>,
    read_only: bool,
) -> anyhow::Result<()> {
    let bus = null_token_bus();
    let app = build_llm_agent_full(
        provider,
        mcp,
        Arc::new(AutoApprove),
        skills,
        bus.clone(),
        agents,
        read_only,
    )?;
    // `@path` 引用 → 注入文件正文(一次性任务也支持)。继承上个会话的未决信号(信号复利)。
    let state = AgentState::new(expand_mentions(task))
        .with_budget(budget)
        .with_signals(load_signal_block());
    let out = run_streamed(&app, state, &bus).await?;
    trace_and_report(&out);
    Ok(())
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
                trace_and_report(&out);
            }
            Err(e) => eprintln!("[ridgecode] 出错:{e}"),
        }
    }
    Ok(())
}

/// 把一轮落成标准存储库的一条 run(`.ridge/runs/<id>/` 含 manifest.json + trace.json,best-effort)
/// + 打印结果 + 播报停机原因。每 run 独立目录,审计历史不再互相覆盖。
fn trace_and_report(out: &AgentState) {
    let run_dir = run_artifacts_dir();
    match write_run(out, &run_dir) {
        Ok(()) => eprintln!("[ridgecode] 运行留痕已写 {}", run_dir.display()),
        Err(e) => eprintln!("[ridgecode] 写运行留痕失败: {e}"),
    }
    let reason = halt_reason(out);
    if !reason.is_success() {
        // 响亮失败:护栏熔断/未验证时明确播报,别让「悄悄停」被当成成功(loop engineering:fail loudly)。
        eprintln!("[ridgecode] 停机原因:{}(未通过确定性验证)", reason.as_str());
    }
    print_report(out);
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
