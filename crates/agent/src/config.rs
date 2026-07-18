/// `~/.ridge/config.json`:一处配 provider/model/预算/多 MCP/skills(env 仍可覆盖)。
/// 密钥优先走 env(`RIDGE_API_KEY` 或档案 `key_env` 指名的变量);也可在档案里内联 `api_key`
/// (明文存盘,自担风险)。启动取密钥顺序见 `main.rs::real_provider`。
#[derive(Debug, Default, Clone, serde::Deserialize)]
#[serde(default)]
pub struct Config {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    /// 顶层「主 provider」的内联明文密钥(可选,自担明文存盘风险)。填了它,启动即用
    /// 顶层 provider/model/base_url + 此 key,无需 `RIDGE_API_KEY`。留空则回落到 env 或 `providers[]` 档案。
    pub api_key: Option<String>,
    /// 顶层主 provider 的密钥**环境变量名**(可选;`login --default` 设它)。用于从 env 或
    /// `~/.ridge/auth.json` 密钥库取顶层 key,而不必把明文写进 config。解析顺序见 `real_provider`。
    pub key_env: Option<String>,
    pub budget_tokens: Option<usize>,
    pub skills_dir: Option<String>,
    /// 自定义斜杠命令目录(iter-39):`<dir>/*.md` 各成 `/名字`;缺 → env 或 `~/.ridge/commands`。
    pub commands_dir: Option<String>,
    pub skip_danger: Option<bool>,
    /// 输入框下方自定义状态条模板(可选)。占位:`{provider}{model}{ctx}{tokens}{cwd}`。
    /// 留空则用内置默认模板(见 `tui::DEFAULT_STATUS_BAR`)。
    pub status_bar: Option<String>,
    /// 地址越狱(iter-34):true 则放行 cwd 子树外的写。默认 false;开启 TUI 状态栏标红。
    /// **只放宽 cwd 子树** —— 危险命令拦截/受保护路径/只读不受影响。
    pub allow_jailbreak: Option<bool>,
    /// 要并接的多个 MCP(stdio)服务器。
    pub mcp: Vec<McpServerCfg>,
    /// 命名的 provider 档案(多 provider)—— `/provider use <name>` 可热切换到其中之一。
    pub providers: Vec<ProviderProfile>,
    /// 自定义 Hook(iter-40):事件触发点跑一条 shell,可拦截。见 [`HookCfg`]。
    pub hooks: Vec<HookCfg>,
    /// 任务完成通知(iter-40 内置 hook):true 则每个任务毕响一声终端铃。默认关。
    pub notify: Option<bool>,
    /// 外置沙箱包裹(iter-46):配了则 `run_shell` 经它跑,真隔离交平台(docker/wsl/自定义)。
    /// 模板,`{cwd}` 占位当前工作目录;user_cmd 作最后单个 arg 追加(免二次 shell 引号)。
    /// 例:`"docker run --rm -v {cwd}:/w -w /w alpine sh -c"`。留空 = 宿主直跑(现状)。
    pub sandbox_cmd: Option<String>,
}

/// 一个 Hook(iter-40):某事件发生时跑一条 shell 命令,可选拦截。像 git hooks —— 命令是**用户自己**
/// config 里写的(其机器其配置)。命令运行时注入 env `RIDGE_TOOL`(工具名)/`RIDGE_TOOL_ARG`(主参数)。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct HookCfg {
    /// `pre_tool` | `post_tool` | `session_start` | `stop`。
    pub event: String,
    /// 工具名匹配子串(仅 `*_tool` 事件;缺/空 = 匹配所有工具)。
    #[serde(default)]
    pub matcher: Option<String>,
    /// 要跑的 shell 命令。
    pub command: String,
    /// 仅 `pre_tool`:命令**非 0 退出**则**拦下该工具**(BLOCKED,不执行)。
    #[serde(default)]
    pub blocking: Option<bool>,
}

/// 一个要并接的 MCP 服务器(stdio):可执行文件 + 参数 + 命名空间名。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct McpServerCfg {
    pub name: String,
    pub cmd: String,
    #[serde(default)]
    pub args: Vec<String>,
}

