use crate::communication::{
    in_process_exchange, AgentEnvelope, AgentError, AgentHello, AgentMessage, AgentProtocolError,
    AgentResponse, AgentRole, AgentStatus, AgentTask,
};
use crate::exec::{builtin_tool_specs, execute_tool_call};
use crate::route::{choose_route, ModelProfile, RouteDecision, RouteRequest, RouteRole};
use provider::{CompletionRequest, LlmProvider, Message, Role, ToolCall, ToolSpec};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

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
    out.extend(load_command_files(&dir));
    for s in skills {
        if !out.iter().any(|c| c.name == s.name) {
            out.push(SlashCommand {
                name: s.name.clone(),
                description: s.description.clone(),
                body: s.body.clone(),
            });
        }
    }
    // 内置命令(如 /init)垫底:用户文件命令与 skill 同名可覆盖。
    for (name, text) in BUILTIN_COMMANDS {
        if !out.iter().any(|c| c.name == *name) {
            out.push(parse_command_md(text, name));
        }
    }
    out
}

fn load_command_files(dir: impl AsRef<std::path::Path>) -> Vec<SlashCommand> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if !stem.is_empty() {
                out.push(parse_command_md(&text, stem));
            }
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

/// 内置斜杠命令(name, md 文本)。与 skill 不同:**只进命令表,不常驻 system prompt**(一次性动作,常驻是浪费)。
const BUILTIN_COMMANDS: &[(&str, &str)] = &[("init", include_str!("builtin/commands/init.md"))];

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

/// 读全局规则(`global`,如 `~/.ridge/AGENTS.md`)与 cwd 的项目规则文件(CLAUDE.md / AGENTS.md),
/// 拼成一个"技能"注入 system prompt。全局先、项目后(项目更具体,可覆盖全局)。都不存在 → None。
/// 不向上递归(YAGNI):cwd 只看当前工作目录。
pub fn load_project_rules(global: Option<&std::path::Path>) -> Option<Skill> {
    let mut body = String::new();
    let mut push = |label: &str, t: &str| {
        if !t.trim().is_empty() {
            body.push_str(&format!("\n<!-- {label} -->\n{}\n", t.trim()));
        }
    };
    if let Some(t) = global.and_then(|g| std::fs::read_to_string(g).ok()) {
        push("全局规则", &t);
    }
    for f in ["CLAUDE.md", "AGENTS.md"] {
        if let Ok(t) = std::fs::read_to_string(f) {
            push(f, &t);
        }
    }
    (!body.is_empty()).then(|| Skill {
        name: "项目规则".to_string(),
        description: "全局(~/.ridge)与本仓库(CLAUDE.md / AGENTS.md)的规则约定,须遵守".to_string(),
        body,
    })
}

/// sub-agent 注册表:定义列表 + 命名 provider(name → 已建 provider)。
#[derive(Default)]
pub struct Agents {
    pub defs: Vec<Agent>,
    pub providers: HashMap<String, Arc<dyn LlmProvider>>,
    /// Only credential-resolvable profiles enter this registry.
    pub route_candidates: Vec<AgentProvider>,
}

/// A usable provider handle paired with non-secret routing metadata.
pub struct AgentProvider {
    pub profile: ModelProfile,
    pub provider: Arc<dyn LlmProvider>,
}

pub struct RoutedProvider {
    pub provider: Arc<dyn LlmProvider>,
    pub decision: RouteDecision,
}

