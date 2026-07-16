//! # agent —— 跑在 [`langgraph`] 引擎上的最小编码 agent
//!
//! 把「loop engineering」的核心结构落成一张图:
//!
//! ```text
//!   START ─▶ reason ──(action=finish 或 到达回合上限)──▶ verify ──(approved / 到顶)──▶ END
//!             ▲   │                                          │
//!             │   └────────(其它 action)────▶ act ──────────┘(未过 → 回 reason)
//!             └──────────────────(reflection loop)───────────
//! ```
//!
//! 三个原则(见 `docs/REPORT-langgraph-rust.md`):
//! - **maker ≠ checker**:`reason`/`act` 生成,`verify` 独立判定,不让生成者给自己打分。
//! - **确定性验证**:`verify` 只认工具输出里的客观信号(测试是否 passed),不认模型自述。
//! - **停机是设计的一半**:硬回合上限 `MAX_STEPS` + approved 闸门,双保险防跑飞。
//!
//! `Brain` 是接真实 LLM 的接缝;这里给一个离线 `ScriptedBrain`,零联网即可跑通闭环。
//!
//! 富文本输出支持:
//! - 彩色格式化输出（ANSI 颜色）
//! - 表格和结构化展示
//! - 图片/视频/文件路径的直接展示
//! - 交互式输出增强

use std::collections::{BTreeSet, HashMap};
use std::convert::Infallible;
use std::sync::Arc;

use langgraph::{CompiledGraph, GraphError, GraphState, StateGraph, END};
use mcp::McpClient;
use provider::{CompletionRequest, LlmProvider, Message, Role, ToolCall, ToolSpec};

/// 富文本输出层(彩色 / 表格 / 媒体展示)—— 见 [`rich_output`]。
mod rich_output;
pub use rich_output::{
    Color, Formatter, MediaDisplay, MediaInfo, MediaType, RichOutput, TableDisplay,
};

/// 到达此回合数强制收尾 —— 成本 / 防死循环护栏。
pub const MAX_STEPS: usize = 8;

/// 一条任务清单项(像 Claude Code 的 TodoWrite):`status` ∈ `pending` / `in_progress` / `completed`。
#[derive(Clone, Debug, PartialEq)]
pub struct Todo {
    pub content: String,
    pub status: String,
}

/// agent 的共享状态。`messages` 是事件轨迹(reducer 追加),其余字段覆盖。
#[derive(Clone, Debug, Default)]
pub struct AgentState {
    pub task: String,
    pub messages: Vec<String>,
    pub last_action: Option<String>,
    pub tool_output: Option<String>,
    pub approved: bool,
    pub steps: usize,
    pub issues: Vec<String>,
    /// 由 reason 节点(真实 LLM 路径)产出、待 act 节点执行的结构化工具调用。
    pub pending_call: Option<ToolCall>,
    /// 累计消耗的 token(成本记账)。
    pub total_tokens: usize,
    /// token 预算(0 = 不限)。超了就熔断停机。
    pub budget_tokens: usize,
    /// 连续「无进展」轮数(工具输出与上一轮相同)。到 [`MAX_STALL`] 就熔断。
    pub stall: usize,
    /// **模型面向**的多轮对话历史(system 之外的部分):user / assistant(可带 tool_calls)/ tool 结果。
    /// 这是发给 provider 的真身;REPL 跨轮携带它实现多轮上下文。
    pub history: Vec<Message>,
    /// 当前任务清单(模型经 `todo_write` 维护),REPL 渲染成 `[x]/[~]/[ ]` 给用户看进度。
    pub todos: Vec<Todo>,
    /// **Durable State(持久化事实)**:本次任务已成功改动的文件路径。用 `BTreeSet` 保证**有序稳态**
    /// —— 编进 prompt 事实块时字节稳定,不抖动、利 Claude 缓存。体量 O(去重文件数),不随步数膨胀。
    pub modified_files: BTreeSet<String>,
    /// **Durable State**:上一次工具调用的核心错误摘要(去噪后首行)。事实块据它「重锚定」模型注意力,
    /// 免其在被压缩的模糊历史里遗忘卡在哪。成功时清空。
    pub last_error: Option<String>,
}

impl AgentState {
    pub fn new(task: impl Into<String>) -> Self {
        let task = task.into();
        Self {
            history: vec![Message::user(task.clone())],
            task,
            ..Default::default()
        }
    }

    /// 设 token 预算(loop engineering 的经济护栏之一)。
    pub fn with_budget(mut self, tokens: usize) -> Self {
        self.budget_tokens = tokens;
        self
    }

    /// 用已有对话历史续跑(REPL 多轮携带上下文)。
    pub fn with_history(mut self, history: Vec<Message>) -> Self {
        self.history = history;
        self
    }
}

/// 节点产出的增量更新(delta)。`Batch` 让一个节点一次改多个字段。
#[derive(Debug)]
pub enum Patch {
    Message(String),
    Action(Option<String>),
    ToolOutput(Option<String>),
    Approved(bool),
    Issues(Vec<String>),
    PendingCall(Option<ToolCall>),
    AddTokens(usize),
    SetStall(usize),
    PushHistory(Message),
    SetTodos(Vec<Todo>),
    RecordModified(String),
    SetLastError(Option<String>),
    BumpStep,
    Batch(Vec<Patch>),
}

impl GraphState for AgentState {
    type Update = Patch;
    fn apply(&mut self, u: Patch) {
        match u {
            Patch::Message(m) => self.messages.push(m), // append reducer
            Patch::Action(a) => self.last_action = a,
            Patch::ToolOutput(o) => self.tool_output = o,
            Patch::Approved(b) => self.approved = b,
            Patch::Issues(v) => self.issues = v,
            Patch::PendingCall(c) => self.pending_call = c,
            Patch::AddTokens(n) => self.total_tokens += n,
            Patch::SetStall(n) => self.stall = n,
            Patch::PushHistory(m) => self.history.push(m),
            Patch::SetTodos(t) => self.todos = t,
            Patch::RecordModified(p) => {
                self.modified_files.insert(p);
            }
            Patch::SetLastError(e) => self.last_error = e,
            Patch::BumpStep => self.steps += 1,
            Patch::Batch(v) => v.into_iter().for_each(|p| self.apply(p)),
        }
    }
}

/// 连续无进展多少轮就熔断(no-progress detection)。
pub const MAX_STALL: usize = 3;

/// 权限门:执行**有副作用的**工具(shell / 写文件 / MCP)前征询批准(human-in-the-loop)。
/// REPL 用 stdin y/n;测试用 [`AutoApprove`] / [`AutoDeny`]。`read_file` 等只读工具不走它。
pub trait Approver: Send + Sync {
    fn approve(&self, action: &str, detail: &str) -> bool;
}

/// 一律放行(默认;非交互 / 一次性任务用)。
pub struct AutoApprove;
impl Approver for AutoApprove {
    fn approve(&self, _action: &str, _detail: &str) -> bool {
        true
    }
}

/// 一律拒绝(测试用)。
pub struct AutoDeny;
impl Approver for AutoDeny {
    fn approve(&self, _action: &str, _detail: &str) -> bool {
        false
    }
}

/// 只读工具不需要批准(read_file / search 只读本地;web_search / fetch_url 只读公共网页;
/// todo_write 只更新内部清单,无外部副作用)。
fn needs_approval(tool: &str) -> bool {
    !matches!(
        tool,
        "read_file" | "search" | "web_search" | "fetch_url" | "todo_write"
    )
}

/// web_search 的观察结果:懒探测一次网络环境(缓存)→ 选引擎 → 搜 → 排版给模型看。
/// `fetch`/`net` 从 [`build_core`] 注入,测试可用假抓取器。
async fn web_search_obs(
    fetch: &dyn provider::search::WebFetch,
    net: &std::sync::OnceLock<provider::search::NetEnv>,
    call: &ToolCall,
) -> String {
    use provider::search::{detect_net, engine_for, web_search, NetEnv};
    let query = call
        .arguments
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if query.is_empty() {
        return "web_search error: 缺少 query".to_string();
    }
    // 网络环境只探一次,整个会话复用(ponytail: 进程级缓存;网络切换需重启)。
    let env = match net.get() {
        Some(e) => *e,
        None => {
            let e = detect_net(fetch).await;
            let _ = net.set(e);
            e
        }
    };
    let label = match env {
        NetEnv::International => "直连国际",
        NetEnv::Restricted => "受限(GFW 内)",
    };
    match web_search(fetch, query, env).await {
        Ok(rs) if rs.is_empty() => format!("网络:{label} · 引擎:{} · (无结果)", engine_for(env)),
        Ok(rs) => {
            let mut s = format!("网络:{label} · 引擎:{}\n", engine_for(env));
            for (i, r) in rs.iter().enumerate() {
                s.push_str(&format!(
                    "{}. {} — {}\n   {}\n",
                    i + 1,
                    r.title,
                    r.url,
                    r.snippet
                ));
            }
            s
        }
        Err(e) => format!("web_search error: {e}"),
    }
}

/// fetch_url 的观察结果:抓网页正文喂给模型(RAG 闭环的「读」)。`fetch` 从 [`build_core`] 注入。
async fn fetch_url_obs(fetch: &dyn provider::search::WebFetch, call: &ToolCall) -> String {
    let url = call
        .arguments
        .get("url")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if url.is_empty() {
        return "fetch_url error: 缺少 url".to_string();
    }
    match provider::search::fetch_url(fetch, url).await {
        Ok(text) if text.is_empty() => format!("(空正文) {url}"),
        Ok(text) => format!("正文 {url}:\n{text}"),
        Err(e) => format!("fetch_url error: {e}"),
    }
}

/// 给权限门一个**人类可读的预览**,而非生糊 JSON:让用户「看着 diff 批准」而非盲批。
/// edit_file → `-/+` diff;write_file → 路径+规模;run_shell → 命令原文;其余回落到参数。
pub fn preview_call(call: &ToolCall) -> String {
    let arg = |k: &str| call.arguments.get(k).and_then(|v| v.as_str()).unwrap_or("");
    match call.name.as_str() {
        "edit_file" => {
            let minus: String = arg("old_string")
                .lines()
                .map(|l| format!("\n    - {l}"))
                .collect();
            let plus: String = arg("new_string")
                .lines()
                .map(|l| format!("\n    + {l}"))
                .collect();
            format!("{}{}{}", arg("path"), minus, plus)
        }
        "write_file" => {
            let c = arg("contents");
            format!(
                "{} ({} 行, {} 字节)",
                arg("path"),
                c.lines().count(),
                c.len()
            )
        }
        "apply_edits" => {
            let edits = parse_edits(call);
            format!(
                "批量编辑 {} 处:\n{}",
                edits.len(),
                tools::edits_diff(&edits)
            )
        }
        "run_shell" => arg("cmd").to_string(),
        _ => call.arguments.to_string(),
    }
}

/// 声明式技能(知识层):一份 `SKILL.md` = 某领域的知识/行为,注入 system prompt,
/// 让 agent 做**编程以外**的事(做饭/日程/电商/调研)而不改 Rust 源码 —— 模块化框架的核心。
#[derive(Clone, Debug, PartialEq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub body: String,
}

/// 扫描一个技能目录(`<dir>/<skill>/SKILL.md`),解析成 [`Skill`] 列表。目录不存在 → 空。
pub fn load_skills(dir: impl AsRef<std::path::Path>) -> Vec<Skill> {
    let mut skills = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return skills;
    };
    for entry in entries.flatten() {
        let path = entry.path().join("SKILL.md");
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Some(s) = parse_skill(&text) {
                skills.push(s);
            }
        }
    }
    skills
}

/// 解析 `SKILL.md`:YAML frontmatter(`name` / `description`)+ 正文。无 name → 无效。
fn parse_skill(text: &str) -> Option<Skill> {
    let rest = text.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    let front = &rest[..end];
    let body = rest[end + 4..]
        .trim_start_matches(['-', '\n'])
        .trim()
        .to_string();
    let (mut name, mut description) = (String::new(), String::new());
    for line in front.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("name:") {
            name = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("description:") {
            description = v.trim().to_string();
        }
    }
    (!name.is_empty()).then_some(Skill {
        name,
        description,
        body,
    })
}