/// 一个命名的 provider 档案:厂商类型 + 模型 + 端点 + 密钥来源。
/// 密钥两种给法:①(**推荐**)`key_env` 指一个**环境变量名**,用时从环境读,不落盘;
/// ②(便捷,自担风险)`api_key` 直接**内联明文**写在 config 里。二者皆有则 `api_key` 优先。
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ProviderProfile {
    pub name: String,
    /// `openai`(兼容端点)| `anthropic`。
    pub kind: String,
    pub model: String,
    pub base_url: String,
    /// 读该 provider 密钥的环境变量名,默认 `RIDGE_API_KEY`。
    #[serde(default = "default_key_env")]
    pub key_env: String,
    /// 内联明文密钥(可选)。**明文存盘,自担风险**;优先于 `key_env`。
    /// `skip_serializing` —— 任何回写 config 的路径(如 `/provider add`)都**不会**把它写出去。
    #[serde(default, skip_serializing)]
    pub api_key: Option<String>,
}

fn default_key_env() -> String {
    "RIDGE_API_KEY".to_string()
}

impl ProviderProfile {
    /// 解析本档案的密钥:内联 `api_key`(非空)优先,否则从 `key_env` 命名的环境变量读。
    /// 都取不到 → `None`(该档案不可用于真实启动)。
    pub fn resolve_key(&self) -> Option<String> {
        self.resolve_key_with(&std::collections::BTreeMap::new())
    }

    /// 同 [`resolve_key`],但把 `~/.ridge/auth.json` 密钥库(`login` 存的)纳入解析:
    /// 内联 `api_key` > env[key_env] > `auth[key_env]`。auth 传空表即退化为纯 env 行为。
    pub fn resolve_key_with(
        &self,
        auth: &std::collections::BTreeMap<String, String>,
    ) -> Option<String> {
        self.api_key
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| resolve_key_env(&self.key_env, auth))
    }
}

/// 按环境变量名取密钥:先读进程 env(非空即用,让用户可临时覆盖),否则回落
/// `~/.ridge/auth.json` 密钥库。空名 / 都无 → `None`。纯函数(env 由调用点决定是否隔离)。
pub fn resolve_key_env(
    name: &str,
    auth: &std::collections::BTreeMap<String, String>,
) -> Option<String> {
    if name.is_empty() {
        return None;
    }
    std::env::var(name)
        .ok()
        .filter(|k| !k.is_empty())
        .or_else(|| auth.get(name).cloned().filter(|k| !k.is_empty()))
}

/// 顶层「主 provider」的 key 解析(iter-41 收敛 —— 原 `real_provider` 前 3 档与 `current_api_key`
/// 各实现一遍,发散风险):`RIDGE_API_KEY` env → 顶层内联 `api_key`(非空)→ 顶层 `key_env`→(env 或
/// auth.json 密钥库,`login --default` 情形)。都无 → None。`providers[]` 档解析另见 `resolve_key_with`。
pub fn resolve_top_level_key(
    cfg: &Config,
    auth: &std::collections::BTreeMap<String, String>,
) -> Option<String> {
    if let Some(k) = std::env::var("RIDGE_API_KEY")
        .ok()
        .filter(|k| !k.is_empty())
    {
        return Some(k);
    }
    if let Some(k) = cfg
        .api_key
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return Some(k);
    }
    cfg.key_env
        .as_deref()
        .and_then(|name| resolve_key_env(name, auth))
}

impl Config {
    /// 从 JSON 文本解析;**解析失败 → 默认空配置**(降级到 env,不崩)。
    pub fn parse(text: &str) -> Self {
        serde_json::from_str(text).unwrap_or_default()
    }

    /// 从路径加载(读不到 → 默认空配置)。
    pub fn load(path: impl AsRef<std::path::Path>) -> Self {
        std::fs::read_to_string(path)
            .map(|t| Self::parse(&t))
            .unwrap_or_default()
    }
}

