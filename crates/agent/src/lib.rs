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
/// 分支工作区隔离(iter-25):BoN 并发分支的物理互踩防护 + 胜者合回,见 [`workspace`]。
pub mod workspace;
pub use rich_output::{
    Color, Formatter, MediaDisplay, MediaInfo, MediaType, RichOutput, TableDisplay,
};

/// 回合硬上限 —— **防跑飞的后备护栏**,非正常终止手段。真正的停机主力是:`approved`(目标达成)、
/// 预算熔断(`budget_tokens`)、无进展检测(`stalled`)。旧值 8 过低,真实多文件任务动辄十数次工具调用即被腰斩;
/// 提到 30(≈60 超步,稳在引擎默认 100 超步之下,无需动引擎/RunConfig)。要更长任务:设 `budget_tokens` 控成本、
/// 或后续把本上限做成可配 + 按其派生引擎超步上限(见 CONTRACT-iteration-15)。
pub const MAX_STEPS: usize = 30;

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
    /// 连续**工具/provider 报错**轮数(与 `stall` 正交:stall 认「输出相同」,本字段认「输出为错误」,
    /// 故报错内容**每轮不同**时 stall 不触发、由本字段兜底)。到 [`MAX_ERR_STREAK`] 熔断,防无人值守烧预算。
    pub err_streak: usize,
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
    /// **信号复利**:run 启动时从 `.ridge/signals` 载入的「继承信号」有界注入块(上个会话留下的未决发现/
    /// 摩擦/待办)。run 中不变,由 CLI 在建 state 时经 [`load_signal_block`] 注入;无则 `None`。
    pub signal_block: Option<String>,
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

    /// 注入继承信号块(信号复利:上个会话的未决发现)。CLI 建 state 时调 [`load_signal_block`] 取之。
    pub fn with_signals(mut self, block: Option<String>) -> Self {
        self.signal_block = block;
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
    SetErrStreak(usize),
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
            Patch::SetErrStreak(n) => self.err_streak = n,
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

/// 连续工具/provider 报错多少轮就熔断(circuit breaker,防无人值守 `--every` 循环持续失败烧预算)。
pub const MAX_ERR_STREAK: usize = 5;

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
        "read_file" | "search" | "web_search" | "fetch_url" | "todo_write" | "signal_write"
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

// ───────────────────────── 斜杠命令:Prompt 模板 + Skills-as-命令(iter-39)─────────────────────────

/// 一个斜杠命令:**Prompt 模板** —— `/name [args]` 调用即把 `body`(其中 `$ARGS` 替换为 args)
/// 注入为一条任务喂给 agent。来源:`~/.ridge/commands/*.md`(用户自定义)或一个 [`Skill`](name→/name)。
#[derive(Clone, Debug, PartialEq)]
pub struct SlashCommand {
    /// 不含前导 `/`。
    pub name: String,
    pub description: String,
    pub body: String,
}

/// 解析命令 `.md`:可选 frontmatter(`description:`/`desc:`)+ 正文;**name 由文件名给**(非 frontmatter)。
/// 无 frontmatter → 全文即 body。纯函数,可单测。
pub fn parse_command_md(text: &str, name: &str) -> SlashCommand {
    let parsed = text.strip_prefix("---").and_then(|rest| {
        rest.find("\n---").map(|end| {
            let front = &rest[..end];
            let body = rest[end + 4..]
                .trim_start_matches(['-', '\n'])
                .trim()
                .to_string();
            let mut desc = String::new();
            for line in front.lines() {
                let line = line.trim();
                if let Some(v) = line
                    .strip_prefix("description:")
                    .or_else(|| line.strip_prefix("desc:"))
                {
                    desc = v.trim().to_string();
                }
            }
            (desc, body)
        })
    });
    let (description, body) = parsed.unwrap_or_else(|| (String::new(), text.trim().to_string()));
    SlashCommand {
        name: name.to_string(),
        description,
        body,
    }
}

/// 展开命令 body:`$ARGS` 全部替换为 `args`;body 无 `$ARGS` 且 args 非空 → args 追加末尾。纯函数。
pub fn expand_command(body: &str, args: &str) -> String {
    if body.contains("$ARGS") {
        body.replace("$ARGS", args)
    } else if args.trim().is_empty() {
        body.to_string()
    } else {
        format!("{body}\n\n{args}")
    }
}

/// 扫描 `<dir>/*.md` 为命令 + 把每个 skill 暴露为同名命令(**文件命令优先,同名 skill 跳过**)。
/// 目录不存在 → 只有 skill 命令。供 TUI 斜杠命令扩展(name→/name)。
pub fn load_commands(dir: impl AsRef<std::path::Path>, skills: &[Skill]) -> Vec<SlashCommand> {
    let mut out: Vec<SlashCommand> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) == Some("md") {
                if let (Some(stem), Ok(text)) = (
                    path.file_stem().and_then(|s| s.to_str()),
                    std::fs::read_to_string(&path),
                ) {
                    if !stem.is_empty() {
                        out.push(parse_command_md(&text, stem));
                    }
                }
            }
        }
    }
    for s in skills {
        if !out.iter().any(|c| c.name == s.name) {
            out.push(SlashCommand {
                name: s.name.clone(),
                description: s.description.clone(),
                body: s.body.clone(),
            });
        }
    }
    out
}

/// 按 name 查命令(name 不含前导 `/`)。纯函数。
pub fn resolve_command<'a>(name: &str, commands: &'a [SlashCommand]) -> Option<&'a SlashCommand> {
    commands.iter().find(|c| c.name == name)
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

/// sub-agent 步数上限(只读检索)。旧值 8 对真实仓库的多文件侦察偏紧;提到 15 仍有界、恒只读故低风险。
const SUBAGENT_MAX_STEPS: usize = 15;

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

// ───────────────────────── 内置供应商 preset + auth 密钥库(iter-37)─────────────────────────

/// 一条内置供应商预设:选它 + 填一把 key 即接入,免手敲 base_url/kind。纯静态数据,编进二进制。
#[derive(Debug, Clone, Copy)]
pub struct ProviderPreset {
    /// 短 id(命令里用,如 `login deepseek`)。
    pub id: &'static str,
    /// 人读名。
    pub label: &'static str,
    /// `openai`(兼容端点)| `anthropic`。
    pub kind: &'static str,
    pub base_url: &'static str,
    /// 该家一个稳妥的默认模型(用户可随时 `--model` 或 `/model` 改)。
    pub default_model: &'static str,
    /// 约定的密钥环境变量名 —— 也是 `auth.json` 里存该家 key 的槽名。
    pub key_env: &'static str,
}

/// 内置供应商清单:世界顶级 + 中国顶级 + 知名聚合。绝大多数是 OpenAI 兼容端点,Claude 走原生。
/// **优先级即接入便捷度的落点**;`login <id>` 据此一键成档。
pub const PROVIDER_PRESETS: &[ProviderPreset] = &[
    // ── 世界顶级 ──
    ProviderPreset {
        id: "openai",
        label: "OpenAI",
        kind: "openai",
        base_url: "https://api.openai.com/v1",
        default_model: "gpt-4o",
        key_env: "OPENAI_API_KEY",
    },
    ProviderPreset {
        id: "anthropic",
        label: "Anthropic Claude",
        kind: "anthropic",
        base_url: "https://api.anthropic.com/v1",
        default_model: "claude-sonnet-4-6",
        key_env: "ANTHROPIC_API_KEY",
    },
    ProviderPreset {
        id: "gemini",
        label: "Google Gemini",
        kind: "openai",
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        default_model: "gemini-2.0-flash",
        key_env: "GEMINI_API_KEY",
    },
    ProviderPreset {
        id: "grok",
        label: "xAI Grok",
        kind: "openai",
        base_url: "https://api.x.ai/v1",
        default_model: "grok-2-latest",
        key_env: "XAI_API_KEY",
    },
    // ── 中国顶级 ──
    ProviderPreset {
        id: "glm",
        label: "Zhipu GLM (智谱)",
        kind: "openai",
        base_url: "https://open.bigmodel.cn/api/paas/v4",
        default_model: "glm-4.6",
        key_env: "ZHIPU_API_KEY",
    },
    ProviderPreset {
        id: "kimi",
        label: "Moonshot Kimi (月之暗面)",
        kind: "openai",
        base_url: "https://api.moonshot.cn/v1",
        default_model: "kimi-k2",
        key_env: "MOONSHOT_API_KEY",
    },
    ProviderPreset {
        id: "deepseek",
        label: "DeepSeek (深度求索)",
        kind: "openai",
        base_url: "https://api.deepseek.com/v1",
        default_model: "deepseek-chat",
        key_env: "DEEPSEEK_API_KEY",
    },
    ProviderPreset {
        id: "qwen",
        label: "Alibaba Qwen / DashScope (通义千问)",
        kind: "openai",
        base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        default_model: "qwen-max",
        key_env: "DASHSCOPE_API_KEY",
    },
    ProviderPreset {
        id: "hunyuan",
        label: "Tencent Hunyuan (腾讯混元)",
        kind: "openai",
        base_url: "https://api.hunyuan.cloud.tencent.com/v1",
        default_model: "hunyuan-turbo",
        key_env: "HUNYUAN_API_KEY",
    },
    ProviderPreset {
        id: "minimax",
        label: "MiniMax (稀宇)",
        kind: "openai",
        base_url: "https://api.minimax.chat/v1",
        default_model: "MiniMax-Text-01",
        key_env: "MINIMAX_API_KEY",
    },
    // ── 知名聚合 ──
    ProviderPreset {
        id: "openrouter",
        label: "OpenRouter (聚合)",
        kind: "openai",
        base_url: "https://openrouter.ai/api/v1",
        default_model: "anthropic/claude-3.5-sonnet",
        key_env: "OPENROUTER_API_KEY",
    },
    ProviderPreset {
        id: "siliconflow",
        label: "SiliconFlow (硅基流动)",
        kind: "openai",
        base_url: "https://api.siliconflow.cn/v1",
        default_model: "deepseek-ai/DeepSeek-V3",
        key_env: "SILICONFLOW_API_KEY",
    },
    ProviderPreset {
        id: "together",
        label: "Together AI (聚合)",
        kind: "openai",
        base_url: "https://api.together.xyz/v1",
        default_model: "meta-llama/Llama-3.3-70B-Instruct-Turbo",
        key_env: "TOGETHER_API_KEY",
    },
    ProviderPreset {
        id: "groq",
        label: "Groq (聚合/极速)",
        kind: "openai",
        base_url: "https://api.groq.com/openai/v1",
        default_model: "llama-3.3-70b-versatile",
        key_env: "GROQ_API_KEY",
    },
];

