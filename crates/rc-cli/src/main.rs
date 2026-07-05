//! ridge-code M2:薄壳。读配置(强/弱双 provider)→ 构造编排器 → 跑 → 打印报告与成本账单。
//! 编排逻辑全在 rc-core。详见 PLAN.md §2。

mod catalog;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use rc_core::{Orchestrator, OrchestratorConfig};
use rc_mcp::{McpHub, McpServerConfig};
use rc_providers::{AnthropicProvider, LlmProvider, OpenAiCompatProvider};
use rc_types::Difficulty;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{info, warn};

#[derive(Parser)]
#[command(
    name = "ridge-code",
    version,
    about = "成本优化的编码 agent CLI(强/弱编排 + 多供应商)",
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true
)]
struct Cli {
    /// 子命令(providers/models/init);省略则按「运行任务」处理。
    #[command(subcommand)]
    command: Option<Command>,
    /// 要执行的任务描述(无子命令时)
    task: Option<String>,
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

#[derive(Subcommand)]
enum Command {
    /// 列出内置供应商目录(base_url / kind / key 环境变量)
    Providers,
    /// 列出某供应商(或全部)的示例模型
    Models {
        /// 供应商名(省略则列全部)
        provider: Option<String>,
    },
    /// 按「供应商 + 模型」生成 ~/.ridge/config.toml(不用手写 base_url)
    Init {
        /// 供应商名(见 `ridge-code providers`)
        provider: String,
        /// 模型 ID(省略则用该供应商的第一个示例模型)
        model: Option<String>,
        /// 覆盖已存在的 config.toml
        #[arg(long)]
        force: bool,
    },
}

/// provider 的 wire 协议:openai 兼容(默认)或原生 anthropic。
#[derive(Deserialize, Clone, Copy, Default)]
#[serde(rename_all = "lowercase")]
enum ProviderKind {
    #[default]
    Openai,
    Anthropic,
}

impl ProviderKind {
    fn as_str(&self) -> &'static str {
        match self {
            ProviderKind::Openai => "openai",
            ProviderKind::Anthropic => "anthropic",
        }
    }
}

#[derive(Deserialize, Clone)]
struct ProviderConfig {
    /// 命名注册表 `[[providers]]` 里用作引用名(内联段无需填)。
    #[serde(default)]
    name: Option<String>,
    /// wire 协议;缺省 openai(旧配置零改动)。
    #[serde(default)]
    kind: ProviderKind,
    base_url: String,
    model: String,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    api_key_env: Option<String>,
    /// 仅 anthropic 用(必填项),缺省由 provider 兜底(8192)。
    #[serde(default)]
    max_tokens: Option<u32>,
}

/// `[roles]`:strong/weak 按名引用 `[[providers]]` 里的 provider。
#[derive(Deserialize, Default)]
struct Roles {
    #[serde(default)]
    strong: Option<String>,
    #[serde(default)]
    weak: Option<String>,
}

/// `[routing]`(可选):按难度把 worker 覆盖到任意命名 provider。
#[derive(Deserialize, Default)]
struct Routing {
    #[serde(default)]
    trivial: Option<String>,
    #[serde(default)]
    moderate: Option<String>,
    #[serde(default)]
    hard: Option<String>,
}

/// 三种写法(自上而下优先):
/// ①命名注册表 `[[providers]]` + `[roles]`(多供应商/多模型,可选 `[routing]` 按难度路由);
/// ②`[strong]`+`[weak]`(混合两档);③单 `[provider]`(强=弱)。后两者为向后兼容。
#[derive(Deserialize)]
struct Config {
    #[serde(default)]
    provider: Option<ProviderConfig>,
    #[serde(default)]
    strong: Option<ProviderConfig>,
    #[serde(default)]
    weak: Option<ProviderConfig>,
    /// 命名 provider 注册表:任意 N 个,由 `[roles]`/`[routing]` 按名引用。
    #[serde(default)]
    providers: Vec<ProviderConfig>,
    #[serde(default)]
    roles: Roles,
    #[serde(default)]
    routing: Routing,
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
    match p.kind {
        ProviderKind::Openai => Ok(Box::new(OpenAiCompatProvider::new(
            p.base_url.clone(),
            key,
            p.model.clone(),
        ))),
        ProviderKind::Anthropic => Ok(Box::new(AnthropicProvider::new(
            p.base_url.clone(),
            key,
            p.model.clone(),
            p.max_tokens,
        ))),
    }
}