/// 交互中可 `/config set` 持久化的标量键白名单。
/// **不含** `mcp`(结构化,直接编辑文件)与任何密钥(密钥只走 `RIDGE_API_KEY` env)。
pub const CONFIG_KEYS: &[&str] = &[
    "provider",
    "model",
    "base_url",
    "budget_tokens",
    "skills_dir",
    "skip_danger",
    "status_bar",
    "allow_jailbreak",
];

/// 把一个标量键写进 JSON 配置文本,**保留其余键**(如 `mcp`),返回美化后的新文本。
/// 文本空/坏 → 从空对象起。类型按 key 归一:`budget_tokens`→number、`skip_danger`→bool、其余→string。
/// 供 REPL 的 `/config set` 用 —— 写盘由调用方做,这里只做纯文本变换(可单测)。
pub fn config_set(text: &str, key: &str, value: &str) -> Result<String, String> {
    if !CONFIG_KEYS.contains(&key) {
        return Err(format!("未知配置键 {key};可设:{}", CONFIG_KEYS.join(", ")));
    }
    let mut root = match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    let v = match key {
        "budget_tokens" => {
            let n: u64 = value
                .parse()
                .map_err(|_| format!("budget_tokens 需要非负整数,得到 {value}"))?;
            serde_json::Value::from(n)
        }
        "skip_danger" | "allow_jailbreak" => {
            let b: bool = value
                .parse()
                .map_err(|_| format!("{key} 需要 true/false,得到 {value}"))?;
            serde_json::Value::from(b)
        }
        _ => serde_json::Value::from(value),
    };
    root.insert(key.to_string(), v);
    serde_json::to_string_pretty(&serde_json::Value::Object(root)).map_err(|e| e.to_string())
}

/// 往 JSON 配置文本的 `providers` 数组加/覆盖一个 provider 档案(按 `name` 去重),**保留其余键**。
/// 文本空/坏 → 从空对象起。纯变换,可单测;写盘由调用方做。供 REPL 的 `/provider add` 用。
pub fn config_add_provider(text: &str, profile: &ProviderProfile) -> Result<String, String> {
    let mut root = match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    let entry = serde_json::to_value(profile).map_err(|e| e.to_string())?;
    let arr = root
        .entry("providers")
        .or_insert_with(|| serde_json::Value::Array(vec![]));
    let serde_json::Value::Array(list) = arr else {
        return Err("config 里 providers 不是数组".into());
    };
    // 同名覆盖,否则追加。
    match list
        .iter_mut()
        .find(|p| p.get("name").and_then(|n| n.as_str()) == Some(profile.name.as_str()))
    {
        Some(slot) => *slot = entry,
        None => list.push(entry),
    }
    serde_json::to_string_pretty(&serde_json::Value::Object(root)).map_err(|e| e.to_string())
}