/// 一个 sub-agent 定义(带 frontmatter 的 `.md`):独立上下文、**只读**、可指定便宜模型。
/// 主 agent 通过 `dispatch_agent` 工具派活给它,或 REPL `/agent` 手动派;它只回精炼结论,省主上下文/token。
#[derive(Clone, Debug)]
pub struct Agent {
    pub name: String,
    pub description: String,
    /// 引用 config.providers 里的档案名(如 `fast`);省略 → 用主 provider。
    pub provider: Option<String>,
    /// 只读工具白名单(`read_file` / `search`);省略 → 给全部只读工具。
    pub tools: Option<Vec<String>>,
    /// 正文 = 该 sub-agent 的 system prompt。
    pub body: String,
}

/// 解析 agent 定义 `.md`:frontmatter(name/description/provider/tools)+ 正文。无 name → 无效。
/// (刻意与 [`parse_skill`] 分开,不动那条已测路径;多解析 provider/tools 两字段。)
fn parse_agent(text: &str) -> Option<Agent> {
    let rest = text.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    let front = &rest[..end];
    let body = rest[end + 4..]
        .trim_start_matches(['-', '\n'])
        .trim()
        .to_string();
    let (mut name, mut description, mut provider, mut tools) =
        (String::new(), String::new(), None, None);
    for line in front.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("name:") {
            name = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("description:") {
            description = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("provider:") {
            let v = v.trim();
            if !v.is_empty() {
                provider = Some(v.to_string());
            }
        } else if let Some(v) = line.strip_prefix("tools:") {
            let list: Vec<String> = v
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if !list.is_empty() {
                tools = Some(list);
            }
        }
    }
    (!name.is_empty()).then_some(Agent {
        name,
        description,
        provider,
        tools,
        body,
    })
}

/// 扫描扁平目录 `<dir>/*.md` 解析成 agent 定义列表。目录不存在 → 空。
pub fn load_agents(dir: impl AsRef<std::path::Path>) -> Vec<Agent> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("md") {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Some(a) = parse_agent(&text) {
                    out.push(a);
                }
            }
        }
    }
    out
}

/// 内置 agent / skill(编进二进制;用户放同名文件即可覆盖)。
const BUILTIN_AGENTS: &[&str] = &[
    include_str!("builtin/agents/fastcontext.md"),
    include_str!("builtin/agents/explorer.md"),
    include_str!("builtin/agents/reviewer.md"),
];
const BUILTIN_SKILLS: &[&str] = &[
    include_str!("builtin/skills/agent-creator.md"),
    include_str!("builtin/skills/skill-creator.md"),
];

/// 内置 agent 定义(fastcontext / explorer / reviewer)。
pub fn builtin_agents() -> Vec<Agent> {
    BUILTIN_AGENTS
        .iter()
        .filter_map(|t| parse_agent(t))
        .collect()
}

/// 内置 skill(agent-creator / skill-creator:教主 agent 自建 agent/skill)。
pub fn builtin_skills() -> Vec<Skill> {
    BUILTIN_SKILLS
        .iter()
        .filter_map(|t| parse_skill(t))
        .collect()
}

/// 读 cwd 的项目规则文件(CLAUDE.md / AGENTS.md),拼成一个"技能"注入 system prompt。都不存在 → None。
/// 不向上递归(YAGNI):只看当前工作目录。
pub fn load_project_rules() -> Option<Skill> {
    let mut body = String::new();
    for f in ["CLAUDE.md", "AGENTS.md"] {
        if let Ok(t) = std::fs::read_to_string(f) {
            if !t.trim().is_empty() {
                body.push_str(&format!("\n<!-- {f} -->\n{}\n", t.trim()));
            }
        }
    }
    (!body.is_empty()).then(|| Skill {
        name: "项目规则".to_string(),
        description: "本仓库的 CLAUDE.md / AGENTS.md 约定,须遵守".to_string(),
        body,
    })
}

/// sub-agent 注册表:定义列表 + 命名 provider(name → 已建 provider)。
#[derive(Default)]
pub struct Agents {
    pub defs: Vec<Agent>,
    pub providers: HashMap<String, Arc<dyn LlmProvider>>,
}

/// sub-agent 允许的**只读**工具(不下放写/改/shell,免绕过主 agent 的权限门)。
const READONLY_TOOLS: &[&str] = &["read_file", "search"];

/// sub-agent 步数上限(只读检索,不需要很多轮)。
const SUBAGENT_MAX_STEPS: usize = 8;

/// 按白名单裁出 sub-agent 可用的只读工具 spec。`allow=None` → 全部只读工具。
fn readonly_tool_specs(allow: &Option<Vec<String>>) -> Vec<ToolSpec> {
    builtin_tool_specs()
        .into_iter()
        .filter(|s| READONLY_TOOLS.contains(&s.name.as_str()))
        .filter(|s| {
            allow
                .as_ref()
                .is_none_or(|a| a.iter().any(|t| t == &s.name))
        })
        .collect()
}

/// 跑一个**只读** sub-agent:独立 system(=定义正文)+ 只读工具,自成一轮 reason-act 循环,
/// 返回它的最终结论文本(不回灌工具轨迹到主上下文 —— 这正是省 token 的关键)。
/// ponytail: 只读(read_file/search),要写让主 agent 写;放开写权限需接权限门。
pub async fn run_subagent(def: &Agent, provider: Arc<dyn LlmProvider>, task: &str) -> String {
    let system = format!(
        "你是 '{}' sub-agent。{}\n\n{}\n\n你是**只读**的:只能用 read_file / search 搜集信息,不能改文件或跑命令。查完后用纯文本回一个精炼结论。",
        def.name, def.description, def.body
    );
    let tools = readonly_tool_specs(&def.tools);
    let mut history: Vec<Message> = vec![Message::user(task.to_string())];
    for _ in 0..SUBAGENT_MAX_STEPS {
        let mut msgs = vec![Message::new(Role::System, system.clone())];
        msgs.extend(history.iter().cloned());
        let req = CompletionRequest {
            messages: msgs,
            tools: tools.clone(),
        };
        let completion = match provider.complete(&req).await {
            Ok(c) => c,
            Err(e) => return format!("[{} 出错: {e}]", def.name),
        };
        match completion.tool_calls.into_iter().next() {
            Some(call) => {
                // 深度防御:即便模型幻觉调了非只读工具,也挡下,绝不执行副作用工具。
                let obs = if READONLY_TOOLS.contains(&call.name.as_str()) {
                    execute_tool_call(&call)
                } else {
                    format!("sub-agent 无权调用 {}(只读)", call.name)
                };
                history
                    .push(Message::assistant(completion.text).with_tool_calls(vec![call.clone()]));
                history.push(Message::tool_result(call.id.clone(), obs));
            }
            None => return completion.text,
        }
    }
    format!("[{} 达到步数上限,未收敛]", def.name)
}

/// `dispatch_agent` 工具 spec(仅在有 agent 定义时暴露)。让主 agent 自主把只读子任务派出去。
fn dispatch_spec(agents: &Agents) -> Option<ToolSpec> {
    if agents.defs.is_empty() {
        return None;
    }
    let names: Vec<String> = agents.defs.iter().map(|a| a.name.clone()).collect();
    let list = agents
        .defs
        .iter()
        .map(|a| format!("- {}: {}", a.name, a.description))
        .collect::<Vec<_>>()
        .join("\n");
    Some(ToolSpec {
        name: "dispatch_agent".to_string(),
        description: format!(
            "把一个**只读**子任务(检索/探索/审查)派给专职 sub-agent:独立上下文,只回精炼结论,替你省上下文与 token。可用 agent:\n{list}"
        ),
        schema: serde_json::json!({
            "type":"object",
            "properties":{
                "agent":{"type":"string","enum":names},
                "task":{"type":"string","description":"交给该 sub-agent 的具体只读子任务"}
            },
            "required":["agent","task"]
        }),
    })
}

/// 执行 `dispatch_agent`:选 provider(定义指定的档案,缺则主 provider)→ 跑 sub-agent → 回结论。
async fn dispatch_obs(agents: &Agents, main: &Arc<dyn LlmProvider>, call: &ToolCall) -> String {
    let name = call
        .arguments
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let task = call
        .arguments
        .get("task")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let Some(def) = agents.defs.iter().find(|a| a.name == name) else {
        return format!("没有名为 {name} 的 sub-agent(dispatch_agent 的 enum 里选)");
    };
    let provider = def
        .provider
        .as_ref()
        .and_then(|p| agents.providers.get(p))
        .cloned()
        .unwrap_or_else(|| main.clone());
    let out = run_subagent(def, provider, task).await;
    format!("[sub-agent {name} 的结论]\n{out}")
}

/// `~/.ridge/config.json`:一处配 provider/model/预算/多 MCP/skills(env 仍可覆盖)。
/// **密钥不进 config**(明文风险)—— API key 只从 `RIDGE_API_KEY` env 读。
#[derive(Debug, Default, Clone, serde::Deserialize)]
#[serde(default)]
pub struct Config {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub budget_tokens: Option<usize>,
    pub skills_dir: Option<String>,
    pub skip_danger: Option<bool>,
    /// 要并接的多个 MCP(stdio)服务器。
    pub mcp: Vec<McpServerCfg>,
    /// 命名的 provider 档案(多 provider)—— `/provider use <name>` 可热切换到其中之一。
    pub providers: Vec<ProviderProfile>,
}

/// 一个要并接的 MCP 服务器(stdio):可执行文件 + 参数 + 命名空间名。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct McpServerCfg {
    pub name: String,
    pub cmd: String,
    #[serde(default)]
    pub args: Vec<String>,
}

/// 一个命名的 provider 档案:厂商类型 + 模型 + 端点 + **读密钥的 env 变量名**。
/// **密钥不进 config**(明文风险)—— 只存要读的 env 名(`key_env`),用时才从环境变量取。
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
}

fn default_key_env() -> String {
    "RIDGE_API_KEY".to_string()
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
        "skip_danger" => {
            let b: bool = value
                .parse()
                .map_err(|_| format!("skip_danger 需要 true/false,得到 {value}"))?;
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

/// 通用 agent 的基础 system prompt(不再只面向编码)。
const BASE_SYSTEM: &str = "You are a capable agent. Use the provided tools to accomplish the \
     user's task. To change existing files, prefer edit_file (surgical, unique-match replace) over \
     rewriting the whole file with write_file; use search and ranged read_file to explore before \
     editing. For external/real-time info, web_search to find links then fetch_url to read the \
     actual page — trust the page text, not just the snippet. When there is an objective way to \
     verify (compiler exit code, tests), rely on it and don't trust your own claim. \
     Reply concisely: no filler or restating the task; when changing code, emit only the minimal \
     edit (unique-match replace / diff), not a full-file rewrite. When done, stop.";

/// 把技能注入 system prompt(知识层 → 大脑偏好)。
fn build_system_prompt(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return BASE_SYSTEM.to_string();
    }
    let mut s = String::from(BASE_SYSTEM);
    s.push_str("\n\n# Skills — domain knowledge to apply\n");
    for sk in skills {
        s.push_str(&format!(
            "\n## {} — {}\n{}\n",
            sk.name, sk.description, sk.body
        ));
    }
    s
}

/// 超预算?(0 预算 = 不限)
fn over_budget(s: &AgentState) -> bool {
    s.budget_tokens > 0 && s.total_tokens >= s.budget_tokens
}

/// 陷入僵局?(连续 MAX_STALL 轮工具输出没变)
fn stalled(s: &AgentState) -> bool {
    s.stall >= MAX_STALL
}

/// 确定性**成功**信号(编码任务:shell `exit 0` 或测试 `passed`)。
fn tool_output_ok(o: &str) -> bool {
    o.contains("exit 0") || (o.contains("passed") && !o.contains("failed"))
}

/// 确定性**失败**信号(编译/测试出错、非 0 退出、被拦截/拒绝)。
fn tool_output_failed(o: &str) -> bool {
    o.contains("failed")
        || o.contains("error")
        || o.contains("BLOCKED")
        || o.contains("permission denied")
        || (o.starts_with("exit ") && !o.starts_with("exit 0"))
}

/// verify 判据(通用 agent):
/// - 有确定性成功信号(编码任务)→ 通过;
/// - **模型 finish 且没有失败信号**(开放式/信息类任务,如调 MCP 查数据)→ 接受完成,不空转到回合上限。
///
/// 编码任务仍严格卡 `exit 0`;只对「模型自己收尾且无客观失败」放行,兼顾通用性与 maker≠checker。
fn verify_ok(s: &AgentState) -> bool {
    let out = s.tool_output.as_deref();
    out.is_some_and(tool_output_ok)
        || (s.last_action.as_deref() == Some("finish") && !out.is_some_and(tool_output_failed))
}

/// 多层独立退出:到回合上限 / 超预算 / 无进展任一命中,循环都该停(loop engineering:停机是设计的一半)。
fn must_stop(s: &AgentState) -> bool {
    s.steps >= MAX_STEPS || over_budget(s) || stalled(s)
}

/// reason 之后的路由(scripted / llm 两条路径共用):finish 或需停机 → verify,否则 → act。
fn reason_route(s: &AgentState) -> Vec<String> {
    if must_stop(s) {
        return vec!["verify".to_string()];
    }
    match s.last_action.as_deref() {
        Some("finish") => vec!["verify".to_string()],
        _ => vec!["act".to_string()],
    }
}

/// verify 之后的路由(共用):通过或需停机 → END,否则回 reason。
fn verify_route(s: &AgentState) -> Vec<String> {
    if s.approved || must_stop(s) {
        vec![END.to_string()]
    } else {
        vec!["reason".to_string()]
    }
}

/// 决策大脑(maker 的一半):看状态,给出下一步动作名或 `"finish"`。
/// 这是接真实 LLM provider 的接缝 —— 换实现即可,图不用动。
pub trait Brain: Send + Sync + 'static {
    fn decide(&self, state: &AgentState) -> String;
}