impl Agents {
    /// Select a usable provider deterministically. If preferences cannot be
    /// satisfied, retry selection without them before using the caller's main
    /// provider; the decision always explains which fallback occurred.
    pub fn select_provider(
        &self,
        request: &RouteRequest,
        fallback: Arc<dyn LlmProvider>,
    ) -> RoutedProvider {
        let profiles: Vec<ModelProfile> = self
            .route_candidates
            .iter()
            .map(|candidate| candidate.profile.clone())
            .collect();
        let mut decision = choose_route(request, &profiles);
        if decision.selected.is_none()
            && (request.preferred_provider.is_some() || request.preferred_model.is_some())
        {
            let mut fallback_decision = choose_route(&request.without_preferences(), &profiles);
            if fallback_decision.selected.is_some() {
                fallback_decision.used_fallback = true;
                fallback_decision.reason = format!(
                    "{}; explicit preference unavailable, automatic fallback: {}",
                    decision.reason, fallback_decision.reason
                );
                decision = fallback_decision;
            }
        }
        if let Some(selected) = decision.selected.as_ref() {
            if let Some(candidate) = self
                .route_candidates
                .iter()
                .find(|candidate| candidate.profile.key() == selected.key())
            {
                tracing::debug!(
                    selected = %selected.key(),
                    fallback = decision.used_fallback,
                    reason = %decision.reason,
                    "agent route decision"
                );
                return RoutedProvider {
                    provider: candidate.provider.clone(),
                    decision,
                };
            }
        }
        decision.used_fallback = true;
        decision.reason = format!(
            "{}; no usable routed handle, caller main provider fallback",
            decision.reason
        );
        tracing::debug!(
            fallback = true,
            reason = %decision.reason,
            "agent route decision"
        );
        RoutedProvider {
            provider: fallback,
            decision,
        }
    }
}

/// sub-agent 允许的**只读**工具(不下放写/改/shell,免绕过主 agent 的权限门)。
const READONLY_TOOLS: &[&str] = &["read_file", "search"];

/// sub-agent 步数上限(只读检索)。旧值 8 对真实仓库的多文件侦察偏紧;提到 15 仍有界、恒只读故低风险。
const SUBAGENT_MAX_STEPS: usize = 15;
const DEFAULT_SUBAGENT_TIMEOUT_SECS: u64 = 45;
const MAX_PARALLEL_SUBAGENTS: usize = 3;

fn subagent_timeout() -> Duration {
    std::env::var("RIDGE_SUBAGENT_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .filter(|&seconds| seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_SUBAGENT_TIMEOUT_SECS))
}

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
/// Keep provider failures typed at the dispatch boundary so one selected
/// candidate can fail over exactly once to the caller's main provider.
async fn run_subagent_attempt(
    def: &Agent,
    provider: Arc<dyn LlmProvider>,
    task: &str,
) -> Result<String, String> {
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
        let completion = provider
            .complete(&req)
            .await
            .map_err(|error| provider_failure_label(error.as_ref()))?;
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
            None => return Ok(completion.text),
        }
    }
    Ok(format!("[{} 达到步数上限,未收敛]", def.name))
}

/// Provider errors may contain response bodies or transport details. Keep the
/// user-visible route audit bounded and never echo a secret-bearing payload.
pub(crate) fn provider_failure_label(error: &(dyn std::error::Error + Send + Sync)) -> String {
    let text = error.to_string();
    let parts: Vec<&str> = text.split_whitespace().collect();
    for window in parts.windows(2) {
        if window[0] == "http" {
            let status = window[1].trim_matches(|character: char| !character.is_ascii_digit());
            if status.len() == 3 {
                return format!("http {status}");
            }
        }
    }
    let lower = text.to_ascii_lowercase();
    if lower.contains("timeout") || lower.contains("timed out") {
        "provider request timed out".to_string()
    } else {
        "provider request failed".to_string()
    }
}

