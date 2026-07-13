use std::sync::Arc;

use agent::{build_agent, build_llm_agent, default_tool, scripted, AgentState};
use langgraph::{MemoryCheckpointer, RunConfig};
use provider::{AnthropicProvider, LlmProvider, OpenAiProvider};

/// ridge —— 编码 agent CLI。
///
/// 用法:
///   ridge "把 add/mul 实现出来并各写一个单测"        # 有 key 时用真实 LLM 跑
///   ridge --cwd /path/to/project "修好编译错误"        # 在目标项目里跑
///   ridge                                              # 没配 key 时跑离线脚本 demo
///
/// 配置(环境变量,支持 .env 由 shell 提前 export):
///   RIDGE_API_KEY    provider 密钥(必填才走真实 LLM,否则跑 demo)
///   RIDGE_PROVIDER   anthropic | openai(缺省 openai)
///   RIDGE_MODEL      模型名(缺省按 provider 给个常见值)
///   RIDGE_BASE_URL   API base(缺省用官方地址)
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (task, cwd) = parse_args();
    if let Some(dir) = &cwd {
        std::env::set_current_dir(dir)?;
    }

    match real_provider() {
        Some(p) => run_llm(p, &task).await,
        None => {
            eprintln!("[ridge] 未检测到 RIDGE_API_KEY,跑离线脚本 demo(设置密钥即用真实 LLM)。\n");
            run_demo().await
        }
    }
}

/// 解析参数:非 flag 拼成任务;`--cwd <dir>` 切换工作目录。
fn parse_args() -> (String, Option<String>) {
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
    if task.is_empty() {
        task = "make the test suite pass".to_string();
    }
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

/// 真实 LLM 路径:结构化 tool_call 驱动真实工具(shell/文件),verify 认确定性信号。
async fn run_llm(provider: Arc<dyn LlmProvider>, task: &str) -> anyhow::Result<()> {
    let app = build_llm_agent(provider)?;
    let cp = MemoryCheckpointer::new();
    let out = app
        .invoke_with(
            AgentState::new(task),
            &RunConfig::default(),
            Some(&cp),
            None,
        )
        .await?;
    print_report(&out.messages, out.approved, out.steps, out.total_tokens);
    Ok(())
}

/// 离线 demo:脚本大脑 + 假工具,零联网跑通闭环。
async fn run_demo() -> anyhow::Result<()> {
    let app = build_agent(scripted(), default_tool())?;
    let cp = MemoryCheckpointer::new();
    let out = app
        .invoke_with(
            AgentState::new("make the test suite pass"),
            &RunConfig::default(),
            Some(&cp),
            None,
        )
        .await?;

    print_report(&out.messages, out.approved, out.steps, out.total_tokens);
    println!("\n== supersteps (checkpoints) ==");
    for c in cp.history() {
        println!("  step {:>2} -> next {:?}", c.step, c.frontier);
    }
    Ok(())
}

fn print_report(messages: &[String], approved: bool, steps: usize, tokens: usize) {
    println!("== agent trace ==");
    for m in messages {
        println!("  {m}");
    }
    println!("\n== result: approved={approved} steps={steps} tokens={tokens} ==");
}