/// 按 id 查 preset(大小写不敏感)。未知 → `None`。
pub fn preset_by_id(id: &str) -> Option<&'static ProviderPreset> {
    let id = id.trim().to_lowercase();
    PROVIDER_PRESETS.iter().find(|p| p.id == id)
}

/// preset → `ProviderProfile`(名与 model 可覆盖)。**api_key 恒 None** —— key 只进 auth.json,不入 config。
pub fn preset_to_profile(
    preset: &ProviderPreset,
    name: Option<&str>,
    model: Option<&str>,
) -> ProviderProfile {
    ProviderProfile {
        name: name
            .map(|s| s.to_string())
            .unwrap_or_else(|| preset.id.to_string()),
        kind: preset.kind.to_string(),
        model: model.unwrap_or(preset.default_model).to_string(),
        base_url: preset.base_url.to_string(),
        key_env: preset.key_env.to_string(),
        api_key: None,
    }
}

/// 解析 `~/.ridge/auth.json` 密钥库文本 → `key_env → key` 映射。坏/空/非对象 → 空表(不崩)。
/// 只收字符串值(未来 OAuth 档为对象,本轮跳过对象值 —— 前向兼容接缝)。
pub fn auth_parse(text: &str) -> std::collections::BTreeMap<String, String> {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Object(m)) => m
            .into_iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
            .collect(),
        _ => std::collections::BTreeMap::new(),
    }
}

/// 往密钥库文本写入/覆盖一把 key(按 `key_env` 名),**保留其余槽**,返回美化 JSON。
/// 文本空/坏 → 从空对象起。纯变换,可单测;写盘 + 收权限由调用方做。
pub fn auth_upsert(text: &str, key_env: &str, key: &str) -> String {
    let mut map = auth_parse(text);
    map.insert(key_env.to_string(), key.to_string());
    let obj: serde_json::Map<String, serde_json::Value> = map
        .into_iter()
        .map(|(k, v)| (k, serde_json::Value::String(v)))
        .collect();
    serde_json::to_string_pretty(&serde_json::Value::Object(obj)).unwrap_or_else(|_| "{}".into())
}

/// 从密钥库文本取某槽的 key。
pub fn auth_get(text: &str, key_env: &str) -> Option<String> {
    auth_parse(text).remove(key_env)
}

/// `login` 的纯核:据 preset 把一个档案加/覆盖进 config 文本的 `providers[]`(经
/// [`config_add_provider`],**产物不含 key**),`make_default` 时再把顶层
/// `provider/model/base_url/key_env` 指向该 preset。key 的落盘由调用方写 auth.json,与此无关。
pub fn apply_login(
    config_text: &str,
    preset: &ProviderPreset,
    name: Option<&str>,
    model: Option<&str>,
    make_default: bool,
) -> Result<String, String> {
    let profile = preset_to_profile(preset, name, model);
    let mut text = config_add_provider(config_text, &profile)?;
    if make_default {
        // 顶层四键指向该 preset;key_env 让启动从 auth.json 取顶层 key(不写明文进 config)。
        text = config_set(&text, "provider", preset.kind)?;
        text = config_set(&text, "model", &profile.model)?;
        text = config_set(&text, "base_url", preset.base_url)?;
        let mut root = match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(serde_json::Value::Object(m)) => m,
            _ => serde_json::Map::new(),
        };
        // key_env 不在 CONFIG_KEYS 白名单(它非用户手调标量),直接对 JSON 对象写。
        root.insert(
            "key_env".to_string(),
            serde_json::Value::String(preset.key_env.to_string()),
        );
        // 抹掉顶层残留内联 api_key —— 否则旧 key 会配新 base_url 认证错乱;新 key 由 key_env→auth 唯一供给。
        root.remove("api_key");
        text = serde_json::to_string_pretty(&serde_json::Value::Object(root))
            .map_err(|e| e.to_string())?;
    }
    Ok(text)
}

/// 通用 agent 的基础 system prompt(不再只面向编码)。
const BASE_SYSTEM: &str = "You are a capable agent. Use the provided tools to accomplish the \
     user's task. To change existing files, prefer edit_file (surgical, unique-match replace) over \
     rewriting the whole file with write_file; use search and ranged read_file to explore before \
     editing. For external/real-time info, web_search to find links then fetch_url to read the \
     actual page — trust the page text, not just the snippet. When there is an objective way to \
     verify (compiler exit code, tests), rely on it and don't trust your own claim. \
     Harness contract: large tool outputs are truncated to a head+tail preview — for detail from a \
     big file use ranged read_file or search, never rely on one giant read. Never delete or empty \
     tests to make a check pass: it is blocked and counts as failure. Record a reusable finding, \
     pitfall or todo with signal_write so the next session inherits it. \
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

/// 熔断?(连续 MAX_ERR_STREAK 轮工具/provider 报错 —— 即便报错内容每轮不同 stall 不触发)
fn circuit_broken(s: &AgentState) -> bool {
    s.err_streak >= MAX_ERR_STREAK
}

/// 确定性**成功**信号(编码任务:shell `exit 0` 或测试 `passed`)。
/// shell 成功恒以 harness 产出的前缀 `"exit 0:"` 打头 —— 用 `starts_with` 而非 `contains`:
/// ①修正确性 bug(失败命令 `exit 7: ...` 正文若含 "exit 0" 文本会被 `contains` 误判成功);
/// ②堵奖励黑客(模型无法伪造位于**行首**的退出码前缀)。
fn tool_output_ok(o: &str) -> bool {
    o.starts_with("exit 0:") || (o.contains("passed") && !o.contains("failed"))
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

/// Best-of-N 分支择优的**确定性**评分(iter-24,配 `CompiledGraph::invoke_best_of`):
/// approved(过独立 verify)压倒一切;同侪比 token 消耗,省者胜。maker≠checker —— 只认
/// 确定性信号,不引入模型自评。真实接入待分支工作区隔离(见 guidance-24 边界)。
pub fn branch_score(s: &AgentState) -> i64 {
    let base = if s.approved { 1_000_000_000 } else { 0 };
    base - s.total_tokens as i64
}

/// 多层独立退出:到回合上限 / 超预算 / 无进展 / 熔断任一命中,循环都该停(loop engineering:停机是设计的一半)。
/// 全是 O(1) 字段判定;上下文腐烂(需算压缩)不进此热路径,只在终态 [`halt_reason`] 里作诊断重标签。
fn must_stop(s: &AgentState) -> bool {
    s.steps >= MAX_STEPS || over_budget(s) || stalled(s) || circuit_broken(s)
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
        ToolSpec {
            name: "signal_write".to_string(),
            description: "记录/消解**跨会话复用**的信号(发现/摩擦/待办)。记:给 type+body;消解已处理的:给 resolve=<id>。下个会话自动继承未决信号".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"type":{"type":"string"},"body":{"type":"string"},"resolve":{"type":"string"}}}),
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
/// 工具观察是否为**错误**(错误前缀 / BLOCKED / permission denied)。单一真相:
/// Durable State 回填与熔断计数(`err_streak`)共用,免两处判据漂移。
fn is_error_observation(obs: &str) -> bool {
    obs.contains(" error:") || obs.starts_with("BLOCKED") || obs.starts_with("permission denied")
}

fn durable_updates(call: &ToolCall, observation: &str) -> Vec<Patch> {
    if is_error_observation(observation) {
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

/// 地址越狱开关(iter-34):进程级,默认 **关**。开则 `jail` 放行 cwd 子树外的写。
/// **只放宽 cwd 子树这一条** —— 危险命令硬拦截、受保护路径(tests/.git)守卫、只读模式全不受影响。
/// 与 `jail` 已读的进程级 cwd 同层(进程内 TUI 与后台任务共享),不逐调用穿参。
static ALLOW_JAILBREAK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// 置地址越狱开关(启动读 config、TUI `/jailbreak` 实时切)。安全放宽,开启时 TUI 须显红标。
pub fn set_allow_jailbreak(on: bool) {
    ALLOW_JAILBREAK.store(on, std::sync::atomic::Ordering::Relaxed);
}
/// 读地址越狱开关(`jail` 与状态栏红标用)。
pub fn allow_jailbreak() -> bool {
    ALLOW_JAILBREAK.load(std::sync::atomic::Ordering::Relaxed)
}

/// 写操作沙箱守卫:路径须落在**进程 cwd 子树**内(`--cwd` 设的工作目录),越狱 → `Err(BLOCKED 串)`。
/// 深度防御,与危险命令拦截同层:即使模型幻觉出绝对路径/`..` 逃逸,也硬拒,防写出工作目录祸害宿主。
fn jail(path: &str) -> Result<(), String> {
    jail_guard(allow_jailbreak(), path)
}

/// jail 决策纯函数(iter-34):`allow` 为开关快照。`allow==true` → 放行;否则钳在 cwd 子树。
/// 抽纯函数是为可测且**不在测试里翻全局**(AtomicBool 全局若被某测试改会污染并行的 `jail_blocks_write_outside_cwd`)。
fn jail_guard(allow: bool, path: &str) -> Result<(), String> {
    if allow {
        return Ok(());
    }
    let root = std::env::current_dir().map_err(|e| format!("BLOCKED (jail): 取 cwd 失败: {e}"))?;
    tools::jail_path(&root, path)
        .map(|_| ())
        .map_err(|e| format!("BLOCKED (jail): {e}"))
}

/// 有副作用的内置工具(改文件 / 跑 shell)。只读模式过滤/拒绝它们;jail 只管其中的写文件路径。
fn is_mutating_tool(name: &str) -> bool {
    matches!(
        name,
        "run_shell" | "write_file" | "edit_file" | "apply_edits"
    )
}

/// **受保护路径**(词法判定):路径任一组件为 `tests`(约定测试目录)或 `.git`。防**奖励黑客**
/// —— 删/清空失败测试以伪造 CI 绿(loop engineering 头号失败模式)。用 `tests`(复数目录)而非 `test`
/// 单词,免误伤 `cargo test`/`test_output.log` 等。
fn is_protected_path(path: &str) -> bool {
    path.replace('\\', "/")
        .split('/')
        .any(|c| matches!(c, "tests" | ".git"))
}

/// 约束守卫(写臂):往受保护路径写**空内容** = 清空测试 → 拒。正常带内容的编辑放行。
fn constraint_guard_write(path: &str, contents: &str) -> Option<String> {
    (is_protected_path(path) && contents.trim().is_empty())
        .then(|| format!("BLOCKED (constraint): 拒绝清空受保护路径 {path}(防奖励黑客删/空测试)"))
}

/// 约束守卫(shell 臂):删除类命令(rm/rmdir/del/unlink/shred)或截断重定向(`>`)touch 受保护路径 → 拒。
fn constraint_guard_shell(cmd: &str) -> Option<String> {
    let lc = cmd.to_lowercase();
    let has_delete = lc
        .split(|c: char| c.is_whitespace())
        .any(|t| matches!(t, "rm" | "rmdir" | "del" | "unlink" | "shred"));
    let has_truncate = lc.contains('>');
    if !(has_delete || has_truncate) {
        return None;
    }
    // 按空白/引号切 token,看是否有 token 的路径组件命中受保护目录。
    let touches_protected = lc
        .split(|c: char| c.is_whitespace() || c == '"' || c == '\'')
        .any(is_protected_path);
    touches_protected.then(|| {
        format!("BLOCKED (constraint): 拒绝对受保护路径(测试)的删除/清空 `{cmd}`(防奖励黑客)")
    })
}

/// 只读模式(`--read-only`)的深度防御:副作用工具即使被 offer/幻觉调到,也硬拒。
/// `Some(观察串)` = 拒绝(与 offering 过滤形成双保险)。
fn read_only_block(read_only: bool, name: &str) -> Option<String> {
    (read_only && is_mutating_tool(name))
        .then(|| format!("BLOCKED (read-only): 只读模式拒绝副作用工具 {name}"))
}

// ───────────────────────── Hook 引擎(iter-40)─────────────────────────

static HOOKS: std::sync::OnceLock<Vec<HookCfg>> = std::sync::OnceLock::new();
static NOTIFY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 启动时装入用户 config 的 hooks(进程级 set-once,与 DYNAMIC_COMMANDS/ALLOW_JAILBREAK 先例一致)。
pub fn set_hooks(hooks: Vec<HookCfg>) {
    let _ = HOOKS.set(hooks);
}
/// 任务完成响铃开关(内置通知 hook)。
pub fn set_notify(on: bool) {
    NOTIFY.store(on, std::sync::atomic::Ordering::Relaxed);
}
fn active_hooks() -> &'static [HookCfg] {
    HOOKS.get().map(|v| v.as_slice()).unwrap_or(&[])
}

/// 选出匹配某事件(+ 工具名)的 hook。`matcher` 缺/空 = 匹配所有工具;否则工具名含该子串。纯函数。
pub fn hooks_for_event<'a>(hooks: &'a [HookCfg], event: &str, tool: &str) -> Vec<&'a HookCfg> {
    hooks
        .iter()
        .filter(|h| {
            h.event == event
                && h.matcher
                    .as_deref()
                    .map(|m| m.is_empty() || tool.contains(m))
                    .unwrap_or(true)
        })
        .collect()
}