/// 从 `[[providers]]` 建「名 → 配置」注册表;无 name 的项告警跳过。
fn build_registry(providers: &[ProviderConfig]) -> HashMap<String, ProviderConfig> {
    let mut map = HashMap::new();
    for p in providers {
        match &p.name {
            Some(name) => {
                map.insert(name.clone(), p.clone());
            }
            None => warn!("[[providers]] 有一项缺少 name,已跳过"),
        }
    }
    map
}

/// 按名从注册表取 provider 配置(报错时指明是哪个角色引用的)。
fn lookup(
    registry: &HashMap<String, ProviderConfig>,
    name: &str,
    role: &str,
) -> Result<ProviderConfig> {
    registry
        .get(name)
        .cloned()
        .with_context(|| format!("[{role}] 指向未在 [[providers]] 定义的 provider: {name}"))
}

/// 解析强/弱 provider 配置:优先 `[roles]` 按名引用,否则回落 `[strong]`/`[weak]` → `[provider]`。
fn resolve_tiers(
    cfg: &Config,
    registry: &HashMap<String, ProviderConfig>,
) -> Result<(ProviderConfig, ProviderConfig)> {
    let strong = match &cfg.roles.strong {
        Some(name) => lookup(registry, name, "roles.strong")?,
        None => cfg
            .strong
            .clone()
            .or_else(|| cfg.provider.clone())
            .context("配置缺少 provider:用 [[providers]]+[roles],或 [strong]/[provider]")?,
    };
    let weak = match &cfg.roles.weak {
        Some(name) => lookup(registry, name, "roles.weak")?,
        None => cfg
            .weak
            .clone()
            .or_else(|| cfg.provider.clone())
            .unwrap_or_else(|| strong.clone()),
    };
    Ok((strong, weak))
}

/// 解析可选的 `[routing]`:按难度构建 worker 覆盖 provider(名字须在注册表)。
fn resolve_worker_models(
    cfg: &Config,
    registry: &HashMap<String, ProviderConfig>,
) -> Result<HashMap<Difficulty, Box<dyn LlmProvider>>> {
    let mut models: HashMap<Difficulty, Box<dyn LlmProvider>> = HashMap::new();
    let entries = [
        (&cfg.routing.trivial, Difficulty::Trivial, "routing.trivial"),
        (
            &cfg.routing.moderate,
            Difficulty::Moderate,
            "routing.moderate",
        ),
        (&cfg.routing.hard, Difficulty::Hard, "routing.hard"),
    ];
    for (name_opt, diff, role) in entries {
        if let Some(name) = name_opt {
            let pc = lookup(registry, name, role)?;
            models.insert(diff, build_provider(&pc)?);
        }
    }
    Ok(models)
}

/// `providers`:列出内置供应商目录。
fn cmd_providers() {
    println!("内置供应商目录(base_url/kind/key 为稳定事实;模型见 `ridge-code models <name>`):\n");
    println!(
        "  {:<11} {:<10} {:<5} {:<46} {:<7}",
        "NAME", "KIND", "FREE", "BASE_URL", "KEY_ENV"
    );
    for e in catalog::catalog() {
        let free = if e.free { "✓" } else { "" };
        println!(
            "  {:<11} {:<10} {:<5} {:<46} {}",
            e.name,
            e.kind.as_str(),
            free,
            e.base_url,
            e.api_key_env
        );
    }
    println!(
        "\n用 `ridge-code models <name>` 看示例模型,`ridge-code init <name> [model]` 一键生成配置。"
    );
}

/// `models`:列出某供应商(或全部)的示例模型。
fn cmd_models(provider: Option<&str>) -> Result<()> {
    println!("示例模型(以各家官方控制台/文档为准,可能更新;init 可填任意 model id):\n");
    let entries: Vec<&catalog::CatalogEntry> = match provider {
        Some(name) => vec![catalog::find(name)
            .with_context(|| format!("未知供应商: {name}(见 `ridge-code providers`)"))?],
        None => catalog::catalog().iter().collect(),
    };
    for e in entries {
        println!("[{}] {}", e.name, e.note);
        for m in e.models {
            println!("  - {m}");
        }
        println!();
    }
    Ok(())
}

