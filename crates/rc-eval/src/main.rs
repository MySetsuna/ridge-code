//! ridge-code eval harness(M3 最小闭环)。详见 docs/superpowers/specs/2026-06-25-m3-eval-design.md。

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use rc_eval::{reporter, runner, tasks, RunMode};
use rc_providers::{LlmProvider, OpenAiCompatProvider};
use rc_types::{Pricing, Rate};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "rc-eval", about = "ridge-code eval harness(M3 最小闭环)")]
struct Cli {
    /// 用离线假模型跑(零成本、零联网)
    #[arg(long)]
    offline: bool,
    /// 保留临时工作副本以便调试
    #[arg(long)]
    keep: bool,
}

#[derive(Deserialize, Clone)]
struct ProviderCfg {
    base_url: String,
    model: String,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    api_key_env: Option<String>,
    #[serde(default)]
    price_in: Option<f64>,
    #[serde(default)]
    price_out: Option<f64>,
}

#[derive(Deserialize)]
struct FileCfg {
    #[serde(default)]
    strong: Option<ProviderCfg>,
    #[serde(default)]
    weak: Option<ProviderCfg>,
    #[serde(default)]
    provider: Option<ProviderCfg>,
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}

fn resolve_api_key(p: &ProviderCfg) -> Result<String> {
    if let Some(k) = &p.api_key {
        if !k.is_empty() {
            return Ok(k.clone());
        }
    }
    let env_name = p.api_key_env.as_deref().unwrap_or("RIDGE_API_KEY");
    std::env::var(env_name).with_context(|| format!("config 未填 api_key,且环境变量 {env_name} 未设置"))
}

fn build_provider(p: &ProviderCfg) -> Result<Box<dyn LlmProvider>> {
    let key = resolve_api_key(p)?;
    Ok(Box::new(OpenAiCompatProvider::new(p.base_url.clone(), key, p.model.clone())))
}

fn rate_of(p: &ProviderCfg) -> Rate {
    Rate { in_per_mtok: p.price_in.unwrap_or(0.0), out_per_mtok: p.price_out.unwrap_or(0.0) }
}

/// 读 ~/.ridge/config.toml,返回 (strong, weak, pricing)。
fn load_real() -> Result<(ProviderCfg, ProviderCfg, Pricing)> {
    let home = home_dir().ok_or_else(|| anyhow!("找不到 HOME / USERPROFILE"))?;
    let path = home.join(".ridge").join("config.toml");
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("读取配置 {} 失败(可参考仓库 config.example.toml)", path.display()))?;
    let cfg: FileCfg = toml::from_str(&text).context("解析 config.toml 失败")?;
    let strong = cfg.strong.clone().or_else(|| cfg.provider.clone()).context("配置缺少 [strong] 或 [provider]")?;
    let weak = cfg.weak.clone().or_else(|| cfg.provider.clone()).unwrap_or_else(|| strong.clone());
    let pricing = Pricing { strong: rate_of(&strong), weak: rate_of(&weak) };
    Ok((strong, weak, pricing))
}

fn default_pricing() -> Pricing {
    // 离线示意定价(仅用于演示报表;离线 token 来自 stub)。
    Pricing {
        strong: Rate { in_per_mtok: 3.0, out_per_mtok: 15.0 },
        weak: Rate { in_per_mtok: 0.5, out_per_mtok: 1.5 },
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::from_filename(".env.local");
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let tasks = tasks::builtin_tasks();

    let (pricing, real) = if cli.offline {
        (default_pricing(), None)
    } else {
        let (s, w, p) = load_real()?;
        (p, Some((s, w)))
    };

    let mut outcomes = Vec::new();
    for task in &tasks {
        for mode in [RunMode::Baseline, RunMode::Orchestrated] {
            let (strong, weak) = if cli.offline {
                runner::offline_providers(task, mode)
            } else {
                let (s, w) = real.as_ref().unwrap();
                (build_provider(s)?, build_provider(w)?)
            };
            let outcome = runner::run_one(task, mode, strong, weak, &pricing, cli.keep).await;
            println!(
                "· {} [{:?}] success={} usd=${:.4} ({} ms)",
                outcome.task, outcome.mode, outcome.success, outcome.usd, outcome.elapsed_ms
            );
            outcomes.push(outcome);
        }
    }

    let summaries = reporter::summarize(&outcomes);
    println!("{}", reporter::render(&summaries));

    let out_path = PathBuf::from("target/eval/result.json");
    reporter::write_json(&outcomes, &out_path)?;
    println!("结果已写入 {}", out_path.display());
    Ok(())
}