/// 工具调用的「主参数」(喂给 hook 的 `RIDGE_TOOL_ARG`):按常见键取一个。纯函数。
fn tool_primary_arg(call: &ToolCall) -> String {
    for k in ["cmd", "path", "query", "url", "task"] {
        if let Some(s) = call.arguments.get(k).and_then(|v| v.as_str()) {
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }
    String::new()
}

/// 跑一条 hook 命令(跨平台),把工具名/主参数经 **env**(非全局,只挂这条 Command —— BSP 并发安全)
/// 注入。返回退出码。best-effort:起不来 → None。
fn run_hook_command(command: &str, tool: &str, arg: &str) -> Option<i32> {
    use std::process::Command;
    let mut c = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", command]);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(command);
        c
    };
    c.env("RIDGE_TOOL", tool).env("RIDGE_TOOL_ARG", arg);
    c.output().ok().map(|o| o.status.code().unwrap_or(-1))
}

/// pre_tool hook:任一 **blocking** hook 命令非 0 退出 → 返回 BLOCKED(拦下工具)。否则 None(放行)。
fn run_pre_tool_hooks(call: &ToolCall) -> Option<String> {
    let arg = tool_primary_arg(call);
    for h in hooks_for_event(active_hooks(), "pre_tool", &call.name) {
        let code = run_hook_command(&h.command, &call.name, &arg);
        if h.blocking.unwrap_or(false) && code.map(|c| c != 0).unwrap_or(true) {
            return Some(format!(
                "BLOCKED (pre_tool hook rejected `{}`) —— 见 config.hooks",
                call.name
            ));
        }
    }
    None
}

/// post_tool hook:工具跑完 fire-and-forget(如写文件后格式化)。
fn run_post_tool_hooks(call: &ToolCall) {
    let arg = tool_primary_arg(call);
    for h in hooks_for_event(active_hooks(), "post_tool", &call.name) {
        let _ = run_hook_command(&h.command, &call.name, &arg);
    }
}

/// 审计行格式(纯函数,不含时间戳 —— 时间戳由 [`audit`] 落盘时前置,保持本函数确定性可测)。
pub fn audit_line(event: &str, detail: &str) -> String {
    if detail.is_empty() {
        format!("[{event}]")
    } else {
        format!("[{event}] {detail}")
    }
}

/// 会话审计留痕(内置 hook,总是开):事件追加进 `~/.ridge/audit.log`(前置 epoch 秒)。best-effort。
fn audit(event: &str, detail: &str) {
    let Some(home) = std::env::var("USERPROFILE")
        .ok()
        .or_else(|| std::env::var("HOME").ok())
    else {
        return;
    };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = format!("{home}/.ridge/audit.log");
    if let Some(dir) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{ts} {}", audit_line(event, detail));
    }
}

/// 触发会话级 hook(`session_start` / `stop`):内置审计 + config 声明的会话 hook;`stop` 且 notify 开则响铃。
/// `detail` 进审计行(如 stop 带步数)。供 main/tui 生命周期调。
pub fn fire_session_hooks(event: &str, detail: &str) {
    audit(event, detail);
    for h in hooks_for_event(active_hooks(), event, "") {
        let _ = run_hook_command(&h.command, event, detail);
    }
    if event == "stop" && NOTIFY.load(std::sync::atomic::Ordering::Relaxed) {
        eprint!("\x07"); // 终端铃:任务完成通知(内置 hook)
        use std::io::Write;
        let _ = std::io::stderr().flush();
    }
}