/// `init`:按「供应商 + 模型」生成 ~/.ridge/config.toml。已存在则默认不覆盖(打印片段)。
fn cmd_init(provider: &str, model: Option<&str>, force: bool) -> Result<()> {
    let e = catalog::find(provider)
        .with_context(|| format!("未知供应商: {provider}(见 `ridge-code providers`)"))?;
    let model = model
        .map(|s| s.to_string())
        .or_else(|| e.models.first().map(|s| s.to_string()))
        .context("该供应商无示例模型,请显式指定 model")?;

    let home = home_dir().ok_or_else(|| anyhow!("找不到 HOME / USERPROFILE"))?;
    let dir = home.join(".ridge");
    let path = dir.join("config.toml");

    let key_line = if e.free {
        "api_key = \"local\"                    # 本地无需真实 key".to_string()
    } else {
        format!("api_key_env = \"{}\"", e.api_key_env)
    };
    let maxtok = if matches!(e.kind, ProviderKind::Anthropic) {
        "\n# max_tokens = 8192"
    } else {
        ""
    };
    let scaffold = format!(
        "# 由 `ridge-code init` 生成。密钥别提交进 git。\n\
[[providers]]\n\
name = \"{name}\"\n\
kind = \"{kind}\"\n\
base_url = \"{base}\"\n\
model = \"{model}\"\n\
{key}{maxtok}\n\
\n\
[roles]\n\
strong = \"{name}\"\n\
weak = \"{name}\"\n",
        name = e.name,
        kind = e.kind.as_str(),
        base = e.base_url,
        key = key_line,
    );

    if path.exists() && !force {
        println!(
            "⚠️ {} 已存在,未覆盖(加 --force 覆盖)。把下面片段粘进去即可:\n",
            path.display()
        );
        println!("{scaffold}");
        return Ok(());
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("创建目录 {} 失败", dir.display()))?;
    std::fs::write(&path, &scaffold).with_context(|| format!("写入 {} 失败", path.display()))?;
    println!(
        "✅ 已写入 {}(供应商 {}, 模型 {})",
        path.display(),
        e.name,
        model
    );
    if e.free {
        println!("本地供应商:先 `ollama pull {model}`,再运行。");
    } else {
        println!(
            "请设置密钥:export {}=你的key(或写进启动目录的 .env.local)。",
            e.api_key_env
        );
    }
    println!("然后:ridge-code --cwd /path/to/project \"你的任务\"");
    Ok(())
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

    // 子命令:providers / models / init —— 无需 config/key,处理完即返回。
    match &cli.command {
        Some(Command::Providers) => {
            cmd_providers();
            return Ok(());
        }
        Some(Command::Models { provider }) => return cmd_models(provider.as_deref()),
        Some(Command::Init {
            provider,
            model,
            force,
        }) => return cmd_init(provider, model.as_deref(), *force),
        None => {}
    }
    let task = cli
        .task
        .clone()
        .context("缺少任务描述;或用子命令 providers / models / init(--help 看用法)")?;

    if let Some(cwd) = &cli.cwd {
        std::env::set_current_dir(cwd)
            .with_context(|| format!("切换目录到 {} 失败", cwd.display()))?;
    }
    let project_dir = std::env::current_dir().context("获取当前目录失败")?;

    let cfg = load_config()?;
    // 命名注册表 + 角色/路由解析(缺省回落旧内联写法)。
    let registry = build_registry(&cfg.providers);
    let (strong_cfg, weak_cfg) = resolve_tiers(&cfg, &registry)?;
    let worker_models = resolve_worker_models(&cfg, &registry)?;

    let strong_model = strong_cfg.model.clone();
    let weak_model = weak_cfg.model.clone();
    let strong = build_provider(&strong_cfg)?;
    let weak = build_provider(&weak_cfg)?;

    info!(
        strong = %strong_model,
        weak = %weak_model,
        providers = registry.len(),
        worker_overrides = worker_models.len(),
        project = %project_dir.display(),
        "ridge-code 启动"
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

    // N 模型:声明了 [routing] 则按难度把 worker 覆盖到命名 provider。
    if !worker_models.is_empty() {
        orch = orch.with_worker_models(worker_models);
    }

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
    let outcome = match orch.run(&task).await {
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
