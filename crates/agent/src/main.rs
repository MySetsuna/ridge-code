use std::io::{IsTerminal, Write};
use std::sync::Arc;

use agent::{
    build_agent, build_llm_agent_full, compact_history, default_tool, load_skills, resolve_mcp,
    scripted, write_trace, AgentState, Approver, AutoApprove, Color, Config, McpTools, RichOutput,
    Skill,
};
use langgraph::{CompiledGraph, RunConfig, StreamEvent};
use mcp::{McpClient, StdioTransport};
use provider::{AnthropicProvider, LlmProvider, Message, OpenAiProvider};

/// ridgecode —— 通用 agent CLI(产品名 RidgeCode)。
///
/// 用法:
///   ridgecode                                # 交互式 REPL(有 key);/exit /reset /help
///   ridgecode "修复编译错误"                  # 一次性任务
///   ridgecode --cwd /path/to/project "..."    # 在目标项目里跑
///   ridgecode --yolo "..."                    # skip-danger:工具自动放行不问 [y/N]
///
/// 配置(环境变量):RIDGE_API_KEY / RIDGE_PROVIDER(anthropic|openai)/ RIDGE_MODEL / RIDGE_BASE_URL
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let (task, cwd, cli_skip_danger, resume) = parse_args();
    if let Some(dir) = &cwd {
        std::env::set_current_dir(dir)?;
    }
    let cfg = load_config(); // ~/.ridge/config.toml(env 仍覆盖)

    match real_provider(&cfg) {
        Some(p) => {
            let mcp = resolve_configured_mcp(&cfg).await; // config 多 server + 兼容旧 env 单 server
            let skills = load_configured_skills(&cfg); // 声明式技能(领域知识)
            let budget = cfg.budget_tokens.unwrap_or(0); // 0 = 不限
            let skip_danger = cli_skip_danger || cfg.skip_danger.unwrap_or(false);
            match task {
                Some(t) => run_once(p, mcp, skills, &t, budget).await, // 一次性
                None => {
                    // --resume:kill-9 / 关掉重开后恢复上一会话的多轮 history。
                    let initial = if resume {
                        load_session(&session_path())
                    } else {
                        Vec::new()
                    };
                    repl(p, mcp, skills, skip_danger, budget, initial).await // 交互式
                }
            }
        }
        None => {
            eprintln!(
                "[ridgecode] 未检测到 RIDGE_API_KEY,跑离线脚本 demo(设置密钥即用真实 LLM / REPL)。\n"
            );
            run_demo().await
        }
    }
}

/// 加载配置:`RIDGE_CONFIG` 指定路径,否则 `~/.ridge/config.toml`。读不到/坏 → 默认空配置(回落 env)。
fn load_config() -> Config {
    let path =
        std::env::var("RIDGE_CONFIG").unwrap_or_else(|_| format!("{}/config.toml", ridge_home()));
    let cfg = Config::load(&path);
    if cfg.provider.is_some() || !cfg.mcp.is_empty() {
        eprintln!("[ridgecode] 已加载 config {path}");
    }
    cfg
}

/// `~/.ridge` 目录(env 配置与 skills 的家)。
fn ridge_home() -> String {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default();
    format!("{home}/.ridge")
}

/// 会话持久化文件:`RIDGE_SESSION` 或 `~/.ridge/session.json`。存 REPL 的对话 history,
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

/// 接入 MCP 服务器:**config 里的多个 `[[mcp]]`** + 兼容旧的单个 env `RIDGE_MCP_CMD`。
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
    let skills = load_skills(&dir);
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