/// 执行一个结构化工具调用,返回给模型看的观察结果(observation)。用真实的 `tools` crate 干活。
/// iter-40:前后各串一层 hook(pre_tool 可拦截 / post_tool fire-and-forget)。
pub fn execute_tool_call(call: &ToolCall) -> String {
    // pre_tool hook(iter-40):blocking hook 拒绝 → 不执行工具。
    if let Some(blocked) = run_pre_tool_hooks(call) {
        return blocked;
    }
    let arg = |k: &str| call.arguments.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let obs = match call.name.as_str() {
        "run_shell" => {
            let cmd = arg("cmd");
            // 危险命令拦截:即使用户批准也拒绝(无沙箱阶段的安全硬门槛)。
            if let Some(why) = tools::is_dangerous_command(cmd) {
                return format!("BLOCKED (dangerous: {why}) —— 拒绝执行 `{cmd}`");
            }
            // 约束守卫:删/清空受保护路径(测试)→ 拒(防奖励黑客)。
            if let Some(m) = constraint_guard_shell(cmd) {
                return m;
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
            // 约束守卫:往受保护路径(测试)写空内容 = 清空测试 → 拒(防奖励黑客)。
            if let Some(m) = constraint_guard_write(arg("path"), contents) {
                return m;
            }
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
        "signal_write" => {
            let resolve = arg("resolve");
            if !resolve.is_empty() {
                return match signal_resolve(SIGNALS_DIR, resolve) {
                    Ok(true) => format!("signal resolved: {resolve}"),
                    Ok(false) => format!("signal 未找到: {resolve}"),
                    Err(e) => format!("signal error: {e}"),
                };
            }
            let body = arg("body");
            if body.is_empty() {
                return "signal error: 缺少 body".to_string();
            }
            let kind = if arg("type").is_empty() {
                "note"
            } else {
                arg("type")
            };
            match signal_create(SIGNALS_DIR, kind, body, "manual") {
                Ok(id) => format!("signal recorded: {id}"),
                Err(e) => format!("signal error: {e}"),
            }
        }
        // 未知/幻觉工具名:归一化为 **error**(含 " error:" → 喂失败信号 + 熔断计数),
        // 并提示只调系统所列工具。此前回 "unknown tool" 不含判据词 → 幻觉工具静默空转不计错。
        other => format!("tool error: 未知工具 `{other}`;请只调用系统所列工具"),
    };
    run_post_tool_hooks(call); // post_tool hook(iter-40):工具跑完 fire-and-forget(如写后格式化)。
    obs
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
/// **上下文腐烂**硬上限:压缩后估算 token 仍超此值(2× 压缩阈值)= 单条巨消息压不掉,
/// 继续只会烧预算/降智 → 停机(诊断标签,喂 signal 复利)。
const CONTEXT_ROT_TOKENS: usize = 2 * AUTO_COMPACT_TOKENS;

/// 单条工具观察的字符上限(超则截断)。取值宽,使**仅病态巨型**输出被截,常规输出零影响。
/// head+tail 各半,合计即上限。
const OBS_CHAR_CAP: usize = 8000;

/// 上下文卫生(根因):巨型工具观察入 `history` 前**确定性截断**为 head+tail 预览 + 中缝标记 ——
/// 补 `compact_history`(压多条旧消息)压不掉「单条近消息」的缺口。纯函数、**零丢数据**
/// (磁盘文件不动,可 `read_file` 区间重取)。截断标记刻意避开 verify/durable 判据词
/// (error/failed/exit/BLOCKED/permission),免污染成功/失败/错误信号。
fn bound_observation(obs: String) -> String {
    let total = obs.chars().count();
    if total <= OBS_CHAR_CAP {
        return obs;
    }
    const HEAD: usize = OBS_CHAR_CAP / 2;
    const TAIL: usize = OBS_CHAR_CAP - HEAD;
    let head: String = obs.chars().take(HEAD).collect();
    let tail: String = obs.chars().skip(total - TAIL).collect();
    let dropped = total - HEAD - TAIL;
    format!("{head}\n\n…[截断 {dropped} 字符;完整内容已存盘,可 read_file 指定区间重取]…\n\n{tail}")
}

/// 本地 token 估算(不引 tiktoken):CJK 等非 ASCII 字 ≈ 1 token/字,ASCII ≈ 1 token/4 字符。
/// 口径同仓内 `bin`/`token-count.mjs`。粗但零依赖、确定可测 —— 只用于「要不要压缩」的触发判断。
pub fn est_tokens(text: &str) -> usize {
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
    // 继承信号块(信号复利:上个会话的未决发现),放末尾同 Durable State,有界、仅在有信号时注入。
    if let Some(block) = &s.signal_block {
        msgs.push(Message::new(Role::System, block.clone()));
    }
    // Durable State 事实块放**末尾**(不进冻结的 system prompt):保首部前缀稳定利 Claude 缓存,
    // 又把模型注意力「重锚定」到当前客观事实。仅在有事实时注入,免空噪。
    if let Some(block) = durable_state_block(s) {
        msgs.push(Message::new(Role::System, block));
    }
    msgs
}

/// 上下文是否**腐烂**:按 `to_messages` 同口径做压缩后,历史估算 token 仍超 [`CONTEXT_ROT_TOKENS`]
/// —— 即单条超大消息压不掉(如塞进一个巨型工具输出)。纯函数、离线可测,只在终态分类时算一次。
fn context_rotted(s: &AgentState) -> bool {
    let raw: usize = s.history.iter().map(|m| est_tokens(&m.content)).sum();
    if raw <= AUTO_COMPACT_TOKENS {
        return false; // 未触发压缩,必然 ≤ 压缩阈值 < 硬上限
    }
    let compacted = compact_history(s.history.clone(), AUTO_COMPACT_KEEP);
    compacted
        .iter()
        .map(|m| est_tokens(&m.content))
        .sum::<usize>()
        > CONTEXT_ROT_TOKENS
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

// ───────────────────────── 信号复利(多 loop 共享大脑)─────────────────────────
// 「标准存储库」不止审计留痕:一个会话探测到的事实(发现/摩擦/待办)落成结构化 signal,
// 下个会话**自动继承**之 —— 解 agent「每会话冷启动、重新学项目」的根本损耗。这是把 agent
// 从孤立脚本升为跨会话复利系统的心脏(证据研判 iter-15:单二进制单用户下证据最硬的差异化长板)。

/// 信号复利:跨会话共享的知识层落盘目录(**项目级**,cwd 本地,像 `.ridge/runs`)。
pub const SIGNALS_DIR: &str = ".ridge/signals";
/// 注入上下文的信号块**硬字符上限** —— 有界,防复利知识膨胀反噬 token 节约成果。
const SIGNALS_BLOCK_MAX: usize = 1200;

/// 一条可跨会话复用的**信号**(发现 / 摩擦点 / 待办)。落盘为带 frontmatter 的 markdown。
#[derive(Clone, Debug, PartialEq)]
pub struct Signal {
    pub id: String,
    pub kind: String,   // frontmatter 里的 `type`(避 Rust 关键字)
    pub status: String, // open / resolved
    pub source: String, // 产它的 run id 或 "manual"
    pub body: String,
}

/// slug 化:留字母数字、小写、其余转 `-`,截 24 字 —— 拼进文件名/id,须文件系统安全。
fn slugify(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let t = out.trim_matches('-');
    if t.is_empty() {
        "signal".to_string()
    } else {
        t.chars().take(24).collect()
    }
}

/// 信号 id = `<slug(kind)>-<内容哈希>`。用 `DefaultHasher`(固定 key、确定性、无时间戳):
/// **同内容 → 同 id** → 天然幂等去重(重复记同一发现不产重复文件),且离线可测。
fn signal_id(kind: &str, body: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    kind.hash(&mut h);
    body.hash(&mut h);
    format!("{}-{:08x}", slugify(kind), h.finish() as u32)
}

fn render_signal(sig: &Signal) -> String {
    format!(
        "---\nid: {}\ntype: {}\nstatus: {}\nsource: {}\n---\n{}\n",
        sig.id,
        sig.kind,
        sig.status,
        sig.source,
        sig.body.trim()
    )
}

/// 解析一份 signal markdown(frontmatter + 正文);缺 id / 格式坏 → `None`。
fn parse_signal(text: &str) -> Option<Signal> {
    let rest = text.strip_prefix("---\n")?;
    let (front, body) = rest.split_once("\n---\n")?;
    let (mut id, mut kind, mut status, mut source) =
        (String::new(), String::new(), String::new(), String::new());
    for line in front.lines() {
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim().to_string();
            match k.trim() {
                "id" => id = v,
                "type" => kind = v,
                "status" => status = v,
                "source" => source = v,
                _ => {}
            }
        }
    }
    if id.is_empty() {
        return None;
    }
    if status.is_empty() {
        status = "open".to_string();
    }
    Some(Signal {
        id,
        kind,
        status,
        source,
        body: body.trim().to_string(),
    })
}

/// **产者**:把一条 `open` 信号落盘 `dir/<id>.md`(同内容 id 相同 → 幂等去重)。返回 id。
pub fn signal_create(
    dir: impl AsRef<std::path::Path>,
    kind: &str,
    body: &str,
    source: &str,
) -> std::io::Result<String> {
    let dir = dir.as_ref();
    std::fs::create_dir_all(dir)?;
    let id = signal_id(kind, body);
    let sig = Signal {
        id: id.clone(),
        kind: kind.to_string(),
        status: "open".to_string(),
        source: source.to_string(),
        body: body.to_string(),
    };
    std::fs::write(dir.join(format!("{id}.md")), render_signal(&sig))?;
    Ok(id)
}

/// **消解**:把 `dir` 里 id 匹配的信号 status 改 `resolved`(闭环,免下轮重复消费)。找不到 → `Ok(false)`。
pub fn signal_resolve(dir: impl AsRef<std::path::Path>, id: &str) -> std::io::Result<bool> {
    let path = dir.as_ref().join(format!("{id}.md"));
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(false);
    };
    let Some(mut sig) = parse_signal(&text) else {
        return Ok(false);
    };
    if sig.status != "resolved" {
        sig.status = "resolved".to_string();
        std::fs::write(&path, render_signal(&sig))?;
    }
    Ok(true)
}

/// **消费者**:读 `dir` 下全部 signal,取 `status=open`,按 id 排序(有序稳态、利缓存/确定性)。
pub fn load_open_signals(dir: impl AsRef<std::path::Path>) -> Vec<Signal> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<Signal> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|t| parse_signal(&t))
        .filter(|s| s.status == "open")
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// 把 open 信号编成**有界**注入块(超 [`SIGNALS_BLOCK_MAX`] 截断,防复利知识膨胀)。无 → `None`。
fn signals_block(sigs: &[Signal]) -> Option<String> {
    if sigs.is_empty() {
        return None;
    }
    let mut b = String::from(
        "<inherited_signals>\n上个会话留下的未决信号;处理完请调 signal_write(resolve=<id>)消解:\n",
    );
    for s in sigs {
        let line = format!("- [{}] ({}) {}\n", s.id, s.kind, s.body.replace('\n', " "));
        if b.len() + line.len() > SIGNALS_BLOCK_MAX {
            b.push_str("- …(更多信号已省略)\n");
            break;
        }
        b.push_str(&line);
    }
    b.push_str("</inherited_signals>");
    Some(b)
}

/// 供 CLI 在建 `AgentState` 前调用:读项目级 `.ridge/signals` 的 open 信号 → 有界注入块。
pub fn load_signal_block() -> Option<String> {
    signals_block(&load_open_signals(SIGNALS_DIR))
}

/// **自动产者**:run 收尾时,失败(非成功停机 / 有报错)自动落一条 `failure` 信号 ——
/// loop engineering「preserve mistakes so the loop can learn」:下个会话开局即继承「上次卡在哪」,
/// 不重蹈覆辙。成功 run 不产噪。同内容幂等去重(反复失败于同处 → 不刷屏)。返回 signal id(无可记 → None)。
pub fn auto_signal_from_run(
    out: &AgentState,
    dir: impl AsRef<std::path::Path>,
    source: &str,
) -> Option<String> {
    let reason = halt_reason(out);
    if reason.is_success() && out.last_error.is_none() {
        return None;
    }
    let task: String = out.task.chars().take(80).collect();
    let mut body = format!("任务未竟: {task} | 停机: {}", reason.as_str());
    if let Some(e) = &out.last_error {
        let e: String = e.chars().take(160).collect();
        body.push_str(&format!(" | 末错: {e}"));
    }
    signal_create(dir, "failure", &body, source).ok()
}

// ─────────── 自动 signal 抽取器(复利环产者的「发现/待办」侧)───────────
// iter-17 的自动产者只记**失败**;本抽取器补另一半:run 收尾用 provider **一次性**把执行轨迹
// 提炼成可跨会话复用的信号(发现/摩擦/待办),喂已建的产→消→解复利环。**opt-in**(env,默认关):
// 尊重 token 北极星,不默认给每轮加一次 LLM 成本;`--every` 常驻用户可开启以求复利。
// (对抗评审 iter-18:采纳「安全内核版」—— 喂已有 signals 环;**驳回**「自动改写 harness」,单用户样本不足、改写无 checker。)

/// 一次抽取最多提炼多少条(宁缺毋滥 + 有界成本/防刷屏)。
const MAX_EXTRACTED_SIGNALS: usize = 5;

const SIGNAL_EXTRACT_SYSTEM: &str =
    "你是复盘助手。从本次执行轨迹提炼**可跨会话复用**的信号,助下个会话免重新摸索。\
只输出新增的、具体的、可复用条目;每行一条,格式严格 `kind: body`,kind ∈ {discovery, friction, todo}:\
discovery=项目事实/结构发现;friction=踩的坑/易错处;todo=本次未竟、下次该做。\
最多 5 条,宁缺毋滥;无可复用信号则只回 NONE。勿复述任务,勿客套。";

/// run 是否有「值得抽取」的实质轨迹(动过工具或改过文件)。纯轻量运行不抽,省一次 LLM 调用。
fn run_has_substance(out: &AgentState) -> bool {
    !out.modified_files.is_empty() || out.messages.iter().any(|m| m.starts_with("act:"))
}

/// 构造抽取请求(有界轨迹 → 提炼复利信号)。无实质轨迹 → `None`(不抽)。
fn signal_extract_request(out: &AgentState) -> Option<CompletionRequest> {
    if !run_has_substance(out) {
        return None;
    }
    let task: String = out.task.chars().take(200).collect();
    // 轨迹有界:复用 bound_observation(head+tail 预览),免巨型轨迹撑爆这一次抽取调用。
    let traj = bound_observation(out.messages.join("\n"));
    Some(CompletionRequest {
        messages: vec![
            Message::new(Role::System, SIGNAL_EXTRACT_SYSTEM),
            Message::new(Role::User, format!("任务:{task}\n\n执行轨迹:\n{traj}")),
        ],
        tools: vec![],
    })
}

/// 解析抽取器输出为 `(kind, body)` 列表。**纯函数**:每行 `kind: body`(冒号中英皆可),
/// kind 须在允许集内、body 非空;`NONE`/空行/不合规行/markdown 项目符号一律容错忽略;上限 [`MAX_EXTRACTED_SIGNALS`]。
fn parse_extracted_signals(text: &str) -> Vec<(String, String)> {
    const ALLOWED: [&str; 3] = ["discovery", "friction", "todo"];
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim().trim_start_matches(['-', '*', '•', ' ']).trim();
        let Some((k, b)) = line.split_once([':', '：']) else {
            continue;
        };
        let kind = k.trim().to_lowercase();
        let body = b.trim();
        if !ALLOWED.contains(&kind.as_str()) || body.is_empty() {
            continue;
        }
        out.push((kind, body.to_string()));
        if out.len() >= MAX_EXTRACTED_SIGNALS {
            break;
        }
    }
    out
}