/// 离线脚本大脑:按工具反馈推进(写代码 → 修复 → 完成),用于零联网跑通闭环 / 测试。
pub struct ScriptedBrain;

impl Brain for ScriptedBrain {
    fn decide(&self, s: &AgentState) -> String {
        match s.tool_output.as_deref() {
            None => "write_code".to_string(),
            Some(o) if o.contains("failed") => "fix".to_string(),
            Some(o) if o.contains("passed") => "finish".to_string(),
            _ => "finish".to_string(),
        }
    }
}

/// 工具执行器(act 节点用)。真实场景后面换成 MCP / shell / 编译器。
pub type Tool = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// 便捷构造:脚本大脑。
pub fn scripted() -> Arc<dyn Brain> {
    Arc::new(ScriptedBrain)
}

/// 便捷构造:默认离线工具 —— 模拟「写代码先挂、修复后通过」的客观验证信号。
pub fn default_tool() -> Tool {
    Arc::new(|action: &str| match action {
        "write_code" => "tests: 1 failed".to_string(),
        "fix" => "tests: passed".to_string(),
        other => format!("unknown tool `{other}`"),
    })
}

/// 便捷构造:**真实** shell 工具(M1 物理闭环)—— 把 action 当命令跑,返回退出码 + 输出。
/// 这是 act 节点触碰真实世界的接缝。⚠ 无沙箱,只喂受控命令。
pub fn shell_tool() -> Tool {
    Arc::new(|action: &str| match tools::run_shell(action) {
        Ok(r) => format!("exit {}: {}{}", r.code, r.stdout.trim(), r.stderr.trim()),
        Err(e) => format!("shell error: {e}"),
    })
}

/// 把 agent 装配成一张编译好的 langgraph 图。
pub fn build_agent(
    brain: Arc<dyn Brain>,
    tool: Tool,
) -> Result<CompiledGraph<AgentState>, GraphError> {
    let mut g = StateGraph::<AgentState>::new();

    // reason:推进一个回合,问大脑要下一步动作。
    let brain_c = brain.clone();
    g.add_node("reason", move |s: AgentState| {
        let brain = brain_c.clone();
        async move {
            let action = brain.decide(&s);
            let msg = format!("reason#{}: -> {action}", s.steps + 1);
            Ok::<_, Infallible>(Patch::Batch(vec![
                Patch::BumpStep,
                Patch::Message(msg),
                Patch::Action(Some(action)),
            ]))
        }
    });

    // act:执行工具,把客观输出写回状态。
    let tool_c = tool.clone();
    g.add_node("act", move |s: AgentState| {
        let tool = tool_c.clone();
        async move {
            let action = s.last_action.clone().unwrap_or_default();
            let out = (tool.as_ref())(&action);
            Ok::<_, Infallible>(Patch::Batch(vec![
                Patch::Message(format!("act: {action} -> {out}")),
                Patch::ToolOutput(Some(out)),
            ]))
        }
    });

    g.add_node("verify", verify_node);

    g.set_entry("reason");
    g.add_conditional_edge("reason", reason_route);
    g.add_edge("act", "reason"); // 反思环:工具跑完回 reason 复盘
    g.add_conditional_edge("verify", verify_route);

    g.compile()
}

/// verify 节点(scripted / llm 两条路径共用):独立 checker,按 [`verify_ok`] 判定。
async fn verify_node(s: AgentState) -> Result<Patch, Infallible> {
    let ok = verify_ok(&s);
    let patch = if ok {
        Patch::Batch(vec![
            Patch::Approved(true),
            Patch::Message("verify: PASS (deterministic gate)".to_string()),
        ])
    } else {
        Patch::Batch(vec![
            Patch::Approved(false),
            Patch::Issues(vec!["build/tests not passing".to_string()]),
            Patch::Message("verify: FAIL -> back to reason".to_string()),
        ])
    };
    Ok(patch)
}

/// 内置工具的规格(喂给 LLM 让它按 schema 出结构化 tool_call)。
pub fn builtin_tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "run_shell".to_string(),
            description: "运行一条 shell 命令,返回退出码与输出".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"cmd":{"type":"string"}},"required":["cmd"]}),
        },
        ToolSpec {
            name: "write_file".to_string(),
            description: "把内容整文件写入路径(覆盖)。仅用于**新建文件**;改动已有文件请用 edit_file".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"},"contents":{"type":"string"}},"required":["path","contents"]}),
        },
        ToolSpec {
            name: "edit_file".to_string(),
            description: "精准编辑:把文件里**唯一**出现的 old_string 换成 new_string(需带足够上下文保证唯一)。改动已有文件优先用它,而非整文件覆写".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"},"old_string":{"type":"string"},"new_string":{"type":"string"}},"required":["path","old_string","new_string"]}),
        },
        ToolSpec {
            name: "apply_edits".to_string(),
            description: "**跨文件批量**精准编辑:多处 {path, old_string, new_string} 汇总一份 diff 一次确认、**原子应用**(全成或全不改)。重构/多文件改动用它".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"edits":{"type":"array","items":{"type":"object","properties":{"path":{"type":"string"},"old_string":{"type":"string"},"new_string":{"type":"string"}},"required":["path","old_string","new_string"]}}},"required":["edits"]}),
        },
        ToolSpec {
            name: "read_file".to_string(),
            description: "读取文件。可选 offset(起始行,1 起)+ limit(行数)只读一段,大文件别整读".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"},"offset":{"type":"integer"},"limit":{"type":"integer"}},"required":["path"]}),
        },
        ToolSpec {
            name: "search".to_string(),
            description: "在目录树下按文件名 glob(如 *.rs)搜含 pattern 子串的行,返回 路径:行号:内容。找代码/定位用它,别 run_shell grep(不可移植)".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"},"glob":{"type":"string"}},"required":["pattern"]}),
        },
        ToolSpec {
            name: "web_search".to_string(),
            description: "联网搜索,返回标题/链接/摘要(自动按网络环境选可用引擎)。查实时信息或外部资料用它;query 会发给外部搜索引擎".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}),
        },
        ToolSpec {
            name: "fetch_url".to_string(),
            description: "抓取一个网页并返回**可读正文**(去脚本/样式/标签)。配合 web_search:先搜到链接,再用它读正文、据原文作答,别只凭摘要猜".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}),
        },
        ToolSpec {
            name: "todo_write".to_string(),
            description: "维护任务清单:把计划拆成若干 {content, status}。**多步/复杂任务**开始时列清单、每完成一步更新其状态给用户看进度;简单单步不必用".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"todos":{"type":"array","items":{"type":"object","properties":{"content":{"type":"string"},"status":{"type":"string","enum":["pending","in_progress","completed"]}},"required":["content","status"]}}},"required":["todos"]}),
        },
    ]
}

/// 从 `apply_edits` 的参数里抽出 `edits` 数组 → [`tools::Edit`] 列表(字段缺失→跳过)。
fn parse_edits(call: &ToolCall) -> Vec<tools::Edit> {
    let Some(arr) = call.arguments.get("edits").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|e| {
            let s = |k: &str| e.get(k).and_then(|v| v.as_str());
            Some(tools::Edit::new(
                s("path")?,
                s("old_string")?,
                s("new_string")?,
            ))
        })
        .collect()
}

/// 从一次工具调用 + 观察结果里,**确定性地**抽出 Durable State 更新(事实驱动回填):
/// 观察到工具错误(前缀 ` error:` / `BLOCKED` / `permission denied`)→ 置 `last_error` 首行;
/// 写类工具成功(write_file/edit_file/apply_edits)→ 记入 `modified_files` 并清 `last_error`;
/// 其余工具不动 durable 状态。这样长任务只凭「当前事实」推理,不必靠全量历史。
fn durable_updates(call: &ToolCall, observation: &str) -> Vec<Patch> {
    let is_err = observation.contains(" error:")
        || observation.starts_with("BLOCKED")
        || observation.starts_with("permission denied");
    if is_err {
        let line = observation
            .lines()
            .next()
            .unwrap_or(observation)
            .to_string();
        return vec![Patch::SetLastError(Some(line))];
    }
    let arg = |k: &str| call.arguments.get(k).and_then(|v| v.as_str());
    match call.name.as_str() {
        "write_file" | "edit_file" => arg("path")
            .map(|p| {
                vec![
                    Patch::RecordModified(p.to_string()),
                    Patch::SetLastError(None),
                ]
            })
            .unwrap_or_default(),
        "apply_edits" => {
            let mut ps: Vec<Patch> = parse_edits(call)
                .into_iter()
                .map(|e| Patch::RecordModified(e.path))
                .collect();
            if !ps.is_empty() {
                ps.push(Patch::SetLastError(None));
            }
            ps
        }
        _ => Vec::new(),
    }
}

/// 从 `todo_write` 的参数里抽出 `todos` 数组 → [`Todo`] 列表(status 缺省 `pending`)。
fn parse_todos(call: &ToolCall) -> Vec<Todo> {
    let Some(arr) = call.arguments.get("todos").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|t| {
            let content = t.get("content").and_then(|v| v.as_str())?.to_string();
            let status = t
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("pending")
                .to_string();
            Some(Todo { content, status })
        })
        .collect()
}

/// 写操作沙箱守卫:路径须落在**进程 cwd 子树**内(`--cwd` 设的工作目录),越狱 → `Err(BLOCKED 串)`。
/// 深度防御,与危险命令拦截同层:即使模型幻觉出绝对路径/`..` 逃逸,也硬拒,防写出工作目录祸害宿主。
fn jail(path: &str) -> Result<(), String> {
    let root = std::env::current_dir().map_err(|e| format!("BLOCKED (jail): 取 cwd 失败: {e}"))?;
    tools::jail_path(&root, path)
        .map(|_| ())
        .map_err(|e| format!("BLOCKED (jail): {e}"))
}