/// 解析 `/provider add` 的定位参数 → [`ProviderProfile`](纯函数,可单测)。
/// 语法:`<name> <kind> <model> <base_url> [key_env]`;kind ∈ {openai, anthropic}。
/// 缺参 / 未知 kind → `Err`(用法提示)。**密钥不在此给** —— 只记 `key_env` 指向,
/// 明文永不因本路径落盘(`api_key=None` 且 [`ProviderProfile::api_key`] 本就 `skip_serializing`)。
pub fn parse_provider_add(args: &str) -> Result<ProviderProfile, String> {
    let f: Vec<&str> = args.split_whitespace().collect();
    if f.len() < 4 {
        return Err(
            "用法: /provider add <name> <kind:openai|anthropic> <model> <base_url> [key_env]"
                .into(),
        );
    }
    let kind = f[1].to_lowercase();
    if kind != "openai" && kind != "anthropic" {
        return Err(format!("未知 kind「{}」,只支持 openai | anthropic", f[1]));
    }
    Ok(ProviderProfile {
        name: f[0].to_string(),
        kind,
        model: f[2].to_string(),
        base_url: f[3].to_string(),
        key_env: f
            .get(4)
            .map(|s| s.to_string())
            .unwrap_or_else(default_key_env),
        api_key: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::*;

    /// config.json:解析含 2 个 `mcp` 的配置 → 2 个 server + provider 设置(CONTRACT-10 P2 验收)。
    #[test]
    fn config_parses_two_mcp_and_provider() {
        let cfg = Config::parse(
            r#"
            {
              "provider": "openai",
              "model": "glm-4.5-air",
              "budget_tokens": 50000,
              "skip_danger": true,
              "mcp": [
                { "name": "nlm", "cmd": "notebooklm-mcp.exe" },
                { "name": "fs", "cmd": "fs-server", "args": ["--root", "/tmp"] }
              ]
            }
        "#,
        );
        assert_eq!(cfg.provider.as_deref(), Some("openai"));
        assert_eq!(cfg.model.as_deref(), Some("glm-4.5-air"));
        assert_eq!(cfg.budget_tokens, Some(50000));
        assert_eq!(cfg.skip_danger, Some(true));
        assert_eq!(cfg.mcp.len(), 2);
        assert_eq!(cfg.mcp[0].name, "nlm");
        assert_eq!(cfg.mcp[1].cmd, "fs-server");
        assert_eq!(cfg.mcp[1].args, vec!["--root", "/tmp"]);
    }

    /// 坏 JSON / 缺文件 → 降级到默认空配置(不崩,回落 env)。
    #[test]
    fn config_bad_json_degrades_to_default() {
        let cfg = Config::parse("这不是合法 json {{{");
        assert!(cfg.provider.is_none() && cfg.mcp.is_empty());
        let missing = Config::load("C:/no/such/ridge-config-xyz.json");
        assert!(missing.mcp.is_empty());
    }

    /// `/config set` 的纯文本变换:改标量键、保留 `mcp`、类型归一、拒绝未知键 —— 且回写能被再解析。
    #[test]
    fn config_set_updates_scalar_keeps_mcp() {
        let start = r#"{ "model": "old", "mcp": [ { "name": "nlm", "cmd": "x.exe" } ] }"#;
        // 改 model → 保留 mcp。
        let s = config_set(start, "model", "glm-4.6").unwrap();
        let cfg = Config::parse(&s);
        assert_eq!(cfg.model.as_deref(), Some("glm-4.6"));
        assert_eq!(cfg.mcp.len(), 1);
        // 类型归一:budget_tokens→数字、skip_danger→bool。
        let s = config_set(&s, "budget_tokens", "80000").unwrap();
        let s = config_set(&s, "skip_danger", "true").unwrap();
        let cfg = Config::parse(&s);
        assert_eq!(cfg.budget_tokens, Some(80000));
        assert_eq!(cfg.skip_danger, Some(true));
        assert_eq!(cfg.mcp.len(), 1); // 一路保留
                                      // 空文本 → 从空对象起,仍写得进。
        assert!(config_set("", "provider", "openai").is_ok());
        // 未知键 / 坏类型 → Err(不写坏文件)。
        assert!(config_set(start, "api_key", "sk-x").is_err());
        assert!(config_set(start, "budget_tokens", "abc").is_err());
    }

    /// `/provider add` 的纯文本变换:追加 provider 档案、同名覆盖、保留 `mcp`、密钥不落盘。
    #[test]
    fn config_add_provider_appends_and_upserts() {
        let prof = |name: &str, model: &str| ProviderProfile {
            name: name.into(),
            kind: "openai".into(),
            model: model.into(),
            base_url: "https://x/v1".into(),
            key_env: "ZHIPU_KEY".into(),
            api_key: None,
        };
        let start = r#"{ "model": "old", "mcp": [ { "name": "nlm", "cmd": "x.exe" } ] }"#;
        // 追加第一个 → mcp 保留、providers 出现。
        let s = config_add_provider(start, &prof("glm", "glm-4.6")).unwrap();
        let cfg = Config::parse(&s);
        assert_eq!(cfg.mcp.len(), 1);
        assert_eq!(cfg.providers.len(), 1);
        assert_eq!(cfg.providers[0].name, "glm");
        assert_eq!(cfg.providers[0].key_env, "ZHIPU_KEY"); // 只存 env 名,不存密钥本身
                                                           // 追加第二个不同名 → 2 个。
        let s = config_add_provider(&s, &prof("kimi", "k2")).unwrap();
        // 同名覆盖 glm 的 model → 仍 2 个,glm 更新。
        let s = config_add_provider(&s, &prof("glm", "glm-4.7")).unwrap();
        let cfg = Config::parse(&s);
        assert_eq!(cfg.providers.len(), 2);
        let glm = cfg.providers.iter().find(|p| p.name == "glm").unwrap();
        assert_eq!(glm.model, "glm-4.7");
        // 缺省 key_env 反序列化为 RIDGE_API_KEY。
        let d = Config::parse(
            r#"{ "providers": [ { "name": "a", "kind": "openai", "model": "m", "base_url": "u" } ] }"#,
        );
        assert_eq!(d.providers[0].key_env, "RIDGE_API_KEY");
    }

    /// `/provider add` 参数解析:合法定位参数 → 档案;缺参/未知 kind → Err;经 config_add_provider
    /// 往返后明文密钥永不落盘。
    #[test]
    fn parse_provider_add_ok_bad_and_no_plaintext() {
        let p = parse_provider_add("mine openai gpt-4o https://api.x.com/v1").unwrap();
        assert_eq!(p.name, "mine");
        assert_eq!(p.kind, "openai");
        assert_eq!(p.model, "gpt-4o");
        assert_eq!(p.base_url, "https://api.x.com/v1");
        assert_eq!(p.key_env, "RIDGE_API_KEY"); // 缺省
        assert!(p.api_key.is_none());
        // 显式 key_env + kind 大小写不敏感。
        let p2 = parse_provider_add("m2 Anthropic claude https://a.com/v1 MY_KEY").unwrap();
        assert_eq!(p2.kind, "anthropic");
        assert_eq!(p2.key_env, "MY_KEY");
        // 缺参、未知 kind → Err。
        assert!(parse_provider_add("mine openai").is_err());
        assert!(parse_provider_add("mine grok model url").is_err());
        // 往返:providers 含该档、api_key 键不出现。
        let out = config_add_provider("{}", &p).unwrap();
        assert!(out.contains("\"mine\""));
        assert!(!out.contains("api_key"));
    }

    /// 密钥解析:内联 `api_key`(非空)优先于 `key_env`;`api_key` 不回写 config(skip_serializing)。
    #[test]
    fn provider_profile_resolve_key_precedence() {
        // 内联 api_key 直接可用,无需任何环境变量。
        let inline = Config::parse(
            r#"{ "providers": [ { "name": "z", "kind": "openai", "model": "m", "base_url": "u", "key_env": "NOPE_UNSET_ENV", "api_key": "  sk-inline  " } ] }"#,
        );
        assert_eq!(
            inline.providers[0].resolve_key().as_deref(),
            Some("sk-inline")
        ); // trim
           // 序列化(如 /provider add 回写)绝不含 api_key。
        let dumped = serde_json::to_string(&inline.providers[0]).unwrap();
        assert!(!dumped.contains("sk-inline") && !dumped.contains("api_key"));
        // 无 api_key 且 key_env 指向未设变量 → None。
        let none = Config::parse(
            r#"{ "providers": [ { "name": "z", "kind": "openai", "model": "m", "base_url": "u", "key_env": "DEFINITELY_UNSET_XYZ" } ] }"#,
        );
        assert_eq!(none.providers[0].resolve_key(), None);
    }

    // ───────────────────── iter-37:preset 表 + auth 密钥库 + login 纯核 ─────────────────────

    /// iter-34:`allow_jailbreak` 是可持久化 bool 配置键。
    #[test]
    fn config_set_accepts_allow_jailbreak_bool() {
        let out = config_set("{}", "allow_jailbreak", "true").unwrap();
        assert!(out.contains("\"allow_jailbreak\": true"), "得到: {out}");
        assert!(
            config_set("{}", "allow_jailbreak", "yes").is_err(),
            "非 bool 应报错"
        );
    }
}