async fn run_subagent_via_protocol(
    def: &Agent,
    provider: Arc<dyn LlmProvider>,
    task: &str,
    correlation_id: &str,
) -> Result<String, String> {
    let request = AgentEnvelope::task(
        format!("{correlation_id}:task"),
        "main",
        def.name.clone(),
        correlation_id,
        AgentTask::new(
            task,
            true,
            vec!["read_file".to_string(), "search".to_string()],
            SUBAGENT_MAX_STEPS,
        ),
    );
    let response = in_process_exchange(
        AgentHello::guarded("main", AgentRole::Maker),
        AgentHello::read_only(def.name.clone(), AgentRole::Explorer),
        request,
        |incoming| async move {
            let correlation_id = incoming.correlation_id.clone();
            let parent_id = incoming.message_id.clone();
            let from = incoming.to.clone();
            let to = incoming.from.clone();
            let AgentMessage::Task(payload) = incoming.message else {
                return Err(AgentProtocolError::Invalid(
                    "sub-agent expected Task".to_string(),
                ));
            };
            match run_subagent_attempt(def, provider, &payload.task).await {
                Ok(summary) => Ok(AgentEnvelope::response(
                    format!("{correlation_id}:response"),
                    from,
                    to,
                    correlation_id,
                    AgentResponse {
                        status: AgentStatus::Done,
                        approved: true,
                        steps: 0,
                        tokens: 0,
                        summary,
                        modified_files: Vec::new(),
                    },
                )
                .with_parent(parent_id.clone())),
                Err(message) => Ok(AgentEnvelope::error(
                    format!("{correlation_id}:error"),
                    from,
                    to,
                    correlation_id,
                    AgentError {
                        code: "subagent_failed".to_string(),
                        message,
                        retryable: false,
                    },
                )
                .with_parent(parent_id)),
            }
        },
    )
    .await
    .map_err(|error| error.to_string())?;
    match response.message {
        AgentMessage::Response(result) if result.status == AgentStatus::Done => Ok(result.summary),
        AgentMessage::Error(error) => Err(error.message),
        AgentMessage::Response(result) => Err(format!("sub-agent status {:?}", result.status)),
        _ => Err("sub-agent returned unexpected message".to_string()),
    }
}

async fn run_subagent_bounded(
    def: &Agent,
    provider: Arc<dyn LlmProvider>,
    task: &str,
    correlation_id: &str,
    timeout: Duration,
) -> Result<String, String> {
    let started = std::time::Instant::now();
    match tokio::time::timeout(
        timeout,
        run_subagent_via_protocol(def, provider, task, correlation_id),
    )
    .await
    {
        // Tokio's timeout may observe an already-ready inner future first when
        // both deadlines wake in one scheduler turn. Re-check elapsed time so
        // a late provider result cannot escape the bounded dispatch contract.
        Ok(_) if started.elapsed() >= timeout => Err(format!(
            "sub-agent timed out after {}ms",
            timeout.as_millis()
        )),
        Ok(result) => result,
        Err(_) => Err(format!(
            "sub-agent timed out after {}ms",
            timeout.as_millis()
        )),
    }
}

/// Run a read-only sub-agent without exposing raw provider error payloads.
pub async fn run_subagent(def: &Agent, provider: Arc<dyn LlmProvider>, task: &str) -> String {
    match run_subagent_attempt(def, provider, task).await {
        Ok(out) => out,
        Err(reason) => format!("[{} 出错: {reason}]", def.name),
    }
}

/// `dispatch_agent` 工具 spec(仅在有 agent 定义时暴露)。让主 agent 自主把只读子任务派出去。
pub(crate) fn dispatch_spec(agents: &Agents) -> Option<ToolSpec> {
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
                "task":{"type":"string","description":"交给该 sub-agent 的具体只读子任务"},
                "difficulty":{"type":"string","enum":["simple","moderate","complex"],"description":"可选：任务难度覆盖；省略则由任务文本确定性推断"},
                "size":{"type":"string","enum":["small","medium","large"],"description":"可选：任务规模覆盖"},
                "kind":{"type":"string","enum":["read_only","research","planning","coding","review","general"],"description":"可选：任务类型覆盖"},
                "provider":{"type":"string","description":"可选：provider profile 名；不可用时确定性回退"},
                "model":{"type":"string","description":"可选：模型名覆盖；不可用时确定性回退"}
            },
            "required":["agent","task"]
        }),
    })
}