/// 执行一个结构化工具调用,返回给模型看的观察结果(observation)。用真实的 `tools` crate 干活。
pub fn execute_tool_call(call: &ToolCall) -> String {
    let arg = |k: &str| call.arguments.get(k).and_then(|v| v.as_str()).unwrap_or("");
    match call.name.as_str() {
        "run_shell" => {
            let cmd = arg("cmd");
            // 危险命令拦截:即使用户批准也拒绝(无沙箱阶段的安全硬门槛)。
            if let Some(why) = tools::is_dangerous_command(cmd) {
                return format!("BLOCKED (dangerous: {why}) —— 拒绝执行 `{cmd}`");
            }
            match tools::run_shell(cmd) {
                Ok(r) => format!("exit {}: {}{}", r.code, r.stdout.trim(), r.stderr.trim()),
                Err(e) => format!("shell error: {e}"),
            }
        }
        "write_file" => {
            if let Err(e) = jail(arg("path")) {
                return e;
            }
            let contents = arg("contents");
            match tools::write_file(arg("path"), contents) {
                Ok(()) => format!("wrote {} bytes to {}", contents.len(), arg("path")),
                Err(e) => format!("write error: {e}"),
            }
        }
        "edit_file" => {
            if let Err(e) = jail(arg("path")) {
                return e;
            }
            match tools::edit_file(arg("path"), arg("old_string"), arg("new_string")) {
                Ok(()) => format!("edited {}", arg("path")),
                Err(e) => format!("edit error: {e}"),
            }
        }
        "apply_edits" => {
            let edits = parse_edits(call);
            if edits.is_empty() {
                return "apply_edits error: 缺少 edits".to_string();
            }
            // 沙箱:任一路径越狱 → 整批拒(与 apply_edits 的原子性一致,不留半成品)。
            for e in &edits {
                if let Err(msg) = jail(&e.path) {
                    return msg;
                }
            }
            match tools::apply_edits(&edits) {
                Ok(n) => format!("applied {n} 个文件的批量编辑"),
                Err(e) => format!("apply_edits error: {e}"),
            }
        }
        "read_file" => {
            let num = |k: &str| call.arguments.get(k).and_then(|v| v.as_u64());
            let (off, lim) = (num("offset"), num("limit"));
            let res = if off.is_some() || lim.is_some() {
                tools::read_file_range(
                    arg("path"),
                    off.unwrap_or(1).max(1) as usize,
                    lim.unwrap_or(2000) as usize,
                )
            } else {
                tools::read_file(arg("path"))
            };
            match res {
                Ok(c) => c,
                Err(e) => format!("read error: {e}"),
            }
        }
        "search" => {
            let or = |k: &str, d: &'static str| {
                let v = arg(k);
                if v.is_empty() {
                    d.to_string()
                } else {
                    v.to_string()
                }
            };
            match tools::search(or("path", "."), arg("pattern"), &or("glob", "*")) {
                Ok(s) if s.is_empty() => "(no matches)".to_string(),
                Ok(s) => s,
                Err(e) => format!("search error: {e}"),
            }
        }
        // 状态更新在 act 节点(发 SetTodos patch);这里只回个观察摘要。
        "todo_write" => format!("已更新任务清单:{} 项", parse_todos(call).len()),
        other => format!("unknown tool `{other}`"),
    }
}

/// 把当前状态铺成给 provider 的消息序列:system(含注入的技能)+ **真实多轮 history**
/// (user / assistant(带 tool_calls) / role=tool 结果),而非把轨迹当 assistant 文本糊上去。
/// history 的**估算 token** 总量超过这么多,发给 LLM 前**自动** compact —— 把 O(n) 全量历史
/// 收敛成有界快照(Runtime State:模型只需知「现在什么情况」,不需知全部「聊过什么」)。此前压缩仅
/// `/compact` 手动;长任务多轮下历史随步数膨胀、爆预算 + 击穿 prompt 缓存。
/// 按**内容体量**触发(而非条数):一条万字日志 ≫ 二十条短问答,条数触发会漏。
/// ponytail: [`est_tokens`] 是本地启发式估算,真实计数(tiktoken)是外置能力不进内核;阈值是可调校准旋钮。
/// 注意:加权触发改善「多条中等消息」的判准;「少数超大单条消息」仍需**单条内容截断**(属外置 squeez 域)。
const AUTO_COMPACT_TOKENS: usize = 6000;
/// 自动 compact 时保留的最近消息条数。
const AUTO_COMPACT_KEEP: usize = 8;

/// 本地 token 估算(不引 tiktoken):CJK 等非 ASCII 字 ≈ 1 token/字,ASCII ≈ 1 token/4 字符。
/// 口径同仓内 `bin`/`token-count.mjs`。粗但零依赖、确定可测 —— 只用于「要不要压缩」的触发判断。
fn est_tokens(text: &str) -> usize {
    let (mut cjk, mut ascii) = (0usize, 0usize);
    for c in text.chars() {
        if c.is_ascii() {
            ascii += 1;
        } else {
            cjk += 1;
        }
    }
    cjk + ascii / 4
}

fn to_messages(system: &str, s: &AgentState) -> Vec<Message> {
    let weight: usize = s.history.iter().map(|m| est_tokens(&m.content)).sum();
    let history = if weight > AUTO_COMPACT_TOKENS {
        compact_history(s.history.clone(), AUTO_COMPACT_KEEP)
    } else {
        s.history.clone()
    };
    let mut msgs = vec![Message::new(Role::System, system)];
    msgs.extend(history);
    // Durable State 事实块放**末尾**(不进冻结的 system prompt):保首部前缀稳定利 Claude 缓存,
    // 又把模型注意力「重锚定」到当前客观事实。仅在有事实时注入,免空噪。
    if let Some(block) = durable_state_block(s) {
        msgs.push(Message::new(Role::System, block));
    }
    msgs
}

/// 把 Durable State 编译成一段紧凑事实块(已改文件 / 上次报错);无事实 → `None`(不注入)。
/// 体量 O(去重文件数 + 一条报错),**不随步数膨胀** —— 这是「事实驱动而非消息驱动」的 O(1) 关键。
fn durable_state_block(s: &AgentState) -> Option<String> {
    if s.modified_files.is_empty() && s.last_error.is_none() {
        return None;
    }
    let mut b = String::from("<durable_state>\n");
    if !s.modified_files.is_empty() {
        let files: Vec<&str> = s.modified_files.iter().map(String::as_str).collect();
        b.push_str(&format!("已改文件: {}\n", files.join(", ")));
    }
    if let Some(e) = &s.last_error {
        b.push_str(&format!("上次报错: {e}\n"));
    }
    b.push_str("</durable_state>");
    Some(b)
}

/// 已连好的 MCP 工具:暴露给 LLM 的 [`ToolSpec`] + 「命名空间名 → (客户端, 原始工具名)」路由表。
#[derive(Default)]
pub struct McpTools {
    specs: Vec<ToolSpec>,
    router: HashMap<String, (Arc<McpClient>, String)>,
}

impl McpTools {
    pub fn empty() -> Self {
        Self::default()
    }

    /// 已接入的 MCP 工具名(命名空间形式,如 `nlm__notebook_list`)。供 `/tools` 列举。
    pub fn tool_names(&self) -> Vec<String> {
        self.specs.iter().map(|s| s.name.clone()).collect()
    }
}

/// 连上一批 MCP 客户端:各自 initialize + list_tools,把工具归一化成 [`ToolSpec`](命名空间)+ 建路由表。
/// **降级不崩**:单个服务器连不上/列不出工具 → 跳过,其余照常。
pub async fn resolve_mcp(clients: Vec<Arc<McpClient>>) -> McpTools {
    let mut out = McpTools::empty();
    for client in clients {
        if client.initialize().await.is_err() {
            continue;
        }
        let Ok(tools) = client.list_tools().await else {
            continue;
        };
        for t in tools {
            let ns = client.namespaced(&t.name);
            out.specs.push(ToolSpec {
                name: ns.clone(),
                description: t.description,
                schema: t.input_schema,
            });
            out.router.insert(ns, (client.clone(), t.name));
        }
    }
    out
}

/// 单个 `@file` 注入的正文上限(超出截断,防爆上下文)。
const MENTION_CAP: usize = 20_000;

/// 展开输入里的 `@path` 引用(像 Claude Code):把每个**存在的**文件正文注入进消息,
/// 让模型直接看到文件内容而不必自己 read_file。不存在的 `@xxx` 原样留着(模型当普通文本看)。
/// ponytail: 路径 = `@` 后一串非空白(去尾部标点);同一路径只注一次;单文件截断到 [`MENTION_CAP`]。
pub fn expand_mentions(input: &str) -> String {
    let mut extra = String::new();
    let mut seen = std::collections::HashSet::new();
    for token in input.split_whitespace() {
        let Some(raw) = token.strip_prefix('@') else {
            continue;
        };
        let path = raw.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '，', '。']);
        if path.is_empty() || !seen.insert(path.to_string()) {
            continue;
        }
        if let Ok(mut content) = std::fs::read_to_string(path) {
            if content.chars().count() > MENTION_CAP {
                content = content.chars().take(MENTION_CAP).collect::<String>() + "\n…(截断)";
            }
            extra.push_str(&format!("\n\n[文件 @{path}]:\n{content}"));
        }
    }
    if extra.is_empty() {
        input.to_string()
    } else {
        format!("{input}{extra}")
    }
}

/// 把任务清单渲染成彩色 checklist(供 REPL 显示进度):完成 `[x]` 绿、进行中 `[~]` 黄、待办 `[ ]`。
/// 空清单 → 空串。
pub fn render_todos(todos: &[Todo]) -> String {
    if todos.is_empty() {
        return String::new();
    }
    let mut s = RichOutput::new()
        .with_color(Color::BrightCyan)
        .bold()
        .format("📋 任务清单:");
    for t in todos {
        let (mark, color) = match t.status.as_str() {
            "completed" => ("[x]", Color::Green),
            "in_progress" => ("[~]", Color::Yellow),
            _ => ("[ ]", Color::White),
        };
        s.push('\n');
        s.push_str(
            &RichOutput::new()
                .with_color(color)
                .format(&format!("  {mark} {}", t.content)),
        );
    }
    s
}

/// 流式 token 总线:REPL 每回合把一个 sender 塞进来,reason 节点边收 provider 的增量文本
/// 边往里发,REPL 侧就能**逐字显示**(像 Claude Code)。`None` = 该回合不流式。
pub type TokenBus = Arc<std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<String>>>>;

/// 一个「永不流式」的空总线(测试 / 非交互装配用)。
pub fn null_token_bus() -> TokenBus {
    Arc::new(std::sync::Mutex::new(None))
}

/// 用**真实 LLM provider** 装配 agent 图(不接 MCP)。见 [`build_llm_agent_with`]。
pub fn build_llm_agent(
    provider: Arc<dyn LlmProvider>,
) -> Result<CompiledGraph<AgentState>, GraphError> {
    build_llm_agent_with(provider, McpTools::empty())
}

/// 装配 agent 图,并把 MCP 工具并入(确定性 verify,无独立模型复核,一律放行)。
pub fn build_llm_agent_with(
    provider: Arc<dyn LlmProvider>,
    mcp: McpTools,
) -> Result<CompiledGraph<AgentState>, GraphError> {
    build_core(
        provider,
        mcp,
        None,
        Arc::new(AutoApprove),
        Vec::new(),
        null_token_bus(),
        Arc::new(Agents::default()),
    )
}

/// 带**独立模型 checker** 的装配(M4,maker≠checker 的强形式):
/// 确定性 verify 通过后,再让一个**独立的** reviewer 模型复核有没有作弊(如删/跳过测试);
/// reviewer 打回则 approved=false、带 issue 回 reason。用**不同的** provider,别让写代码的模型自审。
pub fn build_llm_agent_reviewed(
    provider: Arc<dyn LlmProvider>,
    mcp: McpTools,
    reviewer: Arc<dyn LlmProvider>,
) -> Result<CompiledGraph<AgentState>, GraphError> {
    build_core(
        provider,
        mcp,
        Some(reviewer),
        Arc::new(AutoApprove),
        Vec::new(),
        null_token_bus(),
        Arc::new(Agents::default()),
    )
}

/// 带**权限门**的装配:有副作用的工具执行前过 [`Approver`](REPL 用它做 y/n 确认)。
pub fn build_llm_agent_gated(
    provider: Arc<dyn LlmProvider>,
    mcp: McpTools,
    approver: Arc<dyn Approver>,
) -> Result<CompiledGraph<AgentState>, GraphError> {
    build_core(
        provider,
        mcp,
        None,
        approver,
        Vec::new(),
        null_token_bus(),
        Arc::new(Agents::default()),
    )
}

/// **全装配**(模块化框架):MCP 工具 + 权限门 + 声明式 Skills + 流式 token 总线。CLI 用它。
/// `token_bus` 传 [`null_token_bus`] 即不流式;REPL 传真实总线以逐字显示。
#[allow(clippy::too_many_arguments)]
pub fn build_llm_agent_full(
    provider: Arc<dyn LlmProvider>,
    mcp: McpTools,
    approver: Arc<dyn Approver>,
    skills: Vec<Skill>,
    token_bus: TokenBus,
    agents: Arc<Agents>,
) -> Result<CompiledGraph<AgentState>, GraphError> {
    build_core(provider, mcp, None, approver, skills, token_bus, agents)
}