/// 自动 signal 抽取是否启用(**opt-in**,env `RIDGE_EXTRACT_SIGNALS` = 1/true/on/yes)。
/// 默认关 —— 尊重 token 北极星,不默认给每轮加一次 LLM 成本。
pub fn signal_extract_enabled() -> bool {
    std::env::var("RIDGE_EXTRACT_SIGNALS")
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "on" | "yes"
            )
        })
        .unwrap_or(false)
}

/// **自动抽取器**:run 收尾用 provider 一次性把轨迹提炼成复利信号,落 `dir`(经 `signal_create` 内容哈希
/// **幂等去重**,反复同一发现不刷屏)。返回落盘的 signal id 列表。抽取失败/无所得/无实质轨迹 → 空
/// (best-effort,**绝不掀翻主流程**)。source = 本 run id(溯源回指 `.ridge/runs/<id>`)。
pub async fn extract_signals_from_run(
    provider: &dyn LlmProvider,
    out: &AgentState,
    dir: impl AsRef<std::path::Path>,
    source: &str,
) -> Vec<String> {
    let Some(req) = signal_extract_request(out) else {
        return Vec::new();
    };
    let text = match provider.complete(&req).await {
        Ok(c) => c.text,
        Err(_) => return Vec::new(),
    };
    let dir = dir.as_ref();
    parse_extracted_signals(&text)
        .into_iter()
        .filter_map(|(kind, body)| signal_create(dir, &kind, &body, source).ok())
        .collect()
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
        false,
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
        false,
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
        false,
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
    read_only: bool,
) -> Result<CompiledGraph<AgentState>, GraphError> {
    build_core(
        provider, mcp, None, approver, skills, token_bus, agents, read_only,
    )
}

/// reason 把 内置 + MCP 工具一起 offer 给 LLM,act 按 `<server>__<tool>` 命名空间路由到对应
/// MCP 客户端(否则走内置工具),执行前过权限门,verify 认确定性信号(可选再挂独立模型 reviewer);
/// system prompt 注入 Skills(领域知识)。
#[allow(clippy::too_many_arguments)]
fn build_core(
    provider: Arc<dyn LlmProvider>,
    mcp: McpTools,
    reviewer: Option<Arc<dyn LlmProvider>>,
    approver: Arc<dyn Approver>,
    skills: Vec<Skill>,
    token_bus: TokenBus,
    agents: Arc<Agents>,
    read_only: bool,
) -> Result<CompiledGraph<AgentState>, GraphError> {
    let mut g = StateGraph::<AgentState>::new();
    // 只读模式:不 offer 副作用工具、也不 offer MCP(副作用未知)—— 从源头断写。
    let mut specs = builtin_tool_specs();
    if read_only {
        specs.retain(|s| !is_mutating_tool(&s.name));
    }
    if let Some(d) = dispatch_spec(&agents) {
        specs.push(d); // dispatch_agent 安全(子 agent 恒只读),只读模式也可派
    }
    if !read_only {
        specs.extend(mcp.specs);
    }
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
                    // 只读模式深度防御:副作用工具即使被幻觉调到也硬拒(与 offering 过滤双保险)。
                    let obs = if let Some(m) = read_only_block(read_only, &call.name) {
                        m
                    } else if needs_approval(&call.name)
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
                    // 上下文卫生(根因):巨型工具输出入 history 前确定性截断(head+tail 预览),
                    // 止住单条巨输出撑爆上下文;零丢数据,文件可 read_file 区间重取。所有工具路径汇流此接缝。
                    let obs = bound_observation(obs);
                    // 无进展检测:工具输出与上一轮相同则 stall+1,否则清零。
                    let stall = if s.tool_output.as_deref() == Some(obs.as_str()) {
                        s.stall + 1
                    } else {
                        0
                    };
                    // 熔断计数:本轮观察为错误则 err_streak+1,成功则清零(与 stall 正交,兜「错误每轮不同」)。
                    let err_streak = if is_error_observation(&obs) {
                        s.err_streak + 1
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
                        Patch::SetErrStreak(err_streak),
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

/// 一轮任务的**停机原因**(loop engineering:让「为什么停」成为机器可判的确定性信号,
/// 而非只知道停了)。`Approved`=确定性验证通过(成功);其余三种是护栏熔断(**响亮失败**):
/// `Budget` 超 token 预算、`Stall` 连续无进展、`StepCap` 到硬回合上限;`Unverified`=模型收尾但未获通过。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HaltReason {
    Approved,
    Budget,
    Stall,
    StepCap,
    /// **约束违反**(奖励黑客):试图删/清空受保护路径(测试)等 —— 被守卫硬拦。
    ConstraintBreach,
    /// **上下文腐烂**:压缩后上下文仍超硬上限(单条巨消息压不掉),继续只烧预算/降智。
    ContextRot,
    /// **熔断**:连续工具/provider 报错达 [`MAX_ERR_STREAK`],无人值守下提前停机防烧预算。
    CircuitBroken,
    Unverified,
}

impl HaltReason {
    pub fn as_str(self) -> &'static str {
        match self {
            HaltReason::Approved => "approved",
            HaltReason::Budget => "budget_exceeded",
            HaltReason::Stall => "no_progress",
            HaltReason::StepCap => "step_cap",
            HaltReason::ConstraintBreach => "constraint_breach",
            HaltReason::ContextRot => "context_rot",
            HaltReason::CircuitBroken => "circuit_broken",
            HaltReason::Unverified => "unverified",
        }
    }
    /// 成功(确定性验证通过)才 true;熔断/违约/未验证都是失败,供调用方给非零退出码。
    pub fn is_success(self) -> bool {
        matches!(self, HaltReason::Approved)
    }
}

/// 据终态判定停机原因。优先级(高→低):成功、超预算(经济护栏最该被看见)、**约束违反**(奖励黑客,
/// 安全须显)、**上下文腐烂**(结构性根因)、**熔断**(连错症状)、无进展(输出停滞)、回合上限(通用耗尽)、未验证。
/// 「更根因/更具体者优先」:同为失败终态时,给最有诊断价值的标签(喂 signal 复利)。
pub fn halt_reason(s: &AgentState) -> HaltReason {
    if s.approved {
        HaltReason::Approved
    } else if over_budget(s) {
        HaltReason::Budget
    } else if s
        .last_error
        .as_deref()
        .is_some_and(|e| e.contains("constraint"))
    {
        HaltReason::ConstraintBreach
    } else if context_rotted(s) {
        HaltReason::ContextRot
    } else if circuit_broken(s) {
        HaltReason::CircuitBroken
    } else if stalled(s) {
        HaltReason::Stall
    } else if s.steps >= MAX_STEPS {
        HaltReason::StepCap
    } else {
        HaltReason::Unverified
    }
}