pub(crate) fn dispatch_batch_spec(agents: &Agents) -> Option<ToolSpec> {
    if agents.defs.is_empty() {
        return None;
    }
    let names: Vec<String> = agents.defs.iter().map(|agent| agent.name.clone()).collect();
    Some(ToolSpec {
        name: "dispatch_agents".to_string(),
        description: format!(
            "并发派出 2-3 个相互独立的只读子任务,一次最多 {MAX_PARALLEL_SUBAGENTS} 个;每项只可 read_file/search,返回按输入顺序汇总。可用 agent: {}",
            names.join(", ")
        ),
        schema: serde_json::json!({
            "type":"object",
            "properties":{
                "tasks":{
                    "type":"array",
                    "minItems":2,
                    "maxItems":MAX_PARALLEL_SUBAGENTS,
                    "items":{
                        "type":"object",
                        "properties":{
                            "agent":{"type":"string","enum":names},
                            "task":{"type":"string","description":"具体只读子任务"}
                        },
                        "required":["agent","task"]
                    }
                }
            },
            "required":["tasks"]
        }),
    })
}

/// 执行 `dispatch_agent`:按任务特性选择可用 provider/model → 跑只读 sub-agent → 回结论与路由原因。
pub(crate) async fn dispatch_obs(
    agents: &Agents,
    main: &Arc<dyn LlmProvider>,
    call: &ToolCall,
) -> String {
    dispatch_one_obs(agents, main, &call.arguments, &call.id).await
}

async fn dispatch_one_obs(
    agents: &Agents,
    main: &Arc<dyn LlmProvider>,
    arguments: &serde_json::Value,
    correlation_id: &str,
) -> String {
    dispatch_one_obs_with_timeout(agents, main, arguments, correlation_id, subagent_timeout()).await
}

async fn dispatch_one_obs_with_timeout(
    agents: &Agents,
    main: &Arc<dyn LlmProvider>,
    arguments: &serde_json::Value,
    correlation_id: &str,
    timeout: Duration,
) -> String {
    let name = arguments
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let task = arguments.get("task").and_then(|v| v.as_str()).unwrap_or("");
    let Some(def) = agents.defs.iter().find(|a| a.name == name) else {
        return format!("没有名为 {name} 的 sub-agent(dispatch_agent 的 enum 里选)");
    };
    let request = RouteRequest::from_task(task, RouteRole::Subagent).with_overrides(
        arguments.get("difficulty").and_then(|v| v.as_str()),
        arguments.get("size").and_then(|v| v.as_str()),
        arguments.get("kind").and_then(|v| v.as_str()),
        arguments
            .get("provider")
            .and_then(|v| v.as_str())
            .or(def.provider.as_deref()),
        arguments.get("model").and_then(|v| v.as_str()),
    );
    let routed = agents.select_provider(&request, main.clone());
    let mut decision = routed.decision;
    let out = match run_subagent_bounded(def, routed.provider, task, correlation_id, timeout).await
    {
        Ok(out) => out,
        Err(first_failure) if decision.selected.is_some() => {
            decision.used_fallback = true;
            decision.reason = format!(
                "{}; selected provider failed ({first_failure}), deterministic main-provider fallback",
                decision.reason
            );
            match run_subagent_bounded(
                def,
                main.clone(),
                task,
                &format!("{correlation_id}:fallback"),
                timeout,
            )
            .await
            {
                Ok(out) => out,
                Err(fallback_failure) => format!(
                    "[{} 出错: selected provider failed ({first_failure}); main-provider fallback failed ({fallback_failure})]",
                    def.name
                ),
            }
        }
        Err(failure) => format!("[{} 出错: {failure}]", def.name),
    };
    format!(
        "[sub-agent {name} route: {}]\n[sub-agent {name} 的结论]\n{out}",
        decision
    )
}

