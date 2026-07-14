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

use std::collections::HashMap;
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

/// 只读工具不需要批准(read_file / search 只读本地;web_search / fetch_url 只读公共网页)。
fn needs_approval(tool: &str) -> bool {
    !matches!(tool, "read_file" | "search" | "web_search" | "fetch_url")
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

/// 通用 agent 的基础 system prompt(不再只面向编码)。
const BASE_SYSTEM: &str = "You are a capable agent. Use the provided tools to accomplish the \
     user's task. To change existing files, prefer edit_file (surgical, unique-match replace) over \
     rewriting the whole file with write_file; use search and ranged read_file to explore before \
     editing. For external/real-time info, web_search to find links then fetch_url to read the \
     actual page — trust the page text, not just the snippet. When there is an objective way to \
     verify (compiler exit code, tests), rely on it and don't trust your own claim. When done, stop.";

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
            description: "联网搜索:自动探测网络环境(能否直连国际网络/是否在 GFW 内)并据此选引擎(直连→DuckDuckGo,受限→Bing 中国版),返回标题/链接/摘要。查实时信息或外部资料用它。注意:query 会发给外部搜索引擎".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}),
        },
        ToolSpec {
            name: "fetch_url".to_string(),
            description: "抓取一个网页并返回**可读正文**(去脚本/样式/标签)。配合 web_search:先搜到链接,再用它读正文、据原文作答,别只凭摘要猜".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}),
        },
    ]
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
            let contents = arg("contents");
            match tools::write_file(arg("path"), contents) {
                Ok(()) => format!("wrote {} bytes to {}", contents.len(), arg("path")),
                Err(e) => format!("write error: {e}"),
            }
        }
        "edit_file" => match tools::edit_file(arg("path"), arg("old_string"), arg("new_string")) {
            Ok(()) => format!("edited {}", arg("path")),
            Err(e) => format!("edit error: {e}"),
        },
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
        other => format!("unknown tool `{other}`"),
    }
}

/// 把当前状态铺成给 provider 的消息序列:system(含注入的技能)+ **真实多轮 history**
/// (user / assistant(带 tool_calls) / role=tool 结果),而非把轨迹当 assistant 文本糊上去。
fn to_messages(system: &str, s: &AgentState) -> Vec<Message> {
    let mut msgs = vec![Message::new(Role::System, system)];
    msgs.extend(s.history.iter().cloned());
    msgs
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
    build_core(provider, mcp, None, Arc::new(AutoApprove), Vec::new())
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
    )
}

/// 带**权限门**的装配:有副作用的工具执行前过 [`Approver`](REPL 用它做 y/n 确认)。
pub fn build_llm_agent_gated(
    provider: Arc<dyn LlmProvider>,
    mcp: McpTools,
    approver: Arc<dyn Approver>,
) -> Result<CompiledGraph<AgentState>, GraphError> {
    build_core(provider, mcp, None, approver, Vec::new())
}

/// **全装配**(模块化框架):MCP 工具 + 权限门 + 声明式 Skills(注入 system prompt)。CLI 用它。
pub fn build_llm_agent_full(
    provider: Arc<dyn LlmProvider>,
    mcp: McpTools,
    approver: Arc<dyn Approver>,
    skills: Vec<Skill>,
) -> Result<CompiledGraph<AgentState>, GraphError> {
    build_core(provider, mcp, None, approver, skills)
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
) -> Result<CompiledGraph<AgentState>, GraphError> {
    let mut g = StateGraph::<AgentState>::new();
    let mut specs = builtin_tool_specs();
    specs.extend(mcp.specs);
    let router = Arc::new(mcp.router);
    let system = Arc::new(build_system_prompt(&skills));

    let provider_c = provider.clone();
    let system_c = system.clone();
    g.add_node("reason", move |s: AgentState| {
        let provider = provider_c.clone();
        let tools = specs.clone();
        let system = system_c.clone();
        async move {
            let req = CompletionRequest {
                messages: to_messages(&system, &s),
                tools,
            };
            let completion = provider.complete(&req).await?;
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
    g.add_node("act", move |s: AgentState| {
        let router = router_c.clone();
        let approver = approver_c.clone();
        let fetch = fetch_c.clone();
        let net = net_c.clone();
        async move {
            let patch = match &s.pending_call {
                Some(call) => {
                    // 权限门:有副作用的工具执行前征询批准。
                    let obs = if needs_approval(&call.name)
                        && !approver.approve(&call.name, &preview_call(call))
                    {
                        format!("permission denied by user: {}", call.name)
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
                    Patch::Batch(vec![
                        Patch::Message(format!("act: {} -> {}", call.name, obs)),
                        // 工具结果按 role=tool 正确回灌(匹配 tool_call_id)。
                        Patch::PushHistory(Message::tool_result(call.id.clone(), obs.clone())),
                        Patch::SetStall(stall),
                        Patch::ToolOutput(Some(obs)),
                        Patch::PendingCall(None),
                    ])
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
    let dropped = history.len() - keep - 1;
    let mut out = Vec::with_capacity(keep + 2);
    out.push(history[0].clone()); // 原始任务
    out.push(Message::user(format!(
        "[上下文已压缩:省略 {dropped} 条早期消息]"
    )));
    out.extend(history[history.len() - keep..].iter().cloned());
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
        let mut path = std::env::temp_dir();
        path.push("ridge_llm_toolcall.txt");
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
        let mut path = std::env::temp_dir();
        path.push("ridge_llm_edit.txt");
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