/// 把一轮任务落成**标准存储库**的一条 run:`<run_dir>/manifest.json`(结构化结论:任务/是否通过/
/// 停机原因/步数/token)+ `trace.json`(完整审计轨迹)。相比旧的「cwd 平铺 trace.json 每轮覆盖」,
/// 每 run 独立目录 → 审计历史不再互相冲掉,是 loop engineering 里跨 run 复利的物理底座。
///
/// ponytail: 只落 manifest+trace 这两样**真正被产出**的东西。跨 loop 复利单元 signal 已落地(iter-16),
/// 但存**项目级** `.ridge/signals`(跨 run 共享 → 才复利),非 run 级子目录;溯源靠 signal 的 `source` 字段回指本 run。
pub fn write_run(out: &AgentState, run_dir: impl AsRef<std::path::Path>) -> std::io::Result<()> {
    let dir = run_dir.as_ref();
    std::fs::create_dir_all(dir)?;
    let manifest = serde_json::json!({
        "task": out.task,
        "approved": out.approved,
        "halt_reason": halt_reason(out).as_str(),
        "steps": out.steps,
        "tokens": out.total_tokens,
    });
    let json = serde_json::to_string_pretty(&manifest).map_err(std::io::Error::other)?;
    std::fs::write(dir.join("manifest.json"), json)?;
    write_trace(out, dir.join("trace.json"))
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

    /// 验证器抗奖励黑客:成功信号是**行首前缀** `exit 0:`,而非任意位置的 "exit 0" 子串。
    /// 失败命令(`exit 7:`)正文即便含 "exit 0" 文本,也不得被判成功;真实 `exit 0:` 成功仍认。
    #[test]
    fn tool_output_ok_requires_exit0_prefix_not_substring() {
        assert!(
            tool_output_ok("exit 0: build ok"),
            "真实 exit 0 前缀应算成功"
        );
        assert!(
            !tool_output_ok("exit 7: build failed, expected exit 0 but got 7"),
            "失败命令正文含 'exit 0' 文本不得被误判成功(堵奖励黑客/修正确性 bug)"
        );
        assert!(tool_output_ok("tests: passed"), "结构化 passed 标记仍认");
        assert!(!tool_output_ok("tests: 1 failed"), "failed 不算成功");
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

    /// preset 表结构完好:字段非空、kind 合法、id 唯一、base_url https、含全部要求的 id、条数 ≥ 14。
    #[test]
    fn provider_presets_wellformed() {
        assert!(PROVIDER_PRESETS.len() >= 14);
        let mut ids = std::collections::BTreeSet::new();
        for p in PROVIDER_PRESETS {
            assert!(!p.id.is_empty() && !p.label.is_empty());
            assert!(!p.base_url.is_empty() && !p.default_model.is_empty() && !p.key_env.is_empty());
            assert!(
                p.kind == "openai" || p.kind == "anthropic",
                "bad kind {}",
                p.kind
            );
            assert!(p.base_url.starts_with("https://"), "bad url {}", p.base_url);
            assert!(ids.insert(p.id), "dup id {}", p.id);
        }
        for want in [
            "openai",
            "anthropic",
            "gemini",
            "grok",
            "glm",
            "kimi",
            "deepseek",
            "qwen",
            "openrouter",
            "siliconflow",
            "groq",
        ] {
            assert!(ids.contains(want), "missing preset {want}");
        }
    }

    /// id 查找大小写不敏感;未知 → None;preset → profile 字段对齐且 api_key 恒 None、name/model 可覆盖。
    #[test]
    fn preset_lookup_and_to_profile() {
        let ds = preset_by_id("DeepSeek").expect("deepseek");
        assert!(ds.base_url.contains("deepseek.com"));
        assert!(preset_by_id("nope-vendor").is_none());
        let prof = preset_to_profile(ds, None, None);
        assert_eq!(prof.name, "deepseek");
        assert_eq!(prof.kind, "openai");
        assert_eq!(prof.model, "deepseek-chat");
        assert_eq!(prof.key_env, "DEEPSEEK_API_KEY");
        assert!(prof.api_key.is_none());
        let prof2 = preset_to_profile(ds, Some("work"), Some("deepseek-reasoner"));
        assert_eq!(prof2.name, "work");
        assert_eq!(prof2.model, "deepseek-reasoner");
    }

    /// auth 密钥库往返:写入/覆盖保留余槽、坏文本从空起、产物合法 JSON、可取回。
    #[test]
    fn auth_store_roundtrip() {
        let t1 = auth_upsert("{}", "DEEPSEEK_API_KEY", "sk-a");
        assert_eq!(auth_get(&t1, "DEEPSEEK_API_KEY").as_deref(), Some("sk-a"));
        let t2 = auth_upsert(&t1, "OPENAI_API_KEY", "sk-b");
        assert_eq!(auth_get(&t2, "DEEPSEEK_API_KEY").as_deref(), Some("sk-a")); // 保留
        assert_eq!(auth_get(&t2, "OPENAI_API_KEY").as_deref(), Some("sk-b"));
        let t3 = auth_upsert(&t2, "DEEPSEEK_API_KEY", "sk-c"); // 覆盖
        assert_eq!(auth_get(&t3, "DEEPSEEK_API_KEY").as_deref(), Some("sk-c"));
        // 坏文本从空起,仍产出合法 JSON。
        let t4 = auth_upsert("not json!!", "K", "v");
        assert!(serde_json::from_str::<serde_json::Value>(&t4).is_ok());
        assert_eq!(auth_get(&t4, "K").as_deref(), Some("v"));
        assert!(auth_get(&t4, "MISSING").is_none());
    }

    /// key 解析优先级:内联 api_key > env[key_env] > auth[key_env];皆无 → None。
    /// 用唯一命名的 env 变量避免与并行测试互扰。
    #[test]
    fn resolve_key_precedence_with_auth() {
        use std::collections::BTreeMap;
        // 1) 内联 api_key 压倒一切(env/auth 都不看)。
        let inline = ProviderProfile {
            name: "z".into(),
            kind: "openai".into(),
            model: "m".into(),
            base_url: "u".into(),
            key_env: "RIDGE_ITER37_UNSET".into(),
            api_key: Some(" sk-inline ".into()),
        };
        let mut auth = BTreeMap::new();
        auth.insert("RIDGE_ITER37_UNSET".to_string(), "sk-auth".to_string());
        assert_eq!(inline.resolve_key_with(&auth).as_deref(), Some("sk-inline"));
        // 2) 无内联、env 未设 → 回落 auth。
        let prof = ProviderProfile {
            api_key: None,
            ..inline.clone()
        };
        assert_eq!(prof.resolve_key_with(&auth).as_deref(), Some("sk-auth"));
        // 3) env 设了(唯一名)→ env 压倒 auth。
        let mut prof2 = prof.clone();
        prof2.key_env = "RIDGE_ITER37_ENVWINS".into();
        let mut auth2 = BTreeMap::new();
        auth2.insert("RIDGE_ITER37_ENVWINS".to_string(), "sk-auth".to_string());
        std::env::set_var("RIDGE_ITER37_ENVWINS", "sk-env");
        assert_eq!(prof2.resolve_key_with(&auth2).as_deref(), Some("sk-env"));
        std::env::remove_var("RIDGE_ITER37_ENVWINS");
        // 4) 皆无 → None。
        assert_eq!(prof.resolve_key_with(&BTreeMap::new()), None);
    }

    /// login 纯核:写档进 providers[]、make_default 时改顶层四键、**产物绝不含任何 key**、合法 JSON。
    #[test]
    fn apply_login_writes_profile_no_key() {
        let ds = preset_by_id("deepseek").unwrap();
        // make_default=true:providers 有档 + 顶层指向 deepseek。
        let out = apply_login("{}", ds, None, None, true).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["provider"], "openai");
        assert_eq!(v["model"], "deepseek-chat");
        assert_eq!(v["base_url"], "https://api.deepseek.com/v1");
        assert_eq!(v["key_env"], "DEEPSEEK_API_KEY");
        let prov = &v["providers"][0];
        assert_eq!(prov["name"], "deepseek");
        assert_eq!(prov["base_url"], "https://api.deepseek.com/v1");
        assert_eq!(prov["key_env"], "DEEPSEEK_API_KEY");
        assert!(!out.contains("api_key")); // 铁律:key 永不进 config
                                           // make_default=false:不动顶层,只加档。
        let out2 = apply_login("{}", ds, Some("work"), Some("deepseek-reasoner"), false).unwrap();
        let v2: serde_json::Value = serde_json::from_str(&out2).unwrap();
        assert!(v2.get("provider").is_none());
        assert_eq!(v2["providers"][0]["name"], "work");
        assert_eq!(v2["providers"][0]["model"], "deepseek-reasoner");
        // make_default 抹掉预存顶层 api_key(否则旧 key 配新端点认证错乱)。
        let prev = r#"{"provider":"openai","api_key":"stale-key","base_url":"https://old"}"#;
        let out3 = apply_login(prev, ds, None, None, true).unwrap();
        assert!(!out3.contains("stale-key"));
        assert!(!out3.contains("api_key"));
        let v3: serde_json::Value = serde_json::from_str(&out3).unwrap();
        assert_eq!(v3["key_env"], "DEEPSEEK_API_KEY");
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

    /// 停机原因分类:据终态确定性判定,优先级 成功 > 预算 > 无进展 > 回合上限 > 未验证。
    #[test]
    fn halt_reason_classifies_each_outcome() {
        let approved = AgentState {
            approved: true,
            ..Default::default()
        };
        assert_eq!(halt_reason(&approved), HaltReason::Approved);
        assert!(halt_reason(&approved).is_success());

        let budget = AgentState {
            budget_tokens: 100,
            total_tokens: 100,
            ..Default::default()
        };
        assert_eq!(halt_reason(&budget), HaltReason::Budget);

        let stall = AgentState {
            stall: MAX_STALL,
            ..Default::default()
        };
        assert_eq!(halt_reason(&stall), HaltReason::Stall);

        let cap = AgentState {
            steps: MAX_STEPS,
            ..Default::default()
        };
        assert_eq!(halt_reason(&cap), HaltReason::StepCap);

        // 约束违反(奖励黑客)由 last_error 分类,优先于回合上限。
        let breach = AgentState {
            steps: MAX_STEPS,
            last_error: Some("BLOCKED (constraint): 拒绝清空受保护路径".into()),
            ..Default::default()
        };
        assert_eq!(halt_reason(&breach), HaltReason::ConstraintBreach);

        // 熔断:连错达阈值 → circuit_broken,优先于回合上限(错误内容每轮不同、stall 不触发时兜底)。
        let circuit = AgentState {
            steps: MAX_STEPS,
            err_streak: MAX_ERR_STREAK,
            ..Default::default()
        };
        assert_eq!(halt_reason(&circuit), HaltReason::CircuitBroken);

        // 上下文腐烂:压缩后单条巨消息仍超硬上限 → context_rot,优先于熔断(结构性根因)。
        let big = "字".repeat(CONTEXT_ROT_TOKENS + 1); // 每 CJK 字≈1tok,单条即超硬上限,压不掉
        let rot = AgentState {
            steps: MAX_STEPS,
            err_streak: MAX_ERR_STREAK,
            history: vec![Message::user(big)],
            ..Default::default()
        };
        assert_eq!(halt_reason(&rot), HaltReason::ContextRot);

        // 熔断/违约都非成功 → 调用方据此给非零退出码。
        for r in [
            HaltReason::Budget,
            HaltReason::Stall,
            HaltReason::StepCap,
            HaltReason::ConstraintBreach,
            HaltReason::ContextRot,
            HaltReason::CircuitBroken,
            HaltReason::Unverified,
        ] {
            assert!(!r.is_success(), "{} 不应算成功", r.as_str());
        }
    }

    /// 上下文腐烂判定:小历史不腐烂;单条超硬上限的巨消息(压不掉)→ 腐烂。
    #[test]
    fn context_rotted_detects_unshrinkable_giant_message() {
        let small = AgentState {
            history: vec![Message::user("短消息")],
            ..Default::default()
        };
        assert!(!context_rotted(&small), "小历史不应判腐烂");

        // 多条普通消息**真的超**压缩阈值(1200×6≈7200tok>6000),但压缩保留尾部 8 条 → 收敛到硬上限内 → 不腐烂。
        let many: Vec<Message> = (0..1200).map(|_| Message::user("噪音消息一段")).collect();
        let compactable = AgentState {
            history: many,
            ..Default::default()
        };
        assert!(!context_rotted(&compactable), "可压缩历史不应判腐烂");

        // 单条巨消息压不掉 → 腐烂。
        let rot = AgentState {
            history: vec![Message::user("字".repeat(CONTEXT_ROT_TOKENS + 1))],
            ..Default::default()
        };
        assert!(context_rotted(&rot), "单条超硬上限的巨消息应判腐烂");
    }

    /// 熔断早停:连续报错累计到 MAX_ERR_STREAK 即命中 must_stop(早于回合上限)。
    #[test]
    fn circuit_breaks_before_step_cap() {
        let broken = AgentState {
            err_streak: MAX_ERR_STREAK,
            steps: 1, // 远未到回合上限
            ..Default::default()
        };
        assert!(circuit_broken(&broken));
        assert!(must_stop(&broken), "连错达阈值应触发停机,不必跑到回合上限");

        let ok = AgentState {
            err_streak: MAX_ERR_STREAK - 1,
            steps: 1,
            ..Default::default()
        };
        assert!(!must_stop(&ok), "未达连错阈值不应停机");
    }

    /// 巨型工具输出确定性截断:超上限 → head+tail 预览;不误伤常规输出;保 verify/durable 判据信号。
    #[test]
    fn bound_observation_truncates_giant_but_preserves_signals() {
        // 未超上限 → 原样(逐字节相等)。
        let small = "短小输出 exit 0: done".to_string();
        assert_eq!(bound_observation(small.clone()), small);

        // 超上限 → 截断:含 head 片段 + tail 片段 + 截断标记,总长有界。
        let giant = format!("HEAD_MARK{}TAIL_MARK", "x".repeat(20000));
        let bounded = bound_observation(giant);
        let n = bounded.chars().count();
        assert!(n <= OBS_CHAR_CAP + 60, "截断后应有界,实际 {n}");
        assert!(bounded.starts_with("HEAD_MARK"), "应保留 head 片段");
        assert!(bounded.ends_with("TAIL_MARK"), "应保留 tail 片段");
        assert!(bounded.contains("截断"), "应含截断标记");

        // 截断标记不含判据词 → 不污染 error/失败信号。
        let plain = bound_observation("平安无事输出".repeat(3000));
        assert!(
            !is_error_observation(&plain),
            "无错巨输出截断后不应被判为错误"
        );
        assert!(!tool_output_failed(&plain), "无错巨输出截断后不应判失败");

        // head 保 `exit 0:` 前缀 → 成功信号存活。
        let okout = bound_observation(format!("exit 0: {}", "y".repeat(20000)));
        assert!(tool_output_ok(&okout), "截断后 exit 0 成功信号应存活");

        // head 保 `exit 7:` → 失败信号存活。
        let failout = bound_observation(format!("exit 7: {}", "z".repeat(20000)));
        assert!(tool_output_failed(&failout), "截断后非零退出失败信号应存活");

        // 相同巨输入 → 截断结果相同(stall 检测不被破坏)。
        let a = bound_observation("同样的巨输出".repeat(3000));
        let b = bound_observation("同样的巨输出".repeat(3000));
        assert_eq!(a, b, "确定性:相同输入截断结果一致");
    }

    /// 标准存储库:一轮任务落成独立 run 目录,含 manifest.json(结构化结论)+ trace.json(完整轨迹)。
    #[test]
    fn write_run_creates_per_run_dir_with_manifest_and_trace() {
        let dir = std::env::temp_dir().join("ridge_write_run_test_1");
        let _ = std::fs::remove_dir_all(&dir); // 清上一次残留,保证干净
        let state = AgentState {
            task: "查天气".into(),
            steps: MAX_STEPS, // 未通过 + 到回合上限 → halt_reason=step_cap
            total_tokens: 42,
            ..Default::default()
        };
        write_run(&state, &dir).unwrap();

        let manifest = dir.join("manifest.json");
        let trace = dir.join("trace.json");
        assert!(manifest.exists(), "manifest.json 应物理生成");
        assert!(trace.exists(), "trace.json 应物理生成");

        let m: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
        assert_eq!(m["task"], "查天气");
        assert_eq!(m["approved"], false);
        assert_eq!(m["halt_reason"], "step_cap");
        assert_eq!(m["steps"], MAX_STEPS);
        assert_eq!(m["tokens"], 42);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 信号复利·产→消:产者落盘 open 信号,消费者读回;同内容 id 相同(幂等去重)。
    #[test]
    fn signal_create_then_load_open_roundtrips() {
        let dir = std::env::temp_dir().join("ridge_signal_test_load");
        let _ = std::fs::remove_dir_all(&dir);

        let id = signal_create(&dir, "friction", "构建慢:cold build 90s", "run-1").unwrap();
        // 幂等:同 type+body 再产一次 → 同 id、不产重复文件。
        let id2 = signal_create(&dir, "friction", "构建慢:cold build 90s", "run-2").unwrap();
        assert_eq!(id, id2, "同内容应得同 id(内容哈希幂等去重)");
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "幂等:只该有一个文件"
        );

        let open = load_open_signals(&dir);
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, id);
        assert_eq!(open[0].kind, "friction");
        assert_eq!(open[0].status, "open");
        assert!(open[0].body.contains("cold build 90s"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 信号复利·消解闭环:resolve 翻 status → 不再被消费者扫入(免下轮重复消费)。
    #[test]
    fn signal_resolve_removes_from_open() {
        let dir = std::env::temp_dir().join("ridge_signal_test_resolve");
        let _ = std::fs::remove_dir_all(&dir);

        let id = signal_create(&dir, "todo", "补 X 的单测", "run-1").unwrap();
        assert_eq!(load_open_signals(&dir).len(), 1);

        assert!(signal_resolve(&dir, &id).unwrap(), "应找到并消解");
        assert!(load_open_signals(&dir).is_empty(), "resolved 不该再被消费");
        assert!(
            !signal_resolve(&dir, "nonexistent-00000000").unwrap(),
            "不存在的 id → false"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 信号注入块**有界**:再多再长信号,块字符数不超硬上限;空信号 → None(不注入)。
    #[test]
    fn signals_block_is_bounded_and_none_when_empty() {
        assert!(signals_block(&[]).is_none(), "无信号不注入");

        let many: Vec<Signal> = (0..200)
            .map(|i| Signal {
                id: format!("sig-{i:08x}"),
                kind: "note".to_string(),
                status: "open".to_string(),
                source: "run-x".to_string(),
                body: format!("一条相当长的信号正文用于撑爆上限 {i} ").repeat(4),
            })
            .collect();
        let block = signals_block(&many).unwrap();
        assert!(
            block.len() <= SIGNALS_BLOCK_MAX + 64,
            "注入块须有界,得 {} 字节",
            block.len()
        );
        assert!(block.contains("…(更多信号已省略)"), "超限应截断并标注");
    }

    /// 消费者接线:state 带 signal_block → to_messages 把它作为 system 消息注入(末尾)。
    #[test]
    fn to_messages_injects_inherited_signal_block() {
        let state = AgentState {
            signal_block: Some(
                "<inherited_signals>\n- [x] (todo) 干这个\n</inherited_signals>".into(),
            ),
            ..Default::default()
        };
        let msgs = to_messages("base system", &state);
        assert!(
            msgs.iter()
                .any(|m| m.role == Role::System && m.content.contains("inherited_signals")),
            "继承信号块应作为 system 消息注入"
        );
    }

    /// 约束守卫抗奖励黑客:删/清空受保护路径(测试)被拦;正常编辑与 `cargo test` 不误伤。
    #[test]
    fn constraint_guard_blocks_test_tampering() {
        // 写臂:清空 tests/ 文件被拦;带内容写放行。
        assert!(
            constraint_guard_write("tests/foo.rs", "").is_some(),
            "清空测试应拦"
        );
        assert!(
            constraint_guard_write("tests/foo.rs", "   \n").is_some(),
            "空白=清空,应拦"
        );
        assert!(
            constraint_guard_write("tests/foo.rs", "fn t() {}").is_none(),
            "带内容的正常编辑不该误伤"
        );
        assert!(
            constraint_guard_write("src/lib.rs", "").is_none(),
            "非保护路径不拦"
        );

        // shell 臂:删 tests/ 被拦;截断重定向进 tests/ 被拦;cargo test / 删源码不误伤。
        assert!(
            constraint_guard_shell("rm tests/foo_test.rs").is_some(),
            "rm 测试应拦"
        );
        assert!(
            constraint_guard_shell("rm -rf tests").is_some(),
            "rm 测试目录应拦"
        );
        assert!(
            constraint_guard_shell("echo '' > tests/foo.rs").is_some(),
            "截断测试应拦"
        );
        assert!(
            constraint_guard_shell("cargo test > out.log").is_none(),
            "cargo test 不该被误伤(tests 复数目录才拦)"
        );
        assert!(
            constraint_guard_shell("rm src/tmp.rs").is_none(),
            "删源码非本守卫职责(jail 管边界)"
        );
    }

    /// 自动产者:失败 run 落 failure 信号(preserve mistakes);成功 run 不产噪。
    #[test]
    fn auto_signal_records_failures_only() {
        let dir = std::env::temp_dir().join("ridge_auto_signal_test");
        let _ = std::fs::remove_dir_all(&dir);

        // 成功 run → 不产信号。
        let ok = AgentState {
            approved: true,
            task: "任务甲".into(),
            ..Default::default()
        };
        assert!(
            auto_signal_from_run(&ok, &dir, "run-ok").is_none(),
            "成功不该产噪"
        );

        // 失败 run(到回合上限未通过)→ 落一条 failure 信号,含任务与停机原因。
        let bad = AgentState {
            task: "任务乙".into(),
            steps: MAX_STEPS,
            last_error: Some("build error: E0433".into()),
            ..Default::default()
        };
        let id = auto_signal_from_run(&bad, &dir, "run-bad").expect("失败应产信号");
        let open = load_open_signals(&dir);
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, id);
        assert_eq!(open[0].kind, "failure");
        assert!(
            open[0].body.contains("任务乙") && open[0].body.contains("step_cap"),
            "body 应含任务名 + 停机原因 step_cap:{}",
            open[0].body
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 抽取器解析:合规 `kind: body` 收下,NONE/不合规 kind/空 body/项目符号一律容错,上限截断。
    #[test]
    fn parse_extracted_signals_filters_and_caps() {
        let text = "NONE\n\
            discovery: 构建脚本在 crates/tools\n\
            - friction: MCP 路径需对服务器可见\n\
            todo：中文冒号也认\n\
            garbage: 不在允许集\n\
            discovery:   \n\
            随便一行没冒号\n\
            todo: 甲\ndiscovery: 乙\nfriction: 丙\ntodo: 丁(第6条应被截断)";
        let got = parse_extracted_signals(text);
        // 允许集内 + body 非空的:discovery(构建)、friction(MCP)、todo(中文冒号)、todo(甲)、discovery(乙) = 上限 5 条。
        assert_eq!(got.len(), MAX_EXTRACTED_SIGNALS);
        assert_eq!(
            got[0],
            ("discovery".into(), "构建脚本在 crates/tools".into())
        );
        assert_eq!(got[1].0, "friction");
        assert_eq!(got[2], ("todo".into(), "中文冒号也认".into()));
        // garbage/空 body/无冒号行被过滤;第 6 条被截断。
        assert!(got
            .iter()
            .all(|(k, _)| ["discovery", "friction", "todo"].contains(&k.as_str())));
        assert!(parse_extracted_signals("NONE").is_empty());
    }

    /// 抽取门:无实质轨迹(未动工具/未改文件)→ 不抽(省 LLM 调用);有 act/改文件 → 构造请求。
    #[test]
    fn extract_request_gated_on_substance() {
        let empty = AgentState {
            task: "查天气".into(),
            ..Default::default()
        };
        assert!(!run_has_substance(&empty));
        assert!(signal_extract_request(&empty).is_none());

        let worked = AgentState {
            task: "改代码".into(),
            messages: vec!["act: edit_file -> edited src/x.rs".into()],
            ..Default::default()
        };
        assert!(run_has_substance(&worked));
        assert!(signal_extract_request(&worked).is_some());
    }

    /// 抽取器端到端:假 provider 回 canned 轨迹提炼 → 解析 → 落盘为 open 信号(幂等去重)。
    #[tokio::test]
    async fn extract_signals_from_run_writes_parsed_signals() {
        use provider::{Completion, ScriptedProvider};
        let dir = std::env::temp_dir().join("ridge_extract_test");
        let _ = std::fs::remove_dir_all(&dir);

        let out = AgentState {
            task: "重构 tools".into(),
            messages: vec!["act: edit_file -> edited crates/tools/src/lib.rs".into()],
            ..Default::default()
        };
        let canned = "discovery: Edit 结构字段 path/old/new 皆 pub\nfriction: apply_edits 原子性,任一越狱整批拒\nNONE";
        let provider = ScriptedProvider::new(vec![Completion {
            text: canned.into(),
            ..Default::default()
        }]);

        let ids = extract_signals_from_run(&provider, &out, &dir, "run-xyz").await;
        assert_eq!(ids.len(), 2, "应落 2 条(discovery+friction)");
        let open = load_open_signals(&dir);
        assert_eq!(open.len(), 2);
        assert!(open
            .iter()
            .any(|s| s.kind == "discovery" && s.source == "run-xyz"));
        assert!(open.iter().any(|s| s.kind == "friction"));

        // 幂等:同一 provider 输出再抽一次 → 内容哈希 id 相同,不新增文件。
        let provider2 = ScriptedProvider::new(vec![Completion {
            text: canned.into(),
            ..Default::default()
        }]);
        let ids2 = extract_signals_from_run(&provider2, &out, &dir, "run-xyz").await;
        assert_eq!(ids2, ids, "同内容幂等,id 一致");
        assert_eq!(load_open_signals(&dir).len(), 2, "幂等不新增");

        let _ = std::fs::remove_dir_all(&dir);
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

    /// iter-39:命令 md 解析 + `$ARGS` 展开。
    #[test]
    fn command_parse_and_expand() {
        let c = parse_command_md(
            "---\ndescription: review code\n---\nReview $ARGS for bugs.",
            "review",
        );
        assert_eq!(c.name, "review");
        assert_eq!(c.description, "review code");
        assert_eq!(c.body, "Review $ARGS for bugs.");
        assert_eq!(
            expand_command(&c.body, "src/x.rs"),
            "Review src/x.rs for bugs."
        );
        // 无 frontmatter → 全文 body、空描述;`desc:` 简写亦可。
        let c2 = parse_command_md("just do it", "go");
        assert_eq!(c2.description, "");
        assert_eq!(c2.body, "just do it");
        assert_eq!(
            parse_command_md("---\ndesc: x\n---\nB", "n").description,
            "x"
        );
        // 无 $ARGS:有 args → 追加,无 args → 原样。
        assert_eq!(expand_command("do the thing", "now"), "do the thing\n\nnow");
        assert_eq!(expand_command("do the thing", "  "), "do the thing");
    }

    /// iter-39:命令目录扫描 + skill 合并(文件命令优先于同名 skill)+ 查找。
    #[test]
    fn load_commands_merges_files_and_skills() {
        let mut dir = std::env::temp_dir();
        dir.push(format!("ridge_cmds_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("deploy.md"),
            "---\ndesc: ship it\n---\nDeploy $ARGS",
        )
        .unwrap();
        let skills = vec![
            Skill {
                name: "cooking".into(),
                description: "pasta".into(),
                body: "boil".into(),
            },
            Skill {
                name: "deploy".into(),
                description: "SKILL dup".into(),
                body: "shadowed".into(),
            },
        ];
        let cmds = load_commands(&dir, &skills);
        let deploy = resolve_command("deploy", &cmds).expect("deploy");
        assert_eq!(deploy.description, "ship it"); // 文件优先,非 skill
        assert_eq!(deploy.body, "Deploy $ARGS");
        assert!(resolve_command("cooking", &cmds).is_some()); // skill 命令
        assert!(resolve_command("nope", &cmds).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// iter-40:hook 事件+matcher 过滤 + 审计行格式。
    #[test]
    fn hooks_for_event_filters() {
        let cfg = Config::parse(
            r#"{"hooks":[
                {"event":"pre_tool","matcher":"run_shell","command":"guard.sh","blocking":true},
                {"event":"post_tool","command":"fmt.sh"},
                {"event":"stop","command":"notify.sh"}
            ]}"#,
        );
        assert_eq!(cfg.hooks.len(), 3);
        // pre_tool + matcher:命中 run_shell、不命中 read_file。
        let pre = hooks_for_event(&cfg.hooks, "pre_tool", "run_shell");
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0].blocking, Some(true));
        assert!(hooks_for_event(&cfg.hooks, "pre_tool", "read_file").is_empty());
        // post_tool 无 matcher → 匹配任意工具。
        assert_eq!(
            hooks_for_event(&cfg.hooks, "post_tool", "write_file").len(),
            1
        );
        // stop 会话事件命中;无声明的事件为空。
        assert_eq!(hooks_for_event(&cfg.hooks, "stop", "").len(), 1);
        assert!(hooks_for_event(&cfg.hooks, "session_start", "").is_empty());
    }

    /// iter-40:审计行格式(纯,无时间戳)。
    #[test]
    fn audit_line_format() {
        assert_eq!(audit_line("session_start", ""), "[session_start]");
        assert_eq!(audit_line("stop", "steps=4"), "[stop] steps=4");
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

    /// harness-aware 系统提示词:把 iter-17/19/20 后新成的**物理契约**讲给模型 ——
    /// 输出截断(用 ranged read)、勿删测试(被拦=失败)、signal_write 沉淀复利。
    #[test]
    fn base_system_states_harness_contract() {
        assert!(BASE_SYSTEM.contains("truncated"), "应告知输出被截断");
        assert!(BASE_SYSTEM.contains("ranged read_file"), "应导向分段读");
        assert!(BASE_SYSTEM.contains("delete or empty"), "应禁删/清空测试");
        assert!(BASE_SYSTEM.contains("signal_write"), "应鼓励沉淀复利信号");
    }

    /// 工具调用鲁棒:未知/幻觉工具名归一化为 error(喂失败信号 + 熔断计数),不再静默空转。
    #[test]
    fn unknown_tool_is_error_classified() {
        let call = ToolCall {
            id: "x".into(),
            name: "definitely_not_a_tool".into(),
            arguments: serde_json::json!({}),
        };
        let obs = execute_tool_call(&call);
        assert!(obs.contains("未知工具"), "应指出未知工具:{obs}");
        assert!(
            is_error_observation(&obs),
            "未知工具应被判为 error(喂熔断/失败信号)"
        );
        assert!(tool_output_failed(&obs), "未知工具应算失败信号");
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

    /// iter-34:地址越狱决策纯函数 —— 开则放行 cwd 外,关则拦。测显式 bool,**不翻全局**(免污染并行的 jail_blocks 测试)。
    #[test]
    fn jail_guard_allows_when_on_blocks_when_off() {
        let outside = std::env::temp_dir().join("ridge_jailbreak_probe.txt");
        let p = outside.to_str().unwrap();
        assert!(jail_guard(true, p).is_ok(), "越狱开:放行 cwd 外写");
        let blocked = jail_guard(false, p);
        assert!(
            blocked.is_err() && blocked.unwrap_err().contains("BLOCKED"),
            "越狱关:cwd 外写仍拦"
        );
    }

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

    /// 只读模式:装配时从 offering 里滤掉副作用工具,只留读/查/研究类。
    #[test]
    fn read_only_filters_out_mutating_tools() {
        let ro: Vec<String> = builtin_tool_specs()
            .into_iter()
            .filter(|s| !is_mutating_tool(&s.name))
            .map(|s| s.name)
            .collect();
        for m in ["run_shell", "write_file", "edit_file", "apply_edits"] {
            assert!(!ro.contains(&m.to_string()), "只读不应 offer {m}");
        }
        for r in [
            "read_file",
            "search",
            "web_search",
            "fetch_url",
            "todo_write",
        ] {
            assert!(ro.contains(&r.to_string()), "只读应保留 {r}");
        }
    }

    /// 只读模式深度防御:只拦副作用工具,读类放行;非只读一律不拦。
    #[test]
    fn read_only_block_rejects_mutating_only() {
        assert!(read_only_block(true, "write_file").is_some());
        assert!(read_only_block(true, "run_shell").is_some());
        assert!(read_only_block(true, "read_file").is_none());
        assert!(read_only_block(false, "write_file").is_none());
        assert!(read_only_block(true, "edit_file")
            .unwrap()
            .starts_with("BLOCKED (read-only)"));
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

    /// iter-24:Best-of-N 确定性评分 —— approved 压倒一切,同侪省 token 者胜。
    #[test]
    fn branch_score_prefers_approved_then_cheap() {
        let mut approved_pricey = AgentState::new("t");
        approved_pricey.approved = true;
        approved_pricey.total_tokens = 50_000;
        let mut rejected_cheap = AgentState::new("t");
        rejected_cheap.approved = false;
        rejected_cheap.total_tokens = 10;
        // approved(哪怕贵)恒胜未 approved(哪怕便宜)。
        assert!(branch_score(&approved_pricey) > branch_score(&rejected_cheap));
        // 双 approved:省 token 者胜。
        let mut approved_cheap = AgentState::new("t");
        approved_cheap.approved = true;
        approved_cheap.total_tokens = 100;
        assert!(branch_score(&approved_cheap) > branch_score(&approved_pricey));
    }
}