pub(crate) async fn dispatch_batch_obs(
    agents: &Agents,
    main: &Arc<dyn LlmProvider>,
    call: &ToolCall,
) -> String {
    let Some(tasks) = call
        .arguments
        .get("tasks")
        .and_then(|value| value.as_array())
    else {
        return "dispatch_agents requires a tasks array".to_string();
    };
    if tasks.len() < 2 || tasks.len() > MAX_PARALLEL_SUBAGENTS {
        return format!(
            "dispatch_agents requires 2-{} tasks, got {}",
            MAX_PARALLEL_SUBAGENTS,
            tasks.len()
        );
    }

    let results = match tasks.as_slice() {
        [first, second] => {
            let first_id = format!("{}:0", call.id);
            let second_id = format!("{}:1", call.id);
            let (first, second) = tokio::join!(
                dispatch_one_obs(agents, main, first, &first_id),
                dispatch_one_obs(agents, main, second, &second_id),
            );
            vec![first, second]
        }
        [first, second, third] => {
            let first_id = format!("{}:0", call.id);
            let second_id = format!("{}:1", call.id);
            let third_id = format!("{}:2", call.id);
            let (first, second, third) = tokio::join!(
                dispatch_one_obs(agents, main, first, &first_id),
                dispatch_one_obs(agents, main, second, &second_id),
                dispatch_one_obs(agents, main, third, &third_id),
            );
            vec![first, second, third]
        }
        _ => unreachable!("task count is bounded above"),
    };
    format!(
        "parallel sub-agent wave ({}/{} completed)\n{}",
        results.len(),
        results.len(),
        results.join("\n\n")
    )
}

#[cfg(test)]
mod tests {
    use super::{
        builtin_agents, dispatch_batch_obs, dispatch_obs, dispatch_one_obs_with_timeout,
        expand_command, load_commands, load_project_rules, load_skills, parse_agent,
        parse_command_md, readonly_tool_specs, resolve_command, Agent, AgentProvider, Agents,
        Skill,
    };
    use crate::brain::{build_system_prompt, BASE_SYSTEM};
    use crate::route::{RouteRequest, RouteRole};
    use provider::{CompletionRequest, LlmProvider, ToolCall};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    struct HangingProvider;

    #[async_trait::async_trait]
    impl LlmProvider for HangingProvider {
        async fn complete(
            &self,
            _req: &CompletionRequest,
        ) -> Result<provider::Completion, provider::ProviderError> {
            std::future::pending::<Result<provider::Completion, provider::ProviderError>>().await
        }
    }

    struct BarrierProvider {
        barrier: Arc<tokio::sync::Barrier>,
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl LlmProvider for BarrierProvider {
        async fn complete(
            &self,
            _req: &CompletionRequest,
        ) -> Result<provider::Completion, provider::ProviderError> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            self.barrier.wait().await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(provider::Completion {
                text: if call == 0 {
                    "first result".into()
                } else {
                    "second result".into()
                },
                ..Default::default()
            })
        }
    }

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

    #[test]
    fn route_registry_reports_preference_fallback_without_exposing_secrets() {
        let provider: Arc<dyn LlmProvider> = Arc::new(provider::ScriptedProvider::new(Vec::new()));
        let profile = crate::ModelProfile {
            provider: "fast".into(),
            model: "small".into(),
            kind: "openai".into(),
            context_window: Some(64_000),
            cost_tier: Some(1),
            latency_tier: Some(1),
            supports_tools: Some(true),
            supports_reasoning: Some(false),
            tags: vec!["readonly".into()],
        };
        let agents = Agents {
            defs: Vec::new(),
            providers: HashMap::new(),
            route_candidates: vec![AgentProvider {
                profile,
                provider: provider.clone(),
            }],
        };
        let request = RouteRequest::from_task("read the file", RouteRole::Subagent).with_overrides(
            None,
            None,
            None,
            Some("missing"),
            None,
        );
        let routed = agents.select_provider(&request, provider.clone());
        assert_eq!(
            routed.decision.selected_key().as_deref(),
            Some("fast::small")
        );
        assert!(routed.decision.used_fallback);
        assert!(!routed.decision.reason.contains("api_key"));
    }

