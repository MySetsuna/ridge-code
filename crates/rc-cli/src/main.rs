//! ridge-code M1:单模型 agent loop + 客观验证 + 失败自动修复循环。详见 HANDOFF.md §3、PLAN.md §7。

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use rc_providers::{LlmProvider, OpenAiCompatProvider};
use rc_tools::{dispatch, tool_specs};
use rc_types::{Diagnostic, Message, ToolSpec, Verdict};
use rc_verify::{resolve_plan, verify};
use serde::Deserialize;
use std::path::PathBuf;
use tracing::{debug, info, warn};

#[derive(Parser)]
#[command(name = "ridge-code", version, about = "成本优化的编码 agent CLI(M1)")]
struct Cli {
    /// 要执行的任务描述
    task: String,
    /// 单次执行内的工具循环最大轮数
    #[arg(long, default_value_t = 12)]
    max_steps: usize,
    /// 验证失败后最多自动修复几轮
    #[arg(long, default_value_t = 3)]
    max_repairs: usize,
    /// 切换到该工作目录后再执行
    #[arg(long)]
    cwd: Option<PathBuf>,
}

#[derive(Deserialize)]
struct Config {
    provider: ProviderConfig,
}

#[derive(Deserialize)]
struct ProviderConfig {
    base_url: String,
    model: String,
    /// 直接填写的 key(可选)
    #[serde(default)]
    api_key: Option<String>,
    /// 从该环境变量读 key(默认 RIDGE_API_KEY)
    #[serde(default)]
    api_key_env: Option<String>,
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn config_path() -> Result<PathBuf> {
    let home = home_dir().ok_or_else(|| anyhow!("找不到 HOME / USERPROFILE"))?;
    Ok(home.join(".ridge").join("config.toml"))
}

fn load_config() -> Result<Config> {
    let path = config_path()?;
    let text = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "读取配置 {} 失败(可参考仓库 config.example.toml)",
            path.display()
        )
    })?;
    toml::from_str(&text).context("解析 config.toml 失败")
}

fn resolve_api_key(p: &ProviderConfig) -> Result<String> {
    if let Some(k) = &p.api_key {
        if !k.is_empty() {
            return Ok(k.clone());
        }
    }
    let env_name = p.api_key_env.as_deref().unwrap_or("RIDGE_API_KEY");
    std::env::var(env_name)
        .with_context(|| format!("config 未填 api_key,且环境变量 {env_name} 未设置"))
}

/// 跑一轮 agent:工具循环直到模型不再调用工具,返回其最终文本。
async fn run_agent(
    provider: &impl LlmProvider,
    messages: &mut Vec<Message>,
    tools: &[ToolSpec],
    max_steps: usize,
) -> Result<String> {
    for step in 1..=max_steps {
        let completion = provider.complete(messages.as_slice(), tools).await?;
        debug!(
            step,
            in_tok = completion.usage.input_tokens,
            out_tok = completion.usage.output_tokens,
            "模型回复"
        );
        let msg = completion.message;

        if msg.tool_calls.is_empty() {
            return Ok(msg.content);
        }

        messages.push(msg.clone());
        for call in &msg.tool_calls {
            info!(step, tool = %call.name, args = %call.arguments, "调用工具");
            let result = dispatch(call).await;
            messages.push(Message::tool_result(call.id.clone(), result));
        }
    }
    Err(anyhow!("达到最大轮数 {max_steps} 仍未给出最终答复"))
}

/// 把诊断渲染成回喂模型的反馈文本。
fn render_reasons(reasons: &[Diagnostic]) -> String {
    reasons
        .iter()
        .map(|d| format!("## [{}] 失败\n{}", d.source, d.detail))
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[tokio::main]
async fn main() -> Result<()> {
    // 先加载 .env.local / .env(若存在),让 RIDGE_API_KEY、RUST_LOG 等可从文件读。
    // 在 --cwd 切换之前加载,确保按「启动目录」找到文件;已存在的环境变量优先,不被覆盖。
    let _ = dotenvy::from_filename(".env.local");
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    if let Some(cwd) = &cli.cwd {
        std::env::set_current_dir(cwd)
            .with_context(|| format!("切换目录到 {} 失败", cwd.display()))?;
    }
    let project_dir = std::env::current_dir().context("获取当前目录失败")?;

    let cfg = load_config()?;
    let api_key = resolve_api_key(&cfg.provider)?;
    let provider = OpenAiCompatProvider::new(cfg.provider.base_url, api_key, cfg.provider.model);

    let plan = resolve_plan(&project_dir);
    info!(
        model = provider.model_id(),
        project = %project_dir.display(),
        checks = plan.checks.len(),
        "ridge-code M1 启动"
    );

    let system = "你是 ridge-code,一个编码助手。你能调用工具读写文件、列目录、执行 shell 命令来完成编码任务。\
请先用 list_dir / read_file 了解上下文,再用 write_file / run_shell 实施改动;你写的代码应当能通过编译。\
完成后用一句中文总结你做了什么,并停止调用工具。\
如果之后收到「验证失败」的反馈,请阅读错误信息并直接修改代码修复,然后再停止。";

    let mut messages = vec![Message::system(system), Message::user(cli.task.as_str())];
    let tools = tool_specs();

    // 初次执行任务。
    let mut answer = run_agent(&provider, &mut messages, &tools, cli.max_steps).await?;

    // 验证 + 失败修复循环。
    let mut repairs = 0usize;
    loop {
        match verify(&plan, &project_dir).await? {
            Verdict::Pass => {
                info!("✅ 验证通过");
                break;
            }
            Verdict::Uncertain { note } => {
                warn!(%note, "⚠️ 无法客观验证,按完成处理");
                break;
            }
            Verdict::Fail { reasons } => {
                if repairs >= cli.max_repairs {
                    println!("\n{answer}");
                    return Err(anyhow!(
                        "修复 {repairs} 轮后仍未通过验证。最后的失败:\n{}",
                        render_reasons(&reasons)
                    ));
                }
                repairs += 1;
                warn!(round = repairs, "❌ 验证失败,启动第 {repairs} 轮修复");
                let feedback = format!(
                    "你的改动没有通过验证。以下是验证输出,请据此直接修改代码修复,然后再停止:\n\n{}",
                    render_reasons(&reasons)
                );
                messages.push(Message::user(feedback));
                answer = run_agent(&provider, &mut messages, &tools, cli.max_steps).await?;
            }
        }
    }

    println!("\n{answer}");
    info!(repairs, "任务完成");
    Ok(())
}
