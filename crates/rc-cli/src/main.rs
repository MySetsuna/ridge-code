//! ridge-code M2:薄壳。读配置(强/弱双 provider)→ 构造编排器 → 跑 → 打印报告与成本账单。
//! 编排逻辑全在 rc-core。详见 PLAN.md §2。

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use rc_core::{Orchestrator, OrchestratorConfig};
use rc_mcp::{McpHub, McpServerConfig};
use rc_providers::{LlmProvider, OpenAiCompatProvider};
use serde::Deserialize;
use std::path::PathBuf;
use tracing::{info, warn};

#[derive(Parser)]
#[command(
    name = "ridge-code",
    version,
    about = "成本优化的编码 agent CLI(M2:强/弱编排)"
)]
struct Cli {
    /// 要执行的任务描述
    task: String,
    /// 单次 agent 运行内的工具循环最大轮数
    #[arg(long, default_value_t = 12)]
    max_steps: usize,
    /// 验证失败后最多自动修复几轮
    #[arg(long, default_value_t = 3)]
    max_repairs: usize,
    /// 切换到该工作目录后再执行
    #[arg(long)]
    cwd: Option<PathBuf>,
}

#[derive(Deserialize, Clone)]
struct ProviderConfig {
    base_url: String,
    model: String,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    api_key_env: Option<String>,
}

/// 支持两种写法:`[provider]`(单 provider,强=弱兼容旧配置)或 `[strong]`+`[weak]`(混合)。
#[derive(Deserialize)]
struct Config {
    #[serde(default)]
    provider: Option<ProviderConfig>,
    #[serde(default)]
    strong: Option<ProviderConfig>,
    #[serde(default)]
    weak: Option<ProviderConfig>,
    /// 可选的外部 MCP 服务器(M4):`[[mcp]]` 数组,每项 name/command/args/env。
    #[serde(default)]
    mcp: Vec<McpServerConfig>,
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn load_config() -> Result<Config> {
    let home = home_dir().ok_or_else(|| anyhow!("找不到 HOME / USERPROFILE"))?;
    let path = home.join(".ridge").join("config.toml");
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

fn build_provider(p: &ProviderConfig) -> Result<Box<dyn LlmProvider>> {
    let key = resolve_api_key(p)?;
    Ok(Box::new(OpenAiCompatProvider::new(
        p.base_url.clone(),
        key,
        p.model.clone(),
    )))
}

#[tokio::main]
async fn main() -> Result<()> {
    // 先加载 .env.local / .env(若存在),让 RIDGE_API_KEY、RUST_LOG 等可从文件读。
    // 在 --cwd 切换之前加载,确保按「启动目录」找到文件;已存在的环境变量优先。
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
    let strong_cfg = cfg
        .strong
        .clone()
        .or_else(|| cfg.provider.clone())
        .context("配置缺少 [strong] 或 [provider]")?;
    let weak_cfg = cfg
        .weak
        .clone()
        .or_else(|| cfg.provider.clone())
        .unwrap_or_else(|| strong_cfg.clone());

    let strong_model = strong_cfg.model.clone();
    let weak_model = weak_cfg.model.clone();
    let strong = build_provider(&strong_cfg)?;
    let weak = build_provider(&weak_cfg)?;

    info!(
        strong = %strong_model,
        weak = %weak_model,
        project = %project_dir.display(),
        "ridge-code M2 启动"
    );

    let mut orch = Orchestrator::new(
        strong,
        weak,
        project_dir,
        OrchestratorConfig {
            max_steps: cli.max_steps,
            max_repairs: cli.max_repairs,
        },
    );

    // M4:声明了 [[mcp]] 则连接外部 MCP 服务器,把其工具并入 Worker 工具集。
    if !cfg.mcp.is_empty() {
        let hub = McpHub::connect(cfg.mcp).await;
        if hub.is_empty() {
            warn!("已声明 [[mcp]] 但无服务器连接成功,仅用内置工具");
        } else {
            info!(tools = hub.tool_specs().len(), "MCP 工具已接入");
        }
        orch = orch.with_mcp(hub);
    }

    // 运行;无论成败都关闭 MCP 子进程会话。
    let outcome = match orch.run(&cli.task).await {
        Ok(o) => o,
        Err(e) => {
            orch.shutdown().await;
            return Err(e);
        }
    };

    let c = &outcome.cost;
    let review_status = if outcome.reviewed {
        if outcome.approved {
            "通过"
        } else {
            "未通过"
        }
    } else {
        "跳过"
    };
    println!("\n──────── ridge-code 运行报告 ────────");
    println!(
        "子任务: {}   修复轮次: {}   评审: {}",
        outcome.subtasks, outcome.repairs, review_status
    );
    println!(
        "Token  强模型: {} (in {} / out {})   弱模型: {} (in {} / out {})",
        c.strong_tokens(),
        c.strong_in,
        c.strong_out,
        c.weak_tokens(),
        c.weak_in,
        c.weak_out
    );
    println!(
        "强模型 token 占比: {:.0}%  (越低越省钱,见 PLAN §9)",
        c.strong_share() * 100.0
    );

    orch.shutdown().await;
    Ok(())
}