    fn test_agent(name: &str) -> Agent {
        Agent {
            name: name.into(),
            description: "read files".into(),
            provider: None,
            tools: Some(vec!["search".into()]),
            body: "inspect only".into(),
        }
    }

    #[tokio::test]
    async fn dispatch_agent_timeout_returns_bounded_failure() {
        let provider: Arc<dyn LlmProvider> = Arc::new(HangingProvider);
        let agents = Agents {
            defs: vec![test_agent("explorer")],
            providers: HashMap::new(),
            route_candidates: Vec::new(),
        };
        let args = serde_json::json!({"agent":"explorer","task":"read README"});
        let started = Instant::now();
        let out = dispatch_one_obs_with_timeout(
            &agents,
            &provider,
            &args,
            "timeout-test",
            Duration::from_millis(5),
        )
        .await;
        assert!(started.elapsed() < Duration::from_secs(1), "{out}");
        assert!(out.contains("timed out after 5ms"), "{out}");
    }

    #[tokio::test]
    async fn dispatch_batch_runs_two_subagents_concurrently_and_preserves_slots() {
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let provider: Arc<dyn LlmProvider> = Arc::new(BarrierProvider {
            barrier: Arc::new(tokio::sync::Barrier::new(2)),
            active,
            max_active: max_active.clone(),
            calls,
        });
        let agents = Agents {
            defs: vec![test_agent("explorer"), test_agent("reviewer")],
            providers: HashMap::new(),
            route_candidates: Vec::new(),
        };
        let call = ToolCall {
            id: "batch-test".into(),
            name: "dispatch_agents".into(),
            arguments: serde_json::json!({
                "tasks":[
                    {"agent":"explorer","task":"inspect input"},
                    {"agent":"reviewer","task":"inspect output"}
                ]
            }),
        };
        let started = Instant::now();
        let out = dispatch_batch_obs(&agents, &provider, &call).await;
        assert!(started.elapsed() < Duration::from_secs(1), "{out}");
        assert_eq!(max_active.load(Ordering::SeqCst), 2);
        assert!(out.contains("parallel sub-agent wave (2/2 completed)"));
        assert!(out.contains("first result"), "{out}");
        assert!(out.contains("second result"), "{out}");
    }

    #[tokio::test]
    async fn dispatch_agent_returns_route_audit_and_conclusion() {
        let provider: Arc<dyn LlmProvider> = Arc::new(provider::ScriptedProvider::new(vec![
            provider::Completion {
                text: "read-only result".into(),
                ..Default::default()
            },
        ]));
        let agents = Agents {
            defs: vec![Agent {
                name: "explorer".into(),
                description: "read files".into(),
                provider: None,
                tools: Some(vec!["search".into()]),
                body: "inspect only".into(),
            }],
            providers: HashMap::new(),
            route_candidates: vec![AgentProvider {
                profile: crate::ModelProfile {
                    provider: "fast".into(),
                    model: "small".into(),
                    kind: "openai".into(),
                    context_window: Some(64_000),
                    cost_tier: Some(1),
                    latency_tier: Some(1),
                    supports_tools: Some(true),
                    supports_reasoning: Some(false),
                    tags: vec![],
                },
                provider: provider.clone(),
            }],
        };
        let call = ToolCall {
            id: "route-1".into(),
            name: "dispatch_agent".into(),
            arguments: serde_json::json!({"agent":"explorer","task":"read the README"}),
        };
        let out = dispatch_obs(&agents, &provider, &call).await;
        assert!(out.contains("fast::small"));
        assert!(out.contains("read-only result"));
    }

    struct FailingProvider;

