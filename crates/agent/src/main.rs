use std::io::Write;
use std::sync::Arc;

use agent::{
    build_agent, build_llm_agent_gated, compact_history, default_tool, resolve_mcp, scripted,
    write_trace, AgentState, Approver, AutoApprove, McpTools,
};
use langgraph::{CompiledGraph, RunConfig, StreamEvent};
use mcp::{McpClient, StdioTransport};
use provider::{AnthropicProvider, LlmProvider, Message, OpenAiProvider};

/// ridge —— 编码 agent CLI。
///
/// 用法:
///   ridge                                # 交互式 REPL(有 key);/exit /reset /help
///   ridge "修复编译错误"                  # 一次性任务
///   ridge --cwd /path/to/project "..."    # 在目标项目里跑
///
/// 配置(环境变量):RIDGE_API_KEY / RIDGE_PROVIDER(anthropic|openai)/ RIDGE_MODEL / RIDGE_BASE_URL
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let (task, cwd) = parse_args();
    if let Some(dir) = &cwd {
        std::env::set_current_dir(dir)?;
    }

    match real_provider() {
        Some(p) => {
            let mcp = resolve_configured_mcp().await; // 可选接入 MCP 服务器
            match task {
                Some(t) => run_once(p, mcp, &t).await, // 一次性
                None => repl(p, mcp).await,            // 交互式
            }
        }
        None => {
            eprintln!(
                "[ridge] 未检测到 RIDGE_API_KEY,跑离线脚本 demo(设置密钥即用真实 LLM / REPL)。\n"
            );
            run_demo().await
        }
    }
}

/// 按 env 接入 MCP 服务器:`RIDGE_MCP_CMD`(stdio 服务器可执行文件)+ `RIDGE_MCP_NAME`(命名空间)。
/// 降级不崩:没配 / 起不来 → 空,agent 只用内置工具。
async fn resolve_configured_mcp() -> McpTools {
    let cmd = match std::env::var("RIDGE_MCP_CMD") {
        Ok(c) if !c.is_empty() => c,
        _ => return McpTools::empty(),
    };
    let name = std::env::var("RIDGE_MCP_NAME").unwrap_or_else(|_| "mcp".to_string());
    let transport = match StdioTransport::spawn(&cmd, &[]) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[ridge] MCP 启动失败 {cmd}: {e}");
            return McpTools::empty();
        }
    };
    let client = Arc::new(McpClient::new(name.clone(), Box::new(transport)));
    let tools = resolve_mcp(vec![client]).await;
    eprintln!("[ridge] 已接入 MCP `{name}`");
    tools
}

/// 解析参数:非 flag 拼成任务(无 → REPL);`--cwd <dir>` 切换工作目录。
fn parse_args() -> (Option<String>, Option<String>) {
    let mut task = String::new();
    let mut cwd = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--cwd" => cwd = args.next(),
            _ => {
                if !task.is_empty() {
                    task.push(' ');
                }
                task.push_str(&a);
            }
        }
    }
    let task = if task.is_empty() { None } else { Some(task) };
    (task, cwd)
}

/// 按环境变量装配真实 provider;没有 key 就返回 None(走 demo)。密钥绝不打印。
fn real_provider() -> Option<Arc<dyn LlmProvider>> {
    let key = std::env::var("RIDGE_API_KEY")
        .ok()
        .filter(|k| !k.is_empty())?;
    let kind = std::env::var("RIDGE_PROVIDER").unwrap_or_else(|_| "openai".to_string());
    let model = std::env::var("RIDGE_MODEL").ok();
    match kind.as_str() {
        "anthropic" => {
            let base = std::env::var("RIDGE_BASE_URL")
                .unwrap_or_else(|_| "https://api.anthropic.com/v1".to_string());
            let model = model.unwrap_or_else(|| "claude-sonnet-4-6".to_string());
            Some(Arc::new(AnthropicProvider::new(base, model, key)))
        }
        _ => {
            let base = std::env::var("RIDGE_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
            let model = model.unwrap_or_else(|| "gpt-4o".to_string());
            Some(Arc::new(OpenAiProvider::new(base, model, key)))
        }
    }
}

/// 一次性任务:一律放行,跑完写 trace.json + 打印结果。
async fn run_once(provider: Arc<dyn LlmProvider>, mcp: McpTools, task: &str) -> anyhow::Result<()> {
    let app = build_llm_agent_gated(provider, mcp, Arc::new(AutoApprove))?;
    let out = run_streamed(&app, AgentState::new(task)).await?;
    trace_and_report(&out);
    Ok(())
}

/// 交互式 REPL:跨轮携带 history,有副作用的工具执行前 stdin 确认。`/exit` `/reset` `/compact` `/help`。
async fn repl(provider: Arc<dyn LlmProvider>, mcp: McpTools) -> anyhow::Result<()> {
    println!("ridge REPL —— 输入任务开跑;/help 看命令,/exit 退出。危险操作会先问你 [y/N]。\n");
    let approver: Arc<dyn Approver> = Arc::new(StdinApprover);
    let app = build_llm_agent_gated(provider, mcp, approver)?;
    let mut history: Vec<Message> = Vec::new();

    loop {
        print!("ridge> ");
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
        let state = AgentState::new(input).with_history(history.clone());
        match run_streamed(&app, state).await {
            Ok(out) => {
                history = out.history.clone();
                trace_and_report(&out);
            }
            Err(e) => eprintln!("[ridge] 出错:{e}"),
        }
    }
    println!("bye.");
    Ok(())
}

/// 写审计轨迹 trace.json(best-effort)+ 打印结果。
fn trace_and_report(out: &AgentState) {
    match write_trace(out, "trace.json") {
        Ok(()) => eprintln!("[ridge] 审计轨迹已写 trace.json"),
        Err(e) => eprintln!("[ridge] 写 trace.json 失败: {e}"),
    }
    print_report(out);
}

/// 跑 agent + 实时把「哪个节点在跑」流式打到 stderr(P2:引擎 StreamEvent)。
async fn run_streamed(
    app: &CompiledGraph<AgentState>,
    state: AgentState,
) -> anyhow::Result<AgentState> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<StreamEvent<AgentState>>();
    let printer = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if let StreamEvent::NodeFinished { node, superstep } = ev {
                eprint!("· {node}#{superstep} ");
                std::io::stderr().flush().ok();
            }
        }
        eprintln!();
    });

    let out = app
        .invoke_with(state, &RunConfig::default(), None, Some(&tx))
        .await?;
    drop(tx);
    let _ = printer.await;
    Ok(out)
}

/// 离线 demo:脚本大脑 + 假工具,零联网跑通闭环。
async fn run_demo() -> anyhow::Result<()> {
    let app = build_agent(scripted(), default_tool())?;
    let out = app
        .invoke(AgentState::new("make the test suite pass"))
        .await?;
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
    if let Some(last) = out.messages.last() {
        println!("\n{last}");
    }
    println!(
        "[approved={} steps={} tokens={}]",
        out.approved, out.steps, out.total_tokens
    );
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