/// reason 把 内置 + MCP 工具一起 offer 给 LLM,act 按 `<server>__<tool>` 命名空间路由到对应
/// MCP 客户端(否则走内置工具),执行前过权限门,verify 认确定性信号(可选再挂独立模型 reviewer);
/// system prompt 注入 Skills(领域知识)。
fn build_core(
    provider: Arc<dyn LlmProvider>,
    mcp: McpTools,
    reviewer: Option<Arc<dyn LlmProvider>>,
    approver: Arc<dyn Approver>,
    skills: Vec<Skill>,
    token_bus: TokenBus,
    agents: Arc<Agents>,
) -> Result<CompiledGraph<AgentState>, GraphError> {
    let mut g = StateGraph::<AgentState>::new();
    let mut specs = builtin_tool_specs();
    if let Some(d) = dispatch_spec(&agents) {
        specs.push(d); // 有 sub-agent 才把 dispatch_agent 摆上桌
    }
    specs.extend(mcp.specs);
    let router = Arc::new(mcp.router);
    let system = Arc::new(build_system_prompt(&skills));

    let provider_c = provider.clone();
    let system_c = system.clone();
    g.add_node("reason", move |s: AgentState| {
        let provider = provider_c.clone();
        let tools = specs.clone();
        let system = system_c.clone();
        let bus = token_bus.clone();
        async move {
            let req = CompletionRequest {
                messages: to_messages(&system, &s),
                tools,
            };
            // 流式:provider 每吐一段文本就发进总线,REPL 侧逐字显示。无总线 sender 则等同整段。
            let on_token = move |t: String| {
                if let Some(tx) = bus.lock().unwrap().as_ref() {
                    let _ = tx.send(t);
                }
            };
            let completion = provider.complete_streaming(&req, &on_token).await?;
            let tokens = completion.usage.total() as usize; // 成本记账
            let asst_text = completion.text.clone();
            let patch = if let Some(call) = completion.tool_calls.into_iter().next() {
                // maker 想用工具 → 记 assistant(带 tool_calls)进 history,交给 act 执行。
                let hist = Message::assistant(asst_text).with_tool_calls(vec![call.clone()]);
                Patch::Batch(vec![
                    Patch::BumpStep,
                    Patch::AddTokens(tokens),
                    Patch::Message(format!(
                        "reason#{}: tool_call {} {}",
                        s.steps + 1,
                        call.name,
                        call.arguments
                    )),
                    Patch::PushHistory(hist),
                    Patch::PendingCall(Some(call)),
                    Patch::Action(Some("tool".to_string())),
                ])
            } else {
                // 模型给了最终文本,没有工具调用 → 收尾。
                Patch::Batch(vec![
                    Patch::BumpStep,
                    Patch::AddTokens(tokens),
                    Patch::Message(format!("reason#{}: (final) {}", s.steps + 1, asst_text)),
                    Patch::PushHistory(Message::assistant(asst_text)),
                    Patch::PendingCall(None),
                    Patch::Action(Some("finish".to_string())),
                ])
            };
            Ok::<_, provider::ProviderError>(patch)
        }
    });

    // web_search 依赖:真实抓取器 + 网络环境缓存(整会话懒探测一次)。
    let fetch: Arc<dyn provider::search::WebFetch> =
        Arc::new(provider::search::ReqwestFetch::new());
    let net: Arc<std::sync::OnceLock<provider::search::NetEnv>> =
        Arc::new(std::sync::OnceLock::new());

    let router_c = router.clone();
    let approver_c = approver.clone();
    let fetch_c = fetch.clone();
    let net_c = net.clone();
    let agents_c = agents.clone();
    let provider_act = provider.clone(); // 主 provider:sub-agent 未指定档案时的回落
    g.add_node("act", move |s: AgentState| {
        let router = router_c.clone();
        let approver = approver_c.clone();
        let fetch = fetch_c.clone();
        let net = net_c.clone();
        let agents = agents_c.clone();
        let main_provider = provider_act.clone();
        async move {
            let patch = match &s.pending_call {
                Some(call) => {
                    // 权限门:有副作用的工具执行前征询批准。
                    let obs = if needs_approval(&call.name)
                        && !approver.approve(&call.name, &preview_call(call))
                    {
                        format!("permission denied by user: {}", call.name)
                    } else if call.name == "dispatch_agent" {
                        dispatch_obs(&agents, &main_provider, call).await
                    } else if call.name == "web_search" {
                        web_search_obs(fetch.as_ref(), &net, call).await
                    } else if call.name == "fetch_url" {
                        fetch_url_obs(fetch.as_ref(), call).await
                    } else if let Some((client, raw)) = router.get(&call.name) {
                        // 命名空间命中 → 路由到 MCP 服务器。
                        match client.call_tool(raw, call.arguments.clone()).await {
                            Ok(t) => t,
                            Err(e) => format!("mcp error: {e}"),
                        }
                    } else {
                        execute_tool_call(call)
                    };
                    // 无进展检测:工具输出与上一轮相同则 stall+1,否则清零。
                    let stall = if s.tool_output.as_deref() == Some(obs.as_str()) {
                        s.stall + 1
                    } else {
                        0
                    };
                    // Durable State 回填(事实驱动):在 obs 被移动前算好。
                    let durable = durable_updates(call, &obs);
                    let mut patches = vec![
                        Patch::Message(format!("act: {} -> {}", call.name, obs)),
                        // 工具结果按 role=tool 正确回灌(匹配 tool_call_id)。
                        Patch::PushHistory(Message::tool_result(call.id.clone(), obs.clone())),
                        Patch::SetStall(stall),
                        Patch::ToolOutput(Some(obs)),
                        Patch::PendingCall(None),
                    ];
                    // todo_write:把清单写进状态(REPL 会渲染 [x]/[~]/[ ])。
                    if call.name == "todo_write" {
                        patches.push(Patch::SetTodos(parse_todos(call)));
                    }
                    patches.extend(durable); // 记已改文件 / 上次报错
                    Patch::Batch(patches)
                }
                None => Patch::Message("act: no pending tool_call".to_string()),
            };
            Ok::<_, Infallible>(patch)
        }
    });

    match reviewer {
        // 有独立 reviewer:确定性通过后再让它复核作弊。
        Some(rv) => {
            let rv_c = rv.clone();
            g.add_node("verify", move |s: AgentState| {
                let reviewer = rv_c.clone();
                async move {
                    let det_ok = verify_ok(&s);
                    if !det_ok {
                        return Ok::<_, provider::ProviderError>(Patch::Batch(vec![
                            Patch::Approved(false),
                            Patch::Issues(vec!["build/tests not passing".to_string()]),
                            Patch::Message(
                                "verify: FAIL (deterministic) -> back to reason".to_string(),
                            ),
                        ]));
                    }
                    // 独立模型复核:给它轨迹,问是否合法达成(没作弊)。
                    let verdict = reviewer.complete(&review_request(&s)).await?;
                    let approved =
                        verdict.text.contains("APPROVE") && !verdict.text.contains("REJECT");
                    let patch = if approved {
                        Patch::Batch(vec![
                            Patch::Approved(true),
                            Patch::Message(
                                "verify: PASS (deterministic + 独立 reviewer)".to_string(),
                            ),
                        ])
                    } else {
                        Patch::Batch(vec![
                            Patch::Approved(false),
                            Patch::Issues(vec![format!("reviewer 打回: {}", verdict.text)]),
                            Patch::Message(format!("verify: reviewer REJECT -> {}", verdict.text)),
                        ])
                    };
                    Ok(patch)
                }
            });
        }
        // 无 reviewer:纯确定性 verify。
        None => {
            g.add_node("verify", verify_node);
        }
    }

    g.set_entry("reason");
    g.add_conditional_edge("reason", reason_route);
    g.add_edge("act", "reason");
    g.add_conditional_edge("verify", verify_route);

    g.compile()
}

/// 给独立 reviewer 的复核请求:system 定角色 + user 附上 agent 的轨迹。
fn review_request(s: &AgentState) -> CompletionRequest {
    CompletionRequest {
        messages: vec![
            Message::new(
                Role::System,
                "You are an independent code reviewer. Given the agent's task and trajectory, \
                 decide whether it achieved the goal LEGITIMATELY or CHEATED (e.g. deleted/skipped \
                 failing tests, faked output). Reply exactly 'APPROVE' or 'REJECT: <reason>'.",
            ),
            Message::new(
                Role::User,
                format!("Task: {}\n\nTrajectory:\n{}", s.task, s.messages.join("\n")),
            ),
        ],
        tools: vec![],
    }
}

/// 规划器(M5 起步):让 provider 把一个目标拆成有序子任务(JSON 数组)。
/// 解析失败/模型出错 → **降级**为把整个目标当单个子任务(绝不返回空,循环有活干)。
///
/// 子任务本身可交给 [`build_llm_agent`] 逐个执行;彼此独立的还能靠引擎的 fan-out 并行跑。
pub async fn plan(provider: &dyn LlmProvider, task: &str) -> Vec<String> {
    let req = CompletionRequest {
        messages: vec![
            Message::new(
                Role::System,
                "Break the user's goal into 2-5 ordered, concrete subtasks. \
                 Reply ONLY a JSON array of strings, nothing else.",
            ),
            Message::new(Role::User, task.to_string()),
        ],
        tools: vec![],
    };
    let text = match provider.complete(&req).await {
        Ok(c) => c.text,
        Err(_) => return vec![task.to_string()],
    };
    parse_subtasks(&text).unwrap_or_else(|| vec![task.to_string()])
}

/// 上下文压缩(`/compact`,DoD②):历史太长时,保留**首条(原始任务)**+ 一条摘要标记 + **最近 `keep` 条**,
/// 其余压掉。防长会话「上下文腐烂」(Ralph 式,但用确定性截断,不烧一次 LLM)。
pub fn compact_history(history: Vec<Message>, keep: usize) -> Vec<Message> {
    if history.len() <= keep + 1 {
        return history;
    }
    // 保留窗口 = 末 keep 条。若窗口首条是 role=tool(其配对的 assistant 已被压进摘要),
    // 从前端裁掉这些悬空 tool 结果 —— 否则 OpenAI 兼容端点会因「tool 无前置 tool_calls」400。
    let mut recent = &history[history.len() - keep..];
    while recent.first().is_some_and(|m| m.role == Role::Tool) {
        recent = &recent[1..];
    }
    let dropped = history.len() - 1 - recent.len();
    let mut out = Vec::with_capacity(recent.len() + 2);
    out.push(history[0].clone()); // 原始任务
    out.push(Message::user(format!(
        "[上下文已压缩:省略 {dropped} 条早期消息]"
    )));
    out.extend(recent.iter().cloned());
    out
}

/// 写一轮的审计轨迹到 `trace.json`(DoD⑥:客观证据,含工具输出/退出码 + 多轮 history)。密钥不入 trace。
pub fn write_trace(out: &AgentState, path: impl AsRef<std::path::Path>) -> std::io::Result<()> {
    let record = serde_json::json!({
        "task": out.task,
        "approved": out.approved,
        "steps": out.steps,
        "tokens": out.total_tokens,
        "trace": out.messages,   // 人读轨迹(含 act 的 exit code / 工具输出)
        "history": out.history,  // 模型面向多轮(含 role=tool 结果)
    });
    let json = serde_json::to_string_pretty(&record).map_err(std::io::Error::other)?;
    std::fs::write(path, json)
}

/// 从模型输出里抠出首个 `[` 到末个 `]` 的 JSON 数组(容忍模型包裹的解释文字)。
fn parse_subtasks(text: &str) -> Option<Vec<String>> {
    let start = text.find('[')?;
    let end = text.rfind(']')?;
    let arr: Vec<String> = serde_json::from_str(text.get(start..=end)?).ok()?;
    (!arr.is_empty()).then_some(arr)
}

/// 一个子任务的执行结果。
#[derive(Clone, Debug)]
pub struct SubtaskResult {
    pub task: String,
    pub approved: bool,
    pub steps: usize,
    pub tokens: usize,
}

/// 规划-执行的聚合报告。
#[derive(Clone, Debug)]
pub struct PlanReport {
    pub subtasks: Vec<SubtaskResult>,
    /// 全部子任务都通过才算整体通过。
    pub approved: bool,
    pub total_tokens: usize,
    pub total_steps: usize,
}

