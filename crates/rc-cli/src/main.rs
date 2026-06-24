//! ridge-code M0 walking skeleton:单模型 agent loop(无编排)。详见 HANDOFF.md §3。

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use rc_providers::{LlmProvider, OpenAiCompatProvider};
use rc_tools::{dispatch, tool_specs};
use rc_types::Message;
use serde::Deserialize;
use std::path::PathBuf;
use tracing::{debug, info};

#[derive(Parser)]
#[command(name = "ridge-code", version, about = "成本优化的编码 agent CLI(M0 skeleton)")]
struct Cli {
    /// 要执行的任务描述
    task: String,
    /// 工具循环最大轮数
    #[arg(long, default_value_t = 12)]
    max_steps: usize,
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

    let cfg = load_config()?;
    let api_key = resolve_api_key(&cfg.provider)?;
    let provider = OpenAiCompatProvider::new(cfg.provider.base_url, api_key, cfg.provider.model);
    info!(model = provider.model_id(), "ridge-code M0 启动");

    let system = "你是 ridge-code,一个编码助手。你能调用工具读写文件、列目录、执行 shell 命令来完成编码任务。\
请先用 list_dir / read_file 了解上下文,再用 write_file / run_shell 实施改动。\
完成后用一句中文总结你做了什么,并停止调用工具。";

    let mut messages = vec![Message::system(system), Message::user(cli.task.as_str())];
    let tools = tool_specs();

    for step in 1..=cli.max_steps {
        let completion = provider.complete(&messages, &tools).await?;
        debug!(
            step,
            in_tok = completion.usage.input_tokens,
            out_tok = completion.usage.output_tokens,
            "模型回复"
        );
        let msg = completion.message;

        if msg.tool_calls.is_empty() {
            println!("\n{}", msg.content);
            info!(step, "任务完成");
            return Ok(());
        }

        messages.push(msg.clone());
        for call in &msg.tool_calls {
            info!(step, tool = %call.name, args = %call.arguments, "调用工具");
            let result = dispatch(call).await;
            messages.push(Message::tool_result(call.id.clone(), result));
        }
    }

    Err(anyhow!("达到最大轮数 {} 仍未完成", cli.max_steps))
}