    #[async_trait::async_trait]
    impl LlmProvider for FailingProvider {
        async fn complete(
            &self,
            _req: &CompletionRequest,
        ) -> Result<provider::Completion, provider::ProviderError> {
            Err("http 429: secret-api-body".into())
        }
    }

    #[tokio::test]
    async fn dispatch_agent_falls_back_once_after_selected_provider_failure() {
        let failing: Arc<dyn LlmProvider> = Arc::new(FailingProvider);
        let main: Arc<dyn LlmProvider> = Arc::new(provider::ScriptedProvider::new(vec![
            provider::Completion {
                text: "main fallback result".into(),
                ..Default::default()
            },
        ]));
        let agents = Agents {
            defs: vec![Agent {
                name: "explorer".into(),
                description: "read files".into(),
                provider: None,
                tools: Some(vec!["search".into()]),
                body: "inspect only".into(),
            }],
            providers: HashMap::new(),
            route_candidates: vec![AgentProvider {
                profile: crate::ModelProfile {
                    provider: "failing".into(),
                    model: "limited".into(),
                    kind: "openai".into(),
                    context_window: Some(64_000),
                    cost_tier: Some(1),
                    latency_tier: Some(1),
                    supports_tools: Some(true),
                    supports_reasoning: Some(false),
                    tags: vec![],
                },
                provider: failing,
            }],
        };
        let call = ToolCall {
            id: "route-fallback".into(),
            name: "dispatch_agent".into(),
            arguments: serde_json::json!({"agent":"explorer","task":"read the README"}),
        };

        let out = dispatch_obs(&agents, &main, &call).await;
        assert!(out.contains("selected provider failed (http 429)"), "{out}");
        assert!(out.contains("deterministic main-provider fallback"));
        assert!(out.contains("main fallback result"));
        assert!(!out.contains("secret-api-body"));
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

        // 空目录 → 无技能:基础 prompt(冻结首部)+ host_env 事实块,无技能段。
        let base = build_system_prompt(&[]);
        assert!(base.starts_with(BASE_SYSTEM));
        assert!(!base.contains("# Skills"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 全局规则(~/.ridge/AGENTS.md 类)注入:有 → 标「全局规则」进 body;无且 cwd 无规则文件 → None。
    #[test]
    fn load_project_rules_reads_global_file() {
        let mut f = std::env::temp_dir();
        f.push(format!("ridge_global_rules_{}.md", std::process::id()));
        std::fs::write(&f, "# 全局约定\n答复求简。\n").unwrap();
        let rules = load_project_rules(Some(&f)).expect("全局文件在,应有规则");
        assert!(rules.body.contains("全局规则") && rules.body.contains("答复求简"));
        let _ = std::fs::remove_file(&f);
        // 测试 cwd(crates/agent)无 CLAUDE.md/AGENTS.md,全局也无 → None。
        assert!(load_project_rules(Some(&f)).is_none());
        assert!(load_project_rules(None).is_none());
    }

    /// 内置 /init:恒在命令表(垫底)、不入 skills(不常驻 system prompt);用户同名文件命令可覆盖。
    #[test]
    fn builtin_init_command_present_and_overridable() {
        let cmds = load_commands("/nonexistent", &[]);
        let init = resolve_command("init", &cmds).expect("内置 /init 应恒在");
        assert!(!init.description.is_empty() && init.body.contains("AGENTS.md"));
        // 用户 ~/.ridge/commands/init.md 优先于内置。
        let mut dir = std::env::temp_dir();
        dir.push(format!("ridge_cmds_{}_init_override", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("init.md"), "my custom init").unwrap();
        let cmds = load_commands(&dir, &[]);
        assert_eq!(
            resolve_command("init", &cmds).unwrap().body,
            "my custom init"
        );
        assert_eq!(cmds.iter().filter(|c| c.name == "init").count(), 1);
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
        dir.push(format!("ridge_cmds_{}_merge_skills", std::process::id()));
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
}