/// 解析参数:非 flag 拼成任务(无 → REPL);`--cwd <dir>` 切换工作目录;
/// `--yolo` / `--skip-permissions` / `--dangerously-skip-permissions` 或 env `RIDGE_SKIP_PERMISSIONS=1`
/// 开 skip-danger 模式(工具自动放行,不再 [y/N])。
fn parse_args() -> (Option<String>, Option<String>, bool, bool) {
    let mut task = String::new();
    let mut cwd = None;
    let mut resume = false;
    let mut skip_danger = std::env::var("RIDGE_SKIP_PERMISSIONS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--cwd" => cwd = args.next(),
            "--yolo" | "--skip-permissions" | "--dangerously-skip-permissions" => {
                skip_danger = true
            }
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
    (task, cwd, skip_danger, resume)
}

/// 装配真实 provider:**env > config > 默认**。没有 key(只从 env 读)就返回 None(走 demo)。密钥绝不打印。
fn real_provider(cfg: &Config) -> Option<Arc<dyn LlmProvider>> {
    let key = std::env::var("RIDGE_API_KEY")
        .ok()
        .filter(|k| !k.is_empty())?;
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
    match kind.as_str() {
        "anthropic" => {
            let base = base.unwrap_or_else(|| "https://api.anthropic.com/v1".to_string());
            let model = model.unwrap_or_else(|| "claude-sonnet-4-6".to_string());
            Some(Arc::new(AnthropicProvider::new(base, model, key)))
        }
        _ => {
            let base = base.unwrap_or_else(|| "https://api.openai.com/v1".to_string());
            let model = model.unwrap_or_else(|| "gpt-4o".to_string());
            Some(Arc::new(OpenAiProvider::new(base, model, key)))
        }
    }
}

/// 一次性任务:一律放行,跑完写 trace.json + 打印结果。
async fn run_once(
    provider: Arc<dyn LlmProvider>,
    mcp: McpTools,
    skills: Vec<Skill>,
    task: &str,
    budget: usize,
) -> anyhow::Result<()> {
    let app = build_llm_agent_full(provider, mcp, Arc::new(AutoApprove), skills)?;
    let out = run_streamed(&app, AgentState::new(task).with_budget(budget)).await?;
    trace_and_report(&out);
    Ok(())
}

/// 交互式 REPL:跨轮携带 history,有副作用的工具执行前 stdin 确认。`/exit` `/reset` `/compact` `/help`。
/// `skip_danger` = true 时用 [`AutoApprove`],工具自动放行、不再 [y/N](像 Claude 的 skip-permissions)。
async fn repl(
    provider: Arc<dyn LlmProvider>,
    mcp: McpTools,
    skills: Vec<Skill>,
    skip_danger: bool,
    budget: usize,
    mut history: Vec<Message>,
) -> anyhow::Result<()> {
    let title = RichOutput::new().with_color(Color::BrightCyan).bold();
    println!(
        "{}",
        title.format("RidgeCode —— 输入任务开跑;/help 看命令,/exit 退出。")
    );
    if !history.is_empty() {
        println!(
            "{}",
            RichOutput::new()
                .with_color(Color::Green)
                .format(&format!("(已恢复上次会话:{} 条消息)", history.len()))
        );
    }
    let approver: Arc<dyn Approver> = if skip_danger {
        println!(
            "{}",
            RichOutput::new()
                .with_color(Color::BrightRed)
                .bold()
                .format("⚠ skip-danger 模式:工具自动执行,不再询问 [y/N](灾难命令仍被硬拦截)。")
        );
        Arc::new(AutoApprove)
    } else {
        println!("危险操作会先问你 [y/N]。");
        Arc::new(StdinApprover)
    };
    println!();
    let app = build_llm_agent_full(provider, mcp, approver, skills)?;

    loop {
        print!("ridgecode> ");
        std::io::stdout().flush().ok();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            break; // EOF (Ctrl-D)
        }
        let input = line.trim();
        match input {
            "" => continue,
            "/exit" | "/quit" => break,
            "/help" => {
                println!("命令:/exit 退出 · /reset 清空上下文 · /compact 压缩上下文 · /help 本帮助\n直接输入自然语言即为任务。");
                continue;
            }
            "/reset" => {
                history.clear();
                save_session(&session_path(), &history); // 清空也落盘,--resume 不再带回旧会话
                println!("(上下文已清空)");
                continue;
            }
            "/compact" => {
                let before = history.len();
                history = compact_history(history, 4);
                println!("(上下文已压缩:{before} → {} 条)", history.len());
                continue;
            }
            _ => {}
        }

        // 带上历史续跑;跑完把更新后的 history 存回,实现多轮。
        history.push(Message::user(input));
        let state = AgentState::new(input)
            .with_history(history.clone())
            .with_budget(budget);
        match run_streamed(&app, state).await {
            Ok(out) => {
                history = out.history.clone();
                save_session(&session_path(), &history); // 每轮落盘 → kill-9 后 --resume 可恢复
                trace_and_report(&out);
            }
            Err(e) => eprintln!("[ridgecode] 出错:{e}"),
        }
    }
    println!("bye.");
    Ok(())
}

/// 写审计轨迹 trace.json(best-effort)+ 打印结果。
fn trace_and_report(out: &AgentState) {
    match write_trace(out, "trace.json") {
        Ok(()) => eprintln!("[ridgecode] 审计轨迹已写 trace.json"),
        Err(e) => eprintln!("[ridgecode] 写 trace.json 失败: {e}"),
    }
    print_report(out);
}

/// 跑 agent 并**实时把内容渲染到终端**:等待时转 spinner,每个超步一合并就把新产生的
/// 推理 / 工具调用 / 结果 / 校验**彩色打出来** —— 让用户直接在 shell 里看到输出,而非去翻 trace.json。
/// 非 TTY(管道/重定向)时不转 spinner,只顺序输出内容。
async fn run_streamed(
    app: &CompiledGraph<AgentState>,
    state: AgentState,
) -> anyhow::Result<AgentState> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<StreamEvent<AgentState>>();
    let tty = std::io::stderr().is_terminal();
    let printer = tokio::spawn(async move {
        const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let spin = RichOutput::new().with_color(Color::BrightBlue);
        let dim = RichOutput::new().with_color(Color::Cyan);
        let mut frame = 0usize;
        let mut printed = 0usize; // 已打印到第几条 message
        let mut status = String::from("推理中");
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(90));
        loop {
            tokio::select! {
                _ = ticker.tick(), if tty => {
                    frame = (frame + 1) % FRAMES.len();
                    eprint!("\r\x1b[K{} {}", spin.format(FRAMES[frame]), dim.format(&status));
                    std::io::stderr().flush().ok();
                }
                ev = rx.recv() => match ev {
                    Some(StreamEvent::NodeFinished { node, .. }) => status = node_label(&node),
                    Some(StreamEvent::Superstep { state, .. }) => {
                        for m in state.messages.iter().skip(printed) {
                            if tty {
                                eprint!("\r\x1b[K"); // 清掉 spinner 行再打内容
                            }
                            eprintln!("{}", format_event(m));
                        }
                        printed = state.messages.len();
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

/// stdin 权限门:有副作用的工具执行前问 [y/N]。
struct StdinApprover;
impl Approver for StdinApprover {
    fn approve(&self, action: &str, detail: &str) -> bool {
        eprint!("\n  ⚠ 允许 {action} {detail} ? [y/N] ");
        std::io::stderr().flush().ok();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return false;
        }
        matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
    }
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