/// **规划 + 执行**(orchestrator-workers,M5 完整版):
/// `planner`(通常是强模型)把目标拆成子任务,`worker` 逐个执行,聚合结果。
/// 成本杠杆:强模型只管规划,弱模型扛执行量(planner ≠ worker)。
///
/// 目前**串行**执行(子任务常有依赖);彼此独立的子任务可改用 `tokio::spawn` + `join_all`
/// 并行(引擎/运行时已支持),这里先要正确性。
pub async fn run_planned(
    planner: Arc<dyn LlmProvider>,
    worker: Arc<dyn LlmProvider>,
    task: &str,
) -> Result<PlanReport, GraphError> {
    let subtasks = plan(planner.as_ref(), task).await;
    let mut results = Vec::with_capacity(subtasks.len());
    let mut total_tokens = 0;
    let mut total_steps = 0;
    let mut approved = true;

    for sub in subtasks {
        let app = build_llm_agent(worker.clone())?;
        let out = app.invoke(AgentState::new(sub.clone())).await?;
        approved &= out.approved;
        total_tokens += out.total_tokens;
        total_steps += out.steps;
        results.push(SubtaskResult {
            task: sub,
            approved: out.approved,
            steps: out.steps,
            tokens: out.total_tokens,
        });
    }

    Ok(PlanReport {
        subtasks: results,
        approved,
        total_tokens,
        total_steps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use langgraph::RunConfig;

    #[test]
    fn parse_agent_reads_frontmatter_and_body() {
        let md = "---\nname: fc\ndescription: 检索\nprovider: fast\ntools: read_file, search\n---\n正文指令";
        let a = parse_agent(md).expect("应解析出 agent");
        assert_eq!(a.name, "fc");
        assert_eq!(a.provider.as_deref(), Some("fast"));
        assert_eq!(
            a.tools.as_deref(),
            Some(&["read_file".to_string(), "search".to_string()][..])
        );
        assert_eq!(a.body, "正文指令");
    }

    #[test]
    fn subagent_tools_are_readonly_never_side_effecting() {
        // 安全:sub-agent 工具集绝不含写/改/删/shell(免绕过主 agent 权限门)。
        let names: Vec<String> = readonly_tool_specs(&None)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert!(names.iter().any(|n| n == "read_file") && names.iter().any(|n| n == "search"));
        for forbidden in ["write_file", "edit_file", "apply_edits", "run_shell"] {
            assert!(
                !names.iter().any(|n| n == forbidden),
                "{forbidden} 不该给 sub-agent"
            );
        }
        // 白名单进一步收窄:只要 search。
        let only = readonly_tool_specs(&Some(vec!["search".to_string()]));
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].name, "search");
    }

    #[test]
    fn builtin_agents_parse_with_fast_context() {
        let a = builtin_agents();
        assert!(a
            .iter()
            .any(|x| x.name == "fastcontext" && x.provider.as_deref() == Some("fast")));
        assert!(a.iter().any(|x| x.name == "reviewer"));
    }

    #[tokio::test]
    async fn happy_path_converges_and_gets_approved() {
        let app = build_agent(scripted(), default_tool()).unwrap();
        let out = app
            .invoke(AgentState::new("make tests pass"))
            .await
            .unwrap();
        assert!(out.approved, "checker should approve once tests pass");
        assert_eq!(out.steps, 3, "write_code -> fix -> finish");
        assert!(out.messages.iter().any(|m| m.contains("verify: PASS")));
    }

    /// 大脑永不收工 + 工具永远失败:循环必须在回合上限处停机,而不是烧到天荒地老。
    #[tokio::test]
    async fn broken_loop_terminates_at_cap() {
        struct NeverDone;
        impl Brain for NeverDone {
            fn decide(&self, _s: &AgentState) -> String {
                "retry".to_string()
            }
        }
        let tool: Tool = Arc::new(|_a: &str| "tests: 1 failed".to_string());
        let app = build_agent(Arc::new(NeverDone), tool).unwrap();

        let out = app
            .invoke_with(
                AgentState::new("impossible"),
                &RunConfig::default(),
                None,
                None,
            )
            .await
            .unwrap();

        assert!(!out.approved, "must not fake success");
        assert_eq!(out.steps, MAX_STEPS, "hard cap stops the runaway loop");
    }

    /// P0 物理闭环:shell 工具把真实退出码带回来(0 vs 非 0),不再是脚本假信号。
    #[test]
    fn shell_tool_reflects_real_exit_code() {
        let tool = shell_tool();
        assert!((tool.as_ref())("exit 0").starts_with("exit 0:"));
        assert!((tool.as_ref())("exit 7").starts_with("exit 7:"));
    }

    /// P1:结构化 tool_call → 真实文件写入(物理副作用可验证)。
    #[test]
    fn execute_tool_call_writes_real_file() {
        // 沙箱后:写路径须在 cwd 子树内,故用 cwd 相对唯一名(非 temp_dir)。
        let path = std::env::current_dir()
            .unwrap()
            .join("ridge_llm_toolcall.tmp");
        let _ = std::fs::remove_file(&path);
        let call = ToolCall {
            id: "x".to_string(),
            name: "write_file".to_string(),
            arguments: serde_json::json!({"path": path.to_str().unwrap(), "contents": "physical closure"}),
        };
        let obs = execute_tool_call(&call);
        assert!(obs.contains("wrote"), "{obs}");
        assert_eq!(tools::read_file(&path).unwrap(), "physical closure");
        let _ = std::fs::remove_file(&path);
    }

    /// 驾驭工程:结构化 edit_file tool_call → 精准替换真实文件(而非整文件覆写)。
    #[test]
    fn execute_tool_call_edits_real_file() {
        let path = std::env::current_dir().unwrap().join("ridge_llm_edit.tmp");
        tools::write_file(&path, "let n = 1;\n").unwrap();
        let call = ToolCall {
            id: "e".to_string(),
            name: "edit_file".to_string(),
            arguments: serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "let n = 1;",
                "new_string": "let n = 99;"
            }),
        };
        let obs = execute_tool_call(&call);
        assert!(obs.starts_with("edited"), "{obs}");
        assert_eq!(tools::read_file(&path).unwrap(), "let n = 99;\n");
        let _ = std::fs::remove_file(&path);
    }

    /// 多文件批量编辑:一个 apply_edits 调用改 2 个文件,原子生效;preview 是一份汇总 diff。
    #[test]
    fn apply_edits_batches_multiple_files() {
        let dir = std::env::current_dir()
            .unwrap()
            .join("ridge_agent_batch_tmp");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.txt");
        let b = dir.join("b.txt");
        tools::write_file(&a, "one\n").unwrap();
        tools::write_file(&b, "two\n").unwrap();
        let call = ToolCall {
            id: "b".to_string(),
            name: "apply_edits".to_string(),
            arguments: serde_json::json!({"edits": [
                {"path": a.to_str().unwrap(), "old_string": "one", "new_string": "1"},
                {"path": b.to_str().unwrap(), "old_string": "two", "new_string": "2"},
            ]}),
        };
        // preview:一份汇总 diff,一次确认。
        let p = preview_call(&call);
        assert!(
            p.contains("批量编辑 2 处") && p.contains("- one") && p.contains("+ 2"),
            "{p}"
        );
        // 执行:两文件都改。
        let obs = execute_tool_call(&call);
        assert!(obs.contains("applied 2"), "{obs}");
        assert_eq!(tools::read_file(&a).unwrap(), "1\n");
        assert_eq!(tools::read_file(&b).unwrap(), "2\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 用户交互:权限门看到的是**diff 预览**而非生 JSON —— 用户看着改动批准。
    #[test]
    fn preview_call_renders_edit_diff() {
        let call = ToolCall {
            id: "p".to_string(),
            name: "edit_file".to_string(),
            arguments: serde_json::json!({
                "path": "src/x.rs", "old_string": "old", "new_string": "new"
            }),
        };
        let p = preview_call(&call);
        assert!(p.contains("src/x.rs"), "{p}");
        assert!(p.contains("- old") && p.contains("+ new"), "diff 形态: {p}");
    }

    /// 官方样例 skills 必须能被 load_skills 正确解析(守住 samples/ 不腐坏)。
    #[test]
    fn sample_skills_parse() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../samples/skills");
        let skills = load_skills(dir);
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        for expected in [
            "researcher",
            "rust-fixer",
            "summarize",
            "translate",
            "triage",
        ] {
            assert!(
                names.contains(&expected),
                "缺样例 skill {expected}: {names:?}"
            );
        }
        for s in &skills {
            assert!(
                !s.description.is_empty() && !s.body.is_empty(),
                "{}",
                s.name
            );
        }
    }

    /// todo_write:解析 todos + 渲染 checklist + 只读不走权限门。
    #[test]
    fn todo_write_parses_and_renders() {
        let call = ToolCall {
            id: "t".to_string(),
            name: "todo_write".to_string(),
            arguments: serde_json::json!({"todos": [
                {"content": "读代码", "status": "completed"},
                {"content": "改 bug", "status": "in_progress"},
                {"content": "跑测试", "status": "pending"},
            ]}),
        };
        let todos = parse_todos(&call);
        assert_eq!(todos.len(), 3);
        assert_eq!(todos[0].status, "completed");
        assert!(execute_tool_call(&call).contains("3 项"));
        assert!(!needs_approval("todo_write"), "内部清单更新不打扰用户");
        // 渲染:完成打 [x]、进行中 [~]、待办 [ ]。
        let r = render_todos(&todos);
        assert!(
            r.contains("[x] 读代码") && r.contains("[~] 改 bug") && r.contains("[ ] 跑测试"),
            "{r}"
        );
        assert!(render_todos(&[]).is_empty(), "空清单 → 空串");
    }

    /// `@file` 引用:存在的文件注入正文,不存在的原样留着。
    #[test]
    fn expand_mentions_injects_existing_files() {
        let mut path = std::env::temp_dir();
        path.push("ridge_mention_test.txt");
        std::fs::write(&path, "文件正文ABC").unwrap();
        let p = path.to_str().unwrap();
        let out = expand_mentions(&format!("看看 @{p} 说了什么,还有 @/no/such/file"));
        assert!(out.contains("文件正文ABC"), "应注入存在文件: {out}");
        assert!(out.contains(&format!("[文件 @{p}]")), "带来源标注: {out}");
        assert!(out.contains("@/no/such/file"), "不存在的原样留着");
        // 无 @ → 原样返回。
        assert_eq!(expand_mentions("普通输入"), "普通输入");
        let _ = std::fs::remove_file(&path);
    }

    /// 只读工具(read_file / search / web_search / fetch_url)不走权限门;有副作用的走。
    #[test]
    fn readonly_tools_skip_approval() {
        assert!(!needs_approval("read_file"));
        assert!(!needs_approval("search"));
        assert!(!needs_approval("web_search"));
        assert!(!needs_approval("fetch_url"));
        assert!(needs_approval("edit_file"));
        assert!(needs_approval("write_file"));
        assert!(needs_approval("run_shell"));
    }

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

    /// fetch_url:抓网页 → 抽正文喂模型(RAG 的「读」),走假抓取器不联网。
    #[tokio::test]
    async fn fetch_url_obs_returns_page_text() {
        use provider::search::WebFetch;
        struct Page;
        #[async_trait::async_trait]
        impl WebFetch for Page {
            async fn get_text(&self, _url: &str) -> Result<String, provider::ProviderError> {
                Ok("<body><script>x()</script><p>正文内容在此。</p></body>".to_string())
            }
        }
        let call = ToolCall {
            id: "f".to_string(),
            name: "fetch_url".to_string(),
            arguments: serde_json::json!({"url": "https://ex.com"}),
        };
        let obs = fetch_url_obs(&Page, &call).await;
        assert!(
            obs.contains("正文内容在此") && !obs.contains("x()"),
            "{obs}"
        );

        let bad = ToolCall {
            id: "f2".to_string(),
            name: "fetch_url".to_string(),
            arguments: serde_json::json!({}),
        };
        assert!(fetch_url_obs(&Page, &bad).await.contains("缺少 url"));
    }

    /// web_search:探测网络环境 → 选引擎 → 排版结果,全程走假抓取器(不联网)。
    #[tokio::test]
    async fn web_search_obs_detects_env_and_picks_engine() {
        use provider::search::{NetEnv, WebFetch};

        // 探针失败 → Restricted → 该用 bing-cn;返回一条结果。
        struct RestrictedFetch;
        #[async_trait::async_trait]
        impl WebFetch for RestrictedFetch {
            async fn get_text(&self, url: &str) -> Result<String, provider::ProviderError> {
                if url.contains("generate_204") {
                    return Err("blocked".into());
                }
                assert!(url.contains("bing"), "受限环境应打 bing,实际:{url}");
                Ok(r#"<li class="b_algo"><h2><a href="https://ex.com/">标题</a></h2><p>摘要文本</p></li>"#.to_string())
            }
        }
        let net = std::sync::OnceLock::new();
        let call = ToolCall {
            id: "w".to_string(),
            name: "web_search".to_string(),
            arguments: serde_json::json!({"query": "rust 教程"}),
        };
        let obs = web_search_obs(&RestrictedFetch, &net, &call).await;
        assert!(obs.contains("受限(GFW 内)"), "{obs}");
        assert!(obs.contains("bing-cn"), "{obs}");
        assert!(
            obs.contains("标题") && obs.contains("https://ex.com/"),
            "{obs}"
        );
        assert_eq!(net.get(), Some(&NetEnv::Restricted)); // 探测结果被缓存

        // 缺 query → 明确报错,不打网络。
        struct NeverFetch;
        #[async_trait::async_trait]
        impl WebFetch for NeverFetch {
            async fn get_text(&self, _url: &str) -> Result<String, provider::ProviderError> {
                panic!("不该联网");
            }
        }
        let bad = ToolCall {
            id: "w2".to_string(),
            name: "web_search".to_string(),
            arguments: serde_json::json!({}),
        };
        let obs = web_search_obs(&NeverFetch, &std::sync::OnceLock::new(), &bad).await;
        assert!(obs.contains("缺少 query"), "{obs}");
    }

    /// P1 端到端:provider 吐结构化 tool_call → act 调**真实** shell → verify 认真实 `exit 0` → approved。
    /// 用离线 ScriptedProvider 站位真实 LLM,零联网、确定性。
    #[tokio::test]
    async fn llm_agent_drives_real_tools_to_approved() {
        use provider::{Completion, ScriptedProvider};
        let scripted = ScriptedProvider::new(vec![
            // 第 1 轮:决定跑构建(真实 shell,exit 0 代表构建通过)。
            Completion {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "1".to_string(),
                    name: "run_shell".to_string(),
                    arguments: serde_json::json!({"cmd": "exit 0"}),
                }],
                ..Default::default()
            },
            // 第 2 轮:没有工具调用 → 收尾。
            Completion {
                text: "build is green, done".to_string(),
                tool_calls: vec![],
                ..Default::default()
            },
        ]);
        let app = build_llm_agent(Arc::new(scripted)).unwrap();
        let out = app
            .invoke(AgentState::new("make the build pass"))
            .await
            .unwrap();

        assert!(
            out.approved,
            "real exit 0 should satisfy the deterministic gate"
        );
        assert_eq!(out.steps, 2, "run_shell -> finish");
        assert!(out.messages.iter().any(|m| m.contains("run_shell")));
    }

    /// P0b:一次工具调用后,模型面向的 history 里应出现 role=tool 结果(匹配 tool_call_id)+ 带 tool_calls 的 assistant。
    #[tokio::test]
    async fn history_carries_role_tool_after_tool_call() {
        use provider::{Completion, Role, ScriptedProvider};
        let scripted = ScriptedProvider::new(vec![
            Completion {
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: "run_shell".to_string(),
                    arguments: serde_json::json!({"cmd": "exit 0"}),
                }],
                ..Default::default()
            },
            Completion {
                text: "done".to_string(),
                ..Default::default()
            },
        ]);
        let app = build_llm_agent(Arc::new(scripted)).unwrap();
        let out = app.invoke(AgentState::new("build")).await.unwrap();

        // history = [user(build), assistant(tool_calls), tool_result(call_1), assistant(done)]
        assert!(out
            .history
            .iter()
            .any(|m| m.role == Role::Tool && m.tool_call_id.as_deref() == Some("call_1")));
        assert!(out
            .history
            .iter()
            .any(|m| m.role == Role::Assistant && !m.tool_calls.is_empty()));
        // to_messages 会在最前面加 system;history 首条是 user 任务。
        assert_eq!(out.history.first().map(|m| &m.role), Some(&Role::User));
    }

    // 一个「永不收工、每轮都调同一个失败命令」的 provider 步骤,带可配的 token 用量。
    fn stuck_step(tokens: u32) -> provider::Completion {
        provider::Completion {
            tool_calls: vec![ToolCall {
                id: "1".to_string(),
                name: "run_shell".to_string(),
                arguments: serde_json::json!({"cmd": "exit 1"}),
            }],
            usage: provider::Usage {
                prompt_tokens: tokens,
                completion_tokens: 0,
            },
            ..Default::default()
        }
    }

    /// 成本护栏:每轮烧 token,预算耗尽即熔断,不跑到回合上限。
    #[tokio::test]
    async fn budget_breaker_stops_before_cap() {
        use provider::ScriptedProvider;
        let provider = ScriptedProvider::new((0..8).map(|_| stuck_step(100)).collect::<Vec<_>>());
        let app = build_llm_agent(Arc::new(provider)).unwrap();
        let out = app
            .invoke(AgentState::new("loop").with_budget(250))
            .await
            .unwrap();

        assert!(!out.approved, "must not fake success");
        assert!(out.total_tokens >= 250, "hit budget: {}", out.total_tokens);
        assert!(
            out.steps < MAX_STEPS,
            "budget熔断应早于回合上限: steps={}",
            out.steps
        );
    }

    /// 无进展检测:工具输出连续 MAX_STALL 轮不变即熔断,不跑到回合上限。
    #[tokio::test]
    async fn no_progress_detection_stops_before_cap() {
        use provider::ScriptedProvider;
        let provider = ScriptedProvider::new((0..8).map(|_| stuck_step(0)).collect::<Vec<_>>());
        let app = build_llm_agent(Arc::new(provider)).unwrap();
        let out = app.invoke(AgentState::new("stuck")).await.unwrap();

        assert!(!out.approved);
        assert!(out.stall >= MAX_STALL, "stall={}", out.stall);
        assert!(
            out.steps < MAX_STEPS,
            "no-progress熔断应早于回合上限: steps={}",
            out.steps
        );
    }

    /// M2 端到端:LLM 发一个**命名空间**工具调用 → act 路由到 MCP 服务器 → verify 认其结果 → approved。
    /// 用离线 FnTransport 站位真实 MCP 服务器,零联网。
    #[tokio::test]
    async fn llm_agent_routes_tool_call_to_mcp_server() {
        use mcp::FnTransport;
        use provider::{Completion, ScriptedProvider};

        // 假 MCP 服务器:有一个 check 工具,调用返回成功信号 "tests: passed"。
        let transport = FnTransport(|method: &str, _p: &serde_json::Value| match method {
            "initialize" => Ok(serde_json::json!({})),
            "tools/list" => Ok(serde_json::json!({"tools": [
                {"name": "check", "description": "run project checks", "inputSchema": {"type": "object"}}
            ]})),
            "tools/call" => {
                Ok(serde_json::json!({"content": [{"type": "text", "text": "tests: passed"}]}))
            }
            m => Err(mcp::McpError::BadResponse(m.to_string())),
        });
        let client = Arc::new(McpClient::new("ci", Box::new(transport)));
        let mcp_tools = resolve_mcp(vec![client]).await;

        // LLM:第 1 轮调命名空间工具 ci__check;第 2 轮收尾。
        let scripted = ScriptedProvider::new(vec![
            Completion {
                tool_calls: vec![ToolCall {
                    id: "1".to_string(),
                    name: "ci__check".to_string(),
                    arguments: serde_json::json!({}),
                }],
                ..Default::default()
            },
            Completion {
                text: "done".to_string(),
                ..Default::default()
            },
        ]);
        let app = build_llm_agent_with(Arc::new(scripted), mcp_tools).unwrap();
        let out = app.invoke(AgentState::new("run ci")).await.unwrap();

        assert!(out.approved, "MCP 工具返回 passed 应满足确定性闸");
        assert!(out.messages.iter().any(|m| m.contains("ci__check")));
        assert_eq!(out.tool_output.as_deref(), Some("tests: passed"));
    }

    // maker:跑 exit 0(确定性通过)然后收尾。
    fn maker_passes_then_finishes() -> provider::ScriptedProvider {
        use provider::{Completion, ScriptedProvider};
        ScriptedProvider::new(vec![
            Completion {
                tool_calls: vec![ToolCall {
                    id: "1".to_string(),
                    name: "run_shell".to_string(),
                    arguments: serde_json::json!({"cmd": "exit 0"}),
                }],
                ..Default::default()
            },
            Completion {
                text: "done".to_string(),
                ..Default::default()
            },
        ])
    }

    /// M4:确定性闸通过,但**独立 reviewer** 发现作弊 → 最终不批准。
    #[tokio::test]
    async fn independent_reviewer_rejects_cheating() {
        use provider::{Completion, ScriptedProvider};
        let reviewer = ScriptedProvider::new(
            (0..8)
                .map(|_| Completion {
                    text: "REJECT: agent deleted the failing test".to_string(),
                    ..Default::default()
                })
                .collect(),
        );
        let app = build_llm_agent_reviewed(
            Arc::new(maker_passes_then_finishes()),
            McpTools::empty(),
            Arc::new(reviewer),
        )
        .unwrap();
        let out = app
            .invoke(AgentState::new("make tests pass"))
            .await
            .unwrap();

        assert!(!out.approved, "独立 reviewer 应拦下作弊,即使确定性闸已过");
        assert!(out.messages.iter().any(|m| m.contains("reviewer REJECT")));
    }

    /// M4:确定性闸通过 + 独立 reviewer 认可 → 批准。
    #[tokio::test]
    async fn independent_reviewer_approves_legit_work() {
        use provider::{Completion, ScriptedProvider};
        let reviewer = ScriptedProvider::new(vec![Completion {
            text: "APPROVE".to_string(),
            ..Default::default()
        }]);
        let app = build_llm_agent_reviewed(
            Arc::new(maker_passes_then_finishes()),
            McpTools::empty(),
            Arc::new(reviewer),
        )
        .unwrap();
        let out = app
            .invoke(AgentState::new("make tests pass"))
            .await
            .unwrap();

        assert!(out.approved);
        assert!(out.messages.iter().any(|m| m.contains("独立 reviewer")));
        assert_eq!(out.steps, 2);
    }

    /// M5:规划器把目标拆成子任务(容忍模型包裹的解释文字)。
    #[tokio::test]
    async fn planner_decomposes_goal_into_subtasks() {
        use provider::{Completion, ScriptedProvider};
        let provider = ScriptedProvider::new(vec![Completion {
            text: r#"Sure! ["add fn", "write test", "run cargo test"]"#.to_string(),
            ..Default::default()
        }]);
        let subs = plan(&provider, "implement add").await;
        assert_eq!(subs, vec!["add fn", "write test", "run cargo test"]);
    }

    /// 知识层:扫 SKILL.md 解析成 Skill 并注入 system prompt(让 agent 做编程外的事)。
    #[test]
    fn load_skills_and_inject_into_system_prompt() {
        let mut dir = std::env::temp_dir();
        dir.push(format!("ridge_skills_{}", std::process::id()));
        let sk_dir = dir.join("cooking");
        std::fs::create_dir_all(&sk_dir).unwrap();
        std::fs::write(
            sk_dir.join("SKILL.md"),
            "---\nname: cooking\ndescription: how to cook pasta\n---\nBoil water, add pasta, wait 9 minutes.\n",
        )
        .unwrap();

        let skills = load_skills(&dir);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "cooking");
        assert_eq!(skills[0].description, "how to cook pasta");
        assert!(skills[0].body.contains("Boil water"));

        let prompt = build_system_prompt(&skills);
        assert!(prompt.contains("cooking"));
        assert!(prompt.contains("Boil water")); // 领域知识进了 system prompt

        // 空目录 → 无技能,用基础 prompt。
        assert_eq!(build_system_prompt(&[]), BASE_SYSTEM);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// DoD②:/compact 压缩历史 —— 长度显著减少,保留首条(任务)+ 最近 keep 条。
    #[test]
    fn compact_history_shrinks_but_keeps_task_and_recent() {
        let hist: Vec<Message> = (0..10).map(|i| Message::user(format!("m{i}"))).collect();
        let compacted = compact_history(hist, 4);
        // 1(首)+ 1(摘要)+ 4(最近) = 6 < 10
        assert_eq!(compacted.len(), 6);
        assert_eq!(compacted[0].content, "m0"); // 原始任务保留
        assert!(compacted[1].content.contains("压缩")); // 摘要标记
        assert_eq!(compacted.last().unwrap().content, "m9"); // 最近保留
                                                             // 短历史不动。
        let short: Vec<Message> = (0..3).map(|i| Message::user(format!("s{i}"))).collect();
        assert_eq!(compact_history(short.clone(), 4).len(), short.len());
    }

    /// 自动压缩:history 估算 token 超阈值(按**内容体量**而非条数)时,`to_messages` 发给 LLM
    /// 的消息收敛为有界快照(O(n)→O(1))。
    #[test]
    fn to_messages_auto_compacts_when_history_heavy() {
        // 40 条较大消息 → 估算 token 总量超阈值 → 触发压缩。
        let mut hist = vec![Message::user("原始任务")];
        for i in 0..40 {
            hist.push(Message::assistant(format!("step {i}: {}", "x".repeat(700))));
        }
        let s = AgentState::new("原始任务").with_history(hist);
        let msgs = to_messages("SYS", &s);
        assert!(
            msgs.len() <= 1 + 2 + AUTO_COMPACT_KEEP,
            "重历史应收敛为有界,实得 {}",
            msgs.len()
        );
        assert_eq!(msgs[0].role, Role::System);
        assert!(
            msgs.iter().any(|m| m.content.contains("压缩")),
            "应有压缩标记"
        );
        assert!(
            msgs.iter().any(|m| m.content == "原始任务"),
            "原始任务须保留"
        );
        // 轻历史(总量未超阈值)不压缩,全量带过 —— 哪怕条数不少也不误伤。
        let light = AgentState::new("t")
            .with_history((0..20).map(|i| Message::user(format!("m{i}"))).collect());
        assert_eq!(to_messages("SYS", &light).len(), 1 + 20);
    }

    /// 触发判据的本地估算:同字符数下 CJK ≈ ASCII 的 4 倍(CJK 1 tok/字,ASCII 1 tok/4 字符)。
    #[test]
    fn est_tokens_weights_cjk_heavier_than_ascii() {
        assert_eq!(est_tokens(&"a".repeat(400)), 100);
        assert_eq!(est_tokens(&"中".repeat(400)), 400);
    }

    /// 静态底噪守护:工具 Schema 每轮都发,描述须精简且不回潮(去客套/内部机制/schema 重复)。
    #[test]
    fn tool_descriptions_stay_terse() {
        // 每工具 description 字符上限 —— 描述只说「做什么 + 何时用」,不复述 schema、不讲内部机制。
        const TOOL_DESC_MAX: usize = 120;
        let specs = builtin_tool_specs();
        assert!(!specs.is_empty());
        for s in &specs {
            let n = s.description.chars().count();
            assert!(
                n < TOOL_DESC_MAX,
                "工具 {} 描述 {n} 字,超上限 {TOOL_DESC_MAX} —— 精简它",
                s.name
            );
        }
    }

    /// 输出端省钱:BASE_SYSTEM 含 Lean-output 约束(简洁作答 + 只出最小编辑)。
    #[test]
    fn base_system_has_lean_output_directive() {
        assert!(BASE_SYSTEM.contains("concisely"));
        assert!(BASE_SYSTEM.contains("minimal edit"));
        // 无技能时系统提示词仍等于 BASE_SYSTEM(不引入额外底噪)。
        assert_eq!(build_system_prompt(&[]), BASE_SYSTEM);
    }

    /// Durable State 回填:写类工具成功 → 记 modified_files 清 last_error;工具错误 → 置 last_error。
    #[test]
    fn durable_state_backfill_from_tools() {
        let mut st = AgentState::new("t");
        let ok = ToolCall {
            id: "1".into(),
            name: "write_file".into(),
            arguments: serde_json::json!({"path":"src/a.rs","contents":"x"}),
        };
        for p in durable_updates(&ok, "wrote 1 bytes to src/a.rs") {
            st.apply(p);
        }
        assert!(st.modified_files.contains("src/a.rs"));
        assert!(st.last_error.is_none());

        let bad = ToolCall {
            id: "2".into(),
            name: "edit_file".into(),
            arguments: serde_json::json!({"path":"src/b.rs","old_string":"x","new_string":"y"}),
        };
        for p in durable_updates(&bad, "edit error: old_string 未找到") {
            st.apply(p);
        }
        assert_eq!(
            st.last_error.as_deref(),
            Some("edit error: old_string 未找到")
        );
        assert!(
            !st.modified_files.contains("src/b.rs"),
            "失败不记入已改文件"
        );
    }

    /// 事实驱动 O(1):反复改同两文件 50 步,事实块字符数恒定(不随步数膨胀)。
    #[test]
    fn durable_state_block_stays_bounded_over_steps() {
        let mut st = AgentState::new("t");
        let block_len = |st: &AgentState| {
            durable_state_block(st)
                .map(|b| b.chars().count())
                .unwrap_or(0)
        };
        let mut prev = 0;
        for i in 0..50 {
            let f = if i % 2 == 0 { "a.rs" } else { "b.rs" };
            let call = ToolCall {
                id: i.to_string(),
                name: "write_file".into(),
                arguments: serde_json::json!({"path": f, "contents":"x"}),
            };
            for p in durable_updates(&call, "wrote 1 bytes") {
                st.apply(p);
            }
            let now = block_len(&st);
            if i >= 2 {
                assert_eq!(now, prev, "事实块应有界恒定,step {i} 却变了");
            }
            prev = now;
        }
        assert_eq!(st.modified_files.len(), 2, "去重后仅 2 个文件");
    }

    /// 事实块注入 messages **末尾**(role=system,冻结的首部 system prompt 不动);无事实则不加。
    #[test]
    fn to_messages_appends_durable_fact_block() {
        let mut st = AgentState::new("原始任务").with_history(vec![Message::user("原始任务")]);
        st.modified_files.insert("a.rs".into());
        st.last_error = Some("boom".into());
        let msgs = to_messages("SYS", &st);
        assert_eq!(msgs[0].content, "SYS", "首部 system prompt 保持冻结");
        let last = msgs.last().unwrap();
        assert_eq!(last.role, Role::System);
        assert!(last.content.contains("a.rs") && last.content.contains("boom"));
        // 无 durable 状态 → 不加尾块。
        let clean = AgentState::new("t").with_history(vec![Message::user("t")]);
        assert!(!to_messages("SYS", &clean)
            .last()
            .unwrap()
            .content
            .contains("durable_state"));
    }

    /// 沙箱深度防御:越出 cwd 的绝对路径写 → `execute_tool_call` 硬拒(BLOCKED)且不落盘。
    #[test]
    fn jail_blocks_write_outside_cwd() {
        let outside = std::env::temp_dir().join("ridge_jail_evil_marker.txt");
        let _ = std::fs::remove_file(&outside);
        let call = ToolCall {
            id: "j".into(),
            name: "write_file".into(),
            arguments: serde_json::json!({"path": outside.to_str().unwrap(), "contents":"x"}),
        };
        let obs = execute_tool_call(&call);
        assert!(obs.starts_with("BLOCKED"), "越狱写应被拦: {obs}");
        assert!(!outside.exists(), "拦截后绝不落盘");
    }

    /// 压缩窗口首端的悬空 role=tool(配对 assistant 已被压掉)必须裁掉,防 OpenAI 兼容端点 400。
    #[test]
    fn compact_history_drops_dangling_tool_result() {
        let mut hist = vec![Message::user("task")];
        for i in 0..8 {
            hist.push(Message::assistant(format!("a{i}")));
        }
        hist.push(Message::tool_result("call1", "tool out A")); // keep=4 时会落在窗口首
        hist.push(Message::assistant("a-final"));
        hist.push(Message::tool_result("call2", "tool out B"));
        hist.push(Message::assistant("a-last"));
        let out = compact_history(hist, 4);
        assert_eq!(out[0].content, "task"); // 原始任务保留
        assert_ne!(
            out[2].role,
            Role::Tool,
            "首条保留消息不应是悬空 tool: {:?}",
            out[2]
        );
    }

    /// M5:模型没给出可解析的数组 → 降级为单个子任务(绝不返回空)。
    #[tokio::test]
    async fn planner_falls_back_when_unparseable() {
        use provider::{Completion, ScriptedProvider};
        let provider = ScriptedProvider::new(vec![Completion {
            text: "I'm not sure how to break this down".to_string(),
            ..Default::default()
        }]);
        let subs = plan(&provider, "do the thing").await;
        assert_eq!(subs, vec!["do the thing"]);
    }

    /// 通用性:开放式任务(工具输出无 exit0/passed 也无失败信号)+ 模型 finish → 接受完成,不空转到上限。
    /// (修复 MCP 信息类任务空转烧 token 的问题。)
    #[tokio::test]
    async fn open_ended_finish_accepted_when_no_failure_signal() {
        use provider::{Completion, ScriptedProvider};
        let mut path = std::env::temp_dir();
        path.push("ridge_open_ended.txt");
        std::fs::write(&path, "neutral content, no success or failure signal").unwrap();

        let scripted = ScriptedProvider::new(vec![
            Completion {
                tool_calls: vec![ToolCall {
                    id: "1".to_string(),
                    name: "read_file".to_string(),
                    arguments: serde_json::json!({"path": path.to_str().unwrap()}),
                }],
                ..Default::default()
            },
            Completion {
                text: "here is the content".to_string(),
                ..Default::default()
            },
        ]);
        let app = build_llm_agent(Arc::new(scripted)).unwrap();
        let out = app.invoke(AgentState::new("read the file")).await.unwrap();

        assert!(out.approved, "模型 finish 且无失败信号 → 接受,不该空转");
        assert_eq!(out.steps, 2);
        std::fs::remove_file(&path).ok();
    }

    /// 安全硬门槛:危险命令即使走到 execute_tool_call 也被拦下,不执行。
    #[test]
    fn dangerous_shell_command_is_blocked() {
        let call = ToolCall {
            id: "x".to_string(),
            name: "run_shell".to_string(),
            arguments: serde_json::json!({"cmd": "rm -rf /"}),
        };
        let obs = execute_tool_call(&call);
        assert!(obs.starts_with("BLOCKED"), "{obs}");
    }

    /// P3 权限门:AutoDeny → 有副作用的工具不执行,观察为 permission denied,拿不到成功信号。
    #[tokio::test]
    async fn permission_gate_blocks_denied_tool() {
        use provider::{Completion, ScriptedProvider};
        let scripted = ScriptedProvider::new(vec![
            Completion {
                tool_calls: vec![ToolCall {
                    id: "1".to_string(),
                    name: "run_shell".to_string(),
                    arguments: serde_json::json!({"cmd": "exit 0"}),
                }],
                ..Default::default()
            },
            Completion {
                text: "done".to_string(),
                ..Default::default()
            },
        ]);
        let app = build_llm_agent_gated(Arc::new(scripted), McpTools::empty(), Arc::new(AutoDeny))
            .unwrap();
        let out = app.invoke(AgentState::new("build")).await.unwrap();

        assert!(out.messages.iter().any(|m| m.contains("permission denied")));
        assert!(!out.approved, "被拒的工具没真跑,拿不到 exit 0");
    }

    /// M5 完整:planner 拆 2 个子任务 → worker 逐个执行到 approved → 聚合整体通过。
    #[tokio::test]
    async fn orchestrator_plans_and_runs_subtasks() {
        use provider::{Completion, ScriptedProvider};

        let planner = ScriptedProvider::new(vec![Completion {
            text: r#"["impl add", "test add"]"#.to_string(),
            ..Default::default()
        }]);
        // worker 被两个子任务共享(串行):每个子任务耗 [跑 exit 0, 收尾] 两个补全。
        let step_pass = || Completion {
            tool_calls: vec![ToolCall {
                id: "1".to_string(),
                name: "run_shell".to_string(),
                arguments: serde_json::json!({"cmd": "exit 0"}),
            }],
            ..Default::default()
        };
        let step_done = || Completion {
            text: "done".to_string(),
            ..Default::default()
        };
        let worker =
            ScriptedProvider::new(vec![step_pass(), step_done(), step_pass(), step_done()]);

        let report = run_planned(
            Arc::new(planner),
            Arc::new(worker),
            "implement add with test",
        )
        .await
        .unwrap();

        assert_eq!(report.subtasks.len(), 2);
        assert!(report.approved, "两个子任务都应通过");
        assert!(report.subtasks.iter().all(|s| s.approved));
        assert_eq!(
            report
                .subtasks
                .iter()
                .map(|s| s.task.as_str())
                .collect::<Vec<_>>(),
            vec!["impl add", "test add"]
        );
    }
}
