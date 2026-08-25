use crate::communication::{
    in_process_exchange, AgentEnvelope, AgentError, AgentHello, AgentMessage, AgentProtocolError,
    AgentResponse, AgentRole, AgentStatus, AgentTask,
};
use crate::dispatch_budget::{
    default_dispatch_budget, DispatchBudget, DispatchBudgetError, DispatchBudgetRejection,
};
use crate::exec::{builtin_tool_specs, execute_tool_call};
use crate::route::{choose_route, ModelProfile, RouteDecision, RouteRequest, RouteRole};
use provider::{CompletionRequest, LlmProvider, Message, Role, ToolCall, ToolSpec};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillScope {
    pub label: String,
    pub dir: PathBuf,
}

impl SkillScope {
    pub fn new(label: impl Into<String>, dir: impl Into<PathBuf>) -> Self {
        Self {
            label: label.into(),
            dir: dir.into(),
        }
    }
}

/// Provenance only; diagnostics never include skill body text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillSource {
    pub label: String,
    pub path: Option<PathBuf>,
}

impl SkillSource {
    pub fn summary(&self) -> String {
        match &self.path {
            Some(path) => format!("{}:{}", self.label, path.display()),
            None => self.label.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SkillCandidate {
    pub skill: Skill,
    pub source: SkillSource,
    pub selected: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillCollision {
    pub name: String,
    pub winner: SkillSource,
    pub shadowed: Vec<SkillSource>,
}

/// Bounded result of multi-scope discovery. Only `skills` enter prompts;
/// shadowed candidates exist solely for qualified slash-command aliases.
#[derive(Clone, Debug, PartialEq)]
pub struct SkillCatalog {
    pub skills: Vec<Skill>,
    pub candidates: Vec<SkillCandidate>,
    pub collisions: Vec<SkillCollision>,
}

impl SkillCatalog {
    pub fn selected_source(&self, name: &str) -> Option<&SkillSource> {
        self.candidates
            .iter()
            .find(|candidate| candidate.selected && candidate.skill.name == name)
            .map(|candidate| &candidate.source)
    }
}

const MAX_SKILLS: usize = 256;
const MAX_SKILL_BYTES: u64 = 128 * 1024;
const MAX_COMMAND_FILES: usize = 256;
const MAX_COMMAND_BYTES: u64 = 64 * 1024;
const MAX_AGENT_FILES: usize = 256;
const MAX_AGENT_BYTES: u64 = 64 * 1024;
const MAX_PROJECT_RULE_BYTES: u64 = 128 * 1024;
const FILE_TRUNCATION_MARKER: &str = "\n… [file truncated: size limit] …\n";

fn retain_sorted_path(paths: &mut Vec<std::path::PathBuf>, path: std::path::PathBuf, limit: usize) {
    let position = paths
        .partition_point(|candidate| skill_path_sort_key(candidate) < skill_path_sort_key(&path));
    if position < limit {
        paths.insert(position, path);
        if paths.len() > limit {
            paths.pop();
        }
    }
}

fn read_utf8_bounded(path: &std::path::Path, max_bytes: u64) -> Option<String> {
    use std::io::Read;

    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > max_bytes {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn utf8_prefix(bytes: &[u8]) -> &str {
    match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => std::str::from_utf8(&bytes[..error.valid_up_to()]).unwrap_or_default(),
    }
}

fn utf8_suffix(bytes: &[u8]) -> &str {
    (0..bytes.len().min(4))
        .find_map(|offset| std::str::from_utf8(&bytes[offset..]).ok())
        .unwrap_or_default()
}

fn read_project_rule(path: &std::path::Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    if length <= MAX_PROJECT_RULE_BYTES {
        return read_utf8_bounded(path, MAX_PROJECT_RULE_BYTES);
    }

    let retained =
        (MAX_PROJECT_RULE_BYTES as usize).saturating_sub(FILE_TRUNCATION_MARKER.len()) / 2;
    let mut head = vec![0; retained];
    file.read_exact(&mut head).ok()?;
    file.seek(SeekFrom::End(-(retained as i64))).ok()?;
    let mut tail = vec![0; retained];
    file.read_exact(&mut tail).ok()?;
    Some(format!(
        "{}{}{}",
        utf8_prefix(&head),
        FILE_TRUNCATION_MARKER,
        utf8_suffix(&tail)
    ))
}

/// 扫描一个技能目录(`<dir>/<skill>/SKILL.md`),解析成 [`Skill`] 列表。目录不存在 → 空。
fn skill_paths(dir: &Path, limit: usize) -> Vec<PathBuf> {
    // Keep startup discovery deterministic and bounded. The cap is applied by
    // the caller across all scopes, not once per directory.
    let mut paths = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return paths;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let path = entry.path().join("SKILL.md");
        if path.is_file() {
            retain_sorted_path(&mut paths, path, limit);
        }
    }
    paths
}

fn load_scope_candidates(scope: &SkillScope, remaining: usize) -> Vec<SkillCandidate> {
    skill_paths(&scope.dir, remaining)
        .into_iter()
        .filter_map(|path| {
            let text = read_utf8_bounded(&path, MAX_SKILL_BYTES)?;
            let skill = parse_skill(&text)?;
            Some(SkillCandidate {
                skill,
                source: SkillSource {
                    label: scope.label.clone(),
                    path: Some(path),
                },
                selected: false,
            })
        })
        .collect()
}

/// Compatibility API for a single directory. Multi-scope callers should use
/// [`load_skill_catalog`] so precedence and collisions remain observable.
pub fn load_skills(dir: impl AsRef<Path>) -> Vec<Skill> {
    load_skill_catalog(
        &[SkillScope::new("dir", dir.as_ref().to_path_buf())],
        std::iter::empty(),
    )
    .skills
}

/// Merge scopes in explicit high-to-low order. The 256 candidate cap is
/// global, preventing one cap-sized directory per scope from amplifying
/// startup memory and prompt work.
pub fn load_skill_catalog(
    scopes: &[SkillScope],
    fallback: impl IntoIterator<Item = Skill>,
) -> SkillCatalog {
    let mut candidates = Vec::new();
    for scope in scopes {
        let remaining = MAX_SKILLS.saturating_sub(candidates.len());
        if remaining == 0 {
            break;
        }
        candidates.extend(load_scope_candidates(scope, remaining));
    }
    let builtin_source = SkillSource {
        label: "builtin".to_string(),
        path: None,
    };
    for skill in fallback {
        if candidates.len() >= MAX_SKILLS {
            break;
        }
        candidates.push(SkillCandidate {
            skill,
            source: builtin_source.clone(),
            selected: false,
        });
    }

    let mut winners = BTreeMap::<String, usize>::new();
    let mut collisions: Vec<SkillCollision> = Vec::new();
    for index in 0..candidates.len() {
        let name = candidates[index].skill.name.clone();
        if let Some(&winner) = winners.get(&name) {
            let source = candidates[index].source.clone();
            if let Some(collision) = collisions.iter_mut().find(|entry| entry.name == name) {
                collision.shadowed.push(source);
            } else {
                collisions.push(SkillCollision {
                    name,
                    winner: candidates[winner].source.clone(),
                    shadowed: vec![source],
                });
            }
        } else {
            candidates[index].selected = true;
            winners.insert(name, index);
        }
    }
    let skills = candidates
        .iter()
        .filter(|candidate| candidate.selected)
        .map(|candidate| candidate.skill.clone())
        .collect();
    SkillCatalog {
        skills,
        candidates,
        collisions,
    }
}

fn normalized_path_key(path: &Path) -> String {
    let mut existing = path.to_path_buf();
    let mut suffix = Vec::new();
    while !existing.exists() {
        let Some(name) = existing.file_name().map(|name| name.to_os_string()) else {
            break;
        };
        suffix.push(name);
        if !existing.pop() {
            break;
        }
    }
    let mut normalized = std::fs::canonicalize(&existing).unwrap_or(existing);
    for name in suffix.iter().rev() {
        normalized.push(name);
    }
    let text = normalized
        .to_string_lossy()
        .replace('\\', "/")
        .trim_start_matches("//?/")
        .to_string();
    if cfg!(windows) {
        text.to_ascii_lowercase()
    } else {
        text
    }
}

fn is_workspace_root(path: &Path) -> bool {
    read_utf8_bounded(&path.join("Cargo.toml"), 128 * 1024)
        .is_some_and(|text| text.lines().any(|line| line.trim() == "[workspace]"))
}

/// Find a repository marker in at most 64 ancestors.
pub fn find_repo_root(start: impl AsRef<Path>) -> Option<PathBuf> {
    let start = start.as_ref();
    let mut current = std::fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());
    if !current.is_dir() {
        current.pop();
    }
    for _ in 0..64 {
        if current.join(".git").exists()
            || current.join(".codegraph").is_dir()
            || is_workspace_root(&current)
        {
            return Some(current);
        }
        let parent = current.parent()?.to_path_buf();
        if parent == current {
            return None;
        }
        current = parent;
    }
    None
}

/// Build scopes high-to-low: `env > config > cwd > repo > user`.
pub fn discover_skill_scopes(
    cwd: impl AsRef<Path>,
    user_skills: impl AsRef<Path>,
    config_dir: Option<PathBuf>,
    env_dir: Option<PathBuf>,
) -> Vec<SkillScope> {
    let cwd = cwd.as_ref();
    let repo = find_repo_root(cwd);
    let mut scopes = Vec::new();
    let mut seen = BTreeSet::new();
    let mut add = |label: &str, dir: PathBuf| {
        if seen.insert(normalized_path_key(&dir)) {
            scopes.push(SkillScope::new(label, dir));
        }
    };
    if let Some(dir) = env_dir {
        add("env", dir);
    }
    if let Some(dir) = config_dir {
        add("config", dir);
    }
    add("cwd", cwd.join(".ridge/skills"));
    add("cwd-agents", cwd.join(".agents/skills"));
    if let Some(repo) = repo {
        add("repo", repo.join(".ridge/skills"));
        add("repo-agents", repo.join(".agents/skills"));
    }
    add("user", user_skills.as_ref().to_path_buf());
    scopes
}

/// Merge skill sources with deterministic precedence: the first occurrence of
/// a name wins. Callers can place user/project definitions before built-ins
/// without injecting two competing bodies into the system prompt.
pub fn merge_skills(
    primary: impl IntoIterator<Item = Skill>,
    fallback: impl IntoIterator<Item = Skill>,
) -> Vec<Skill> {
    let mut names = std::collections::BTreeSet::new();
    primary
        .into_iter()
        .chain(fallback)
        .filter(|skill| names.insert(skill.name.clone()))
        .collect()
}

fn skill_path_sort_key(path: &std::path::Path) -> (String, String) {
    let original = path.to_string_lossy().into_owned();
    (original.to_ascii_lowercase(), original)
}

/// 解析 `SKILL.md`:YAML frontmatter(`name` / `description`)+ 正文。无 name → 无效。
fn parse_skill(text: &str) -> Option<Skill> {
    // `read_to_string` preserves a UTF-8 BOM and Windows line endings.  Strip
    // only the BOM, then parse frontmatter by complete lines so `\r\n` and `\n`
    // have identical semantics.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = text.strip_prefix("---")?;
    let (front, body) = split_skill_frontmatter(rest)?;
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

fn split_skill_frontmatter(rest: &str) -> Option<(&str, String)> {
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        let line_without_newline = line.strip_suffix('\n').unwrap_or(line);
        let line_without_cr = line_without_newline
            .strip_suffix('\r')
            .unwrap_or(line_without_newline);
        if line_without_cr.trim() == "---" {
            let front = &rest[..offset];
            let body = rest[offset + line.len()..].trim().to_string();
            return Some((front, body));
        }
        offset += line.len();
    }
    None
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

fn alias_label(label: &str) -> String {
    let mut out = label
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    if out.is_empty() {
        out.push_str("scope");
    }
    out
}

fn qualified_skill_name(candidate: &SkillCandidate, occupied: &BTreeSet<String>) -> String {
    let base = format!(
        "{}:{}",
        alias_label(&candidate.source.label),
        candidate.skill.name
    );
    if !occupied.contains(&base) {
        return base;
    }
    let mut suffix = 2;
    loop {
        let name = format!("{base}:{suffix}");
        if !occupied.contains(&name) {
            return name;
        }
        suffix += 1;
    }
}

// These commands are handled before the dynamic catalog by the TUI router.
// Keeping the small reserved-name set here makes a colliding skill reachable
// through its qualified alias while preserving that router precedence.
const BUILTIN_UI_COMMANDS: &[&str] = &[
    "help",
    "exit",
    "quit",
    "model",
    "provider",
    "config",
    "effort",
    "find",
    "goal",
    "activity",
    "inspect",
    "live",
    "transcript",
    "audit",
    "reasoning",
    "thinking",
    "answer",
    "answers",
    "sessions",
    "new",
    "queue",
    "steer",
    "doctor",
    "tools",
    "history",
    "login",
    "agent",
    "mcp",
    "skills",
    "commands",
    "compact",
    "reset",
    "jailbreak",
    "cost",
];

/// Build commands from a catalog. The winning skill keeps `/name`; shadowed
/// skills remain explicitly callable as deterministic `/<scope>:name` aliases
/// without entering the system prompt. File commands retain their historical
/// precedence over skills, followed by the built-in command fallback.
pub fn load_commands_from_catalog(
    dir: impl AsRef<Path>,
    catalog: &SkillCatalog,
) -> Vec<SlashCommand> {
    let mut out = load_command_files(dir);
    let mut occupied: BTreeSet<String> = out.iter().map(|command| command.name.clone()).collect();
    let file_command_names = occupied.clone();

    for skill in &catalog.skills {
        if !BUILTIN_UI_COMMANDS.contains(&skill.name.as_str())
            && occupied.insert(skill.name.clone())
        {
            out.push(SlashCommand {
                name: skill.name.clone(),
                description: skill.description.clone(),
                body: skill.body.clone(),
            });
        }
    }
    for (name, text) in BUILTIN_COMMANDS {
        if occupied.insert((*name).to_string()) {
            out.push(parse_command_md(text, name));
        }
    }

    for candidate in &catalog.candidates {
        let selected_name_is_occupied_by_file =
            candidate.selected && file_command_names.contains(&candidate.skill.name);
        let selected_name_is_builtin =
            candidate.selected && BUILTIN_UI_COMMANDS.contains(&candidate.skill.name.as_str());
        if !candidate.selected || selected_name_is_occupied_by_file || selected_name_is_builtin {
            let name = qualified_skill_name(candidate, &occupied);
            occupied.insert(name.clone());
            out.push(SlashCommand {
                name,
                description: format!(
                    "{} ({})",
                    candidate.skill.description, candidate.source.label
                ),
                body: candidate.skill.body.clone(),
            });
        }
    }
    out
}

fn load_command_files(dir: impl AsRef<std::path::Path>) -> Vec<SlashCommand> {
    let mut out = Vec::new();
    let mut paths = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) == Some("md") {
            retain_sorted_path(&mut paths, path, MAX_COMMAND_FILES);
        }
    }
    for path in paths {
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some(text) = read_utf8_bounded(&path, MAX_COMMAND_BYTES) else {
            continue;
        };
        if !stem.is_empty() {
            out.push(parse_command_md(&text, stem));
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
    let mut paths = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("md") {
            retain_sorted_path(&mut paths, path, MAX_AGENT_FILES);
        }
    }
    let mut names = std::collections::BTreeSet::new();
    for path in paths {
        if let Some(text) = read_utf8_bounded(&path, MAX_AGENT_BYTES) {
            if let Some(agent) = parse_agent(&text) {
                if names.insert(agent.name.clone()) {
                    out.push(agent);
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
    if let Some(t) = global.and_then(read_project_rule) {
        push("全局规则", &t);
    }
    for f in ["CLAUDE.md", "AGENTS.md"] {
        if let Some(t) = read_project_rule(std::path::Path::new(f)) {
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
    /// Select a usable provider deterministically. An unavailable explicit
    /// preference falls straight back to the caller's current provider/model;
    /// silently choosing a different routed profile would violate dispatch
    /// identity and make per-task failures hard to audit.
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

#[derive(Debug)]
enum DispatchFailure {
    Budget(DispatchBudgetError),
    Provider(String),
    Timeout { millis: u128 },
}

impl DispatchFailure {
    fn message(&self) -> String {
        match self {
            Self::Budget(error) => format!(
                "dispatch_budget_rejected{{operation=\"{}\",limit={},reason=\"{}\"}}",
                error.operation,
                error.limit,
                match error.reason {
                    DispatchBudgetRejection::Cancelled => "cancelled",
                    DispatchBudgetRejection::Closed => "closed",
                    DispatchBudgetRejection::AttemptsExhausted => "attempts_exhausted",
                }
            ),
            Self::Provider(message) => message.clone(),
            Self::Timeout { millis } => format!("sub-agent timed out after {millis}ms"),
        }
    }

    fn is_budget(&self) -> bool {
        matches!(self, Self::Budget(_))
    }
}

impl std::fmt::Display for DispatchFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

async fn run_subagent_bounded_with_budget(
    def: &Agent,
    provider: Arc<dyn LlmProvider>,
    task: &str,
    correlation_id: &str,
    timeout: Duration,
    budget: Arc<DispatchBudget>,
) -> Result<String, DispatchFailure> {
    let _permit = budget
        .acquire(None, "dispatch_agent.subagent")
        .await
        .map_err(DispatchFailure::Budget)?;
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
        Ok(_) if started.elapsed() >= timeout => Err(DispatchFailure::Timeout {
            millis: timeout.as_millis(),
        }),
        Ok(result) => result.map_err(DispatchFailure::Provider),
        Err(_) => Err(DispatchFailure::Timeout {
            millis: timeout.as_millis(),
        }),
    }
}

/// Run a read-only sub-agent without exposing raw provider error payloads.
pub async fn run_subagent(def: &Agent, provider: Arc<dyn LlmProvider>, task: &str) -> String {
    run_subagent_public_with_timeout(
        def,
        provider,
        task,
        subagent_timeout(),
        Arc::new(default_dispatch_budget().scope()),
    )
    .await
}

async fn run_subagent_public_with_timeout(
    def: &Agent,
    provider: Arc<dyn LlmProvider>,
    task: &str,
    timeout: Duration,
    budget: Arc<DispatchBudget>,
) -> String {
    match run_subagent_bounded_with_budget(def, provider, task, "public:subagent", timeout, budget)
        .await
    {
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
        effect: provider::ToolEffect::Explore,
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
        effect: provider::ToolEffect::Explore,
    })
}

/// 执行 `dispatch_agent`:按任务特性选择可用 provider/model → 跑只读 sub-agent → 回结论与路由原因。
#[cfg(test)]
pub(crate) async fn dispatch_obs(
    agents: &Agents,
    main: &Arc<dyn LlmProvider>,
    call: &ToolCall,
) -> String {
    dispatch_obs_with_budget(
        agents,
        main,
        call,
        Arc::new(default_dispatch_budget().scope()),
    )
    .await
}

pub(crate) async fn dispatch_obs_with_budget(
    agents: &Agents,
    main: &Arc<dyn LlmProvider>,
    call: &ToolCall,
    budget: Arc<DispatchBudget>,
) -> String {
    dispatch_one_obs_with_timeout_and_budget(
        agents,
        main,
        &call.arguments,
        &call.id,
        subagent_timeout(),
        budget,
    )
    .await
}

#[cfg(test)]
async fn dispatch_one_obs_with_timeout(
    agents: &Agents,
    main: &Arc<dyn LlmProvider>,
    arguments: &serde_json::Value,
    correlation_id: &str,
    timeout: Duration,
) -> String {
    dispatch_one_obs_with_timeout_and_budget(
        agents,
        main,
        arguments,
        correlation_id,
        timeout,
        Arc::new(default_dispatch_budget().scope()),
    )
    .await
}

async fn dispatch_one_obs_with_timeout_and_budget(
    agents: &Agents,
    main: &Arc<dyn LlmProvider>,
    arguments: &serde_json::Value,
    correlation_id: &str,
    timeout: Duration,
    budget: Arc<DispatchBudget>,
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
    let (out, completed) = match run_subagent_bounded_with_budget(
        def,
        routed.provider,
        task,
        correlation_id,
        timeout,
        budget.clone(),
    )
    .await
    {
        Ok(out) => (out, true),
        Err(first_failure) if decision.selected.is_some() && !first_failure.is_budget() => {
            decision.used_fallback = true;
            decision.reason = format!(
                "{}; selected provider failed ({}), using main agent provider/model",
                decision.reason,
                first_failure.message()
            );
            match run_subagent_bounded_with_budget(
                def,
                main.clone(),
                task,
                &format!("{correlation_id}:fallback"),
                timeout,
                budget,
            )
            .await
            {
                Ok(out) => (out, true),
                Err(fallback_failure) => (format!(
                    "[{} 出错: selected provider failed ({first_failure}); main-provider fallback failed ({fallback_failure})]",
                    def.name
                ), false),
            }
        }
        Err(failure) => (format!("[{} 出错: {failure}]", def.name), false),
    };
    let status = if completed { "completed" } else { "failed" };
    format!(
        "[dispatch_status={status}]\n[sub-agent {name} route: {}]\n[sub-agent {name} 的结论]\n{out}",
        decision
    )
}

#[cfg(test)]
pub(crate) async fn dispatch_batch_obs(
    agents: &Agents,
    main: &Arc<dyn LlmProvider>,
    call: &ToolCall,
) -> String {
    dispatch_batch_obs_with_budget(
        agents,
        main,
        call,
        Arc::new(default_dispatch_budget().scope()),
    )
    .await
}

pub(crate) async fn dispatch_batch_obs_with_budget(
    agents: &Agents,
    main: &Arc<dyn LlmProvider>,
    call: &ToolCall,
    budget: Arc<DispatchBudget>,
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
                dispatch_one_obs_with_timeout_and_budget(
                    agents,
                    main,
                    first,
                    &first_id,
                    subagent_timeout(),
                    budget.clone(),
                ),
                dispatch_one_obs_with_timeout_and_budget(
                    agents,
                    main,
                    second,
                    &second_id,
                    subagent_timeout(),
                    budget.clone(),
                ),
            );
            vec![first, second]
        }
        [first, second, third] => {
            let first_id = format!("{}:0", call.id);
            let second_id = format!("{}:1", call.id);
            let third_id = format!("{}:2", call.id);
            let (first, second, third) = tokio::join!(
                dispatch_one_obs_with_timeout_and_budget(
                    agents,
                    main,
                    first,
                    &first_id,
                    subagent_timeout(),
                    budget.clone(),
                ),
                dispatch_one_obs_with_timeout_and_budget(
                    agents,
                    main,
                    second,
                    &second_id,
                    subagent_timeout(),
                    budget.clone(),
                ),
                dispatch_one_obs_with_timeout_and_budget(
                    agents,
                    main,
                    third,
                    &third_id,
                    subagent_timeout(),
                    budget,
                ),
            );
            vec![first, second, third]
        }
        _ => unreachable!("task count is bounded above"),
    };
    let completed = results
        .iter()
        .filter(|result| result.starts_with("[dispatch_status=completed]"))
        .count();
    let failed = results.len().saturating_sub(completed);
    format!(
        "parallel sub-agent wave ({completed}/{} completed) failed={failed}\n{}",
        results.len(),
        results.join("\n\n")
    )
}

#[cfg(test)]
mod tests {
    use super::{
        builtin_agents, discover_skill_scopes, dispatch_batch_obs, dispatch_batch_obs_with_budget,
        dispatch_obs, dispatch_obs_with_budget, dispatch_one_obs_with_timeout, expand_command,
        load_agents, load_commands, load_commands_from_catalog, load_project_rules,
        load_skill_catalog, load_skills, merge_skills, parse_agent, parse_command_md,
        readonly_tool_specs, resolve_command, run_subagent_public_with_timeout, Agent,
        AgentProvider, Agents, Skill, SkillScope, FILE_TRUNCATION_MARKER, MAX_AGENT_BYTES,
        MAX_AGENT_FILES, MAX_COMMAND_BYTES, MAX_COMMAND_FILES,
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

    struct ProbeProvider {
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl LlmProvider for ProbeProvider {
        async fn complete(
            &self,
            request: &CompletionRequest,
        ) -> Result<provider::Completion, provider::ProviderError> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(10)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            let is_planner = request
                .messages
                .first()
                .is_some_and(|message| message.content.contains("Break the user's goal"));
            Ok(provider::Completion {
                text: if is_planner {
                    r#"["routed task"]"#.into()
                } else {
                    "done".into()
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
    fn load_agents_is_bounded_and_duplicate_precedence_is_deterministic() {
        let dir = std::env::temp_dir().join(format!(
            "ridge_agent_bounds_{}_{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for index in 0..300 {
            std::fs::write(
                dir.join(format!("agent-{index:03}.md")),
                format!("---\nname: agent-{index:03}\n---\nbody"),
            )
            .unwrap();
        }
        let agents = load_agents(&dir);
        assert_eq!(agents.len(), MAX_AGENT_FILES);
        assert_eq!(agents.first().unwrap().name, "agent-000");
        assert_eq!(agents.last().unwrap().name, "agent-255");
        let _ = std::fs::remove_dir_all(&dir);

        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a-first.md"),
            "---\nname: duplicate\n---\nfirst body",
        )
        .unwrap();
        std::fs::write(
            dir.join("z-last.md"),
            "---\nname: duplicate\n---\nlast body",
        )
        .unwrap();
        std::fs::write(
            dir.join("oversized.md"),
            format!(
                "---\nname: oversized\n---\n{}",
                "x".repeat(MAX_AGENT_BYTES as usize)
            ),
        )
        .unwrap();
        let agents = load_agents(&dir);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "duplicate");
        assert_eq!(agents[0].body, "first body");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn route_registry_reports_preference_fallback_without_exposing_secrets() {
        let provider: Arc<dyn LlmProvider> = Arc::new(provider::ScriptedProvider::new(Vec::new()));
        let main: Arc<dyn LlmProvider> = Arc::new(provider::ScriptedProvider::new(Vec::new()));
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
        let routed = agents.select_provider(&request, main.clone());
        assert_eq!(routed.decision.selected_key(), None);
        assert!(routed.decision.used_fallback);
        assert!(routed
            .decision
            .reason
            .contains("caller main provider fallback"));
        assert!(Arc::ptr_eq(&routed.provider, &main));

        let missing_model = RouteRequest::from_task("read the file", RouteRole::Subagent)
            .with_overrides(None, None, None, Some("fast"), Some("missing"));
        let routed = agents.select_provider(&missing_model, main.clone());
        assert_eq!(routed.decision.selected_key(), None);
        assert!(Arc::ptr_eq(&routed.provider, &main));
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
    async fn public_subagent_entry_uses_budget_and_timeout_path() {
        let provider: Arc<dyn LlmProvider> = Arc::new(HangingProvider);
        let budget = Arc::new(crate::DispatchBudget::new_with_attempt_limit(1, 2).unwrap());
        let out = run_subagent_public_with_timeout(
            &test_agent("explorer"),
            provider,
            "read README",
            Duration::from_millis(5),
            budget.clone(),
        )
        .await;
        assert!(out.contains("timed out after 5ms"), "{out}");
        assert_eq!(budget.stats().active, 0);
        assert_eq!(budget.stats().attempts, 1);
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
    async fn graph_dispatch_batch_and_routed_run_share_one_budget() {
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let provider: Arc<dyn LlmProvider> = Arc::new(ProbeProvider {
            active,
            max_active: max_active.clone(),
        });
        let agents = Agents {
            defs: vec![test_agent("explorer"), test_agent("reviewer")],
            providers: HashMap::new(),
            route_candidates: Vec::new(),
        };
        let call = ToolCall {
            id: "shared-budget-batch".into(),
            name: "dispatch_agents".into(),
            arguments: serde_json::json!({
                "tasks":[
                    {"agent":"explorer","task":"inspect input"},
                    {"agent":"reviewer","task":"inspect output"}
                ]
            }),
        };
        let budget = Arc::new(crate::DispatchBudget::new(1).unwrap());
        let routed_agents = Agents::default();
        let (batch, routed) = tokio::join!(
            dispatch_batch_obs_with_budget(&agents, &provider, &call, budget.clone()),
            crate::run_planned_routed_with_budget(
                &routed_agents,
                provider.clone(),
                "route one check",
                budget.clone(),
            )
        );
        assert!(
            batch.contains("parallel sub-agent wave (2/2 completed) failed=0"),
            "{batch}"
        );
        assert!(routed.is_ok(), "{routed:?}");
        assert_eq!(max_active.load(Ordering::SeqCst), 1);
        assert_eq!(budget.stats().active, 0);
    }

    #[tokio::test]
    async fn graph_dispatch_budget_rejection_is_structured_and_not_completed() {
        let provider: Arc<dyn LlmProvider> = Arc::new(provider::ScriptedProvider::new(vec![]));
        let agents = Agents {
            defs: vec![test_agent("explorer")],
            providers: HashMap::new(),
            route_candidates: Vec::new(),
        };
        let call = ToolCall {
            id: "closed-budget".into(),
            name: "dispatch_agent".into(),
            arguments: serde_json::json!({"agent":"explorer","task":"inspect"}),
        };
        let budget = Arc::new(crate::DispatchBudget::new(1).unwrap());
        budget.close();
        let single = dispatch_obs_with_budget(&agents, &provider, &call, budget.clone()).await;
        assert!(single.contains("dispatch_status=failed"), "{single}");
        assert!(single.contains("dispatch_budget_rejected"), "{single}");
        assert!(single.contains("reason=\"closed\""), "{single}");
        assert!(!single.contains("dispatch_status=completed"), "{single}");

        let batch_call = ToolCall {
            id: "closed-budget-batch".into(),
            name: "dispatch_agents".into(),
            arguments: serde_json::json!({
                "tasks":[
                    {"agent":"explorer","task":"one"},
                    {"agent":"explorer","task":"two"}
                ]
            }),
        };
        let batch = dispatch_batch_obs_with_budget(&agents, &provider, &batch_call, budget).await;
        assert!(
            batch.contains("parallel sub-agent wave (0/2 completed) failed=2"),
            "{batch}"
        );
    }

    #[tokio::test]
    async fn graph_dispatch_timeout_drops_waiting_permit() {
        let provider: Arc<dyn LlmProvider> = Arc::new(provider::ScriptedProvider::new(vec![]));
        let agents = Agents {
            defs: vec![test_agent("explorer")],
            providers: HashMap::new(),
            route_candidates: Vec::new(),
        };
        let call = ToolCall {
            id: "waiting-budget".into(),
            name: "dispatch_agent".into(),
            arguments: serde_json::json!({"agent":"explorer","task":"inspect"}),
        };
        let budget = Arc::new(crate::DispatchBudget::new(1).unwrap());
        let holder = budget.acquire(None, "test.holder").await.unwrap();
        let result = tokio::time::timeout(
            Duration::from_millis(20),
            dispatch_obs_with_budget(&agents, &provider, &call, budget.clone()),
        )
        .await;
        assert!(result.is_err(), "waiting dispatch should hit test timeout");
        assert_eq!(budget.stats().active, 1);
        drop(holder);
        assert_eq!(budget.stats().active, 0);
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

    struct FailingProvider(&'static str);

    #[async_trait::async_trait]
    impl LlmProvider for FailingProvider {
        async fn complete(
            &self,
            _req: &CompletionRequest,
        ) -> Result<provider::Completion, provider::ProviderError> {
            Err(self.0.into())
        }
    }

    #[tokio::test]
    async fn dispatch_agent_falls_back_once_after_selected_provider_failure() {
        let failing: Arc<dyn LlmProvider> = Arc::new(FailingProvider("http 429: secret-api-body"));
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

        let budget = Arc::new(crate::DispatchBudget::new_with_attempt_limit(1, 2).unwrap());
        let out = dispatch_obs_with_budget(&agents, &main, &call, budget.clone()).await;
        assert!(out.contains("selected provider failed (http 429)"), "{out}");
        assert!(out.contains("using main agent provider/model"), "{out}");
        assert!(out.contains("main fallback result"));
        assert!(!out.contains("secret-api-body"));
        assert_eq!(budget.stats().attempts, 2);
        assert_eq!(budget.stats().active, 0);
    }

    #[tokio::test]
    async fn dispatch_agent_falls_back_on_auth_and_unreachable_failures() {
        for failure in [
            "http 401: private-auth-body",
            "connection refused: private-host",
        ] {
            let main: Arc<dyn LlmProvider> = Arc::new(provider::ScriptedProvider::new(vec![
                provider::Completion {
                    text: "main recovered".into(),
                    ..Default::default()
                },
            ]));
            let agents = Agents {
                defs: vec![test_agent("explorer")],
                providers: HashMap::new(),
                route_candidates: vec![AgentProvider {
                    profile: crate::ModelProfile {
                        provider: "broken".into(),
                        model: "unavailable".into(),
                        kind: "openai".into(),
                        context_window: Some(64_000),
                        cost_tier: Some(1),
                        latency_tier: Some(1),
                        supports_tools: Some(true),
                        supports_reasoning: Some(false),
                        tags: vec![],
                    },
                    provider: Arc::new(FailingProvider(failure)),
                }],
            };
            let call = ToolCall {
                id: "route-recovery".into(),
                name: "dispatch_agent".into(),
                arguments: serde_json::json!({
                    "agent":"explorer",
                    "task":"inspect target",
                    "provider":"broken",
                    "model":"unavailable"
                }),
            };
            let out = dispatch_obs(&agents, &main, &call).await;
            assert!(out.contains("using main agent provider/model"), "{out}");
            assert!(out.contains("main recovered"), "{out}");
            assert!(!out.contains("private-auth-body"), "{out}");
            assert!(!out.contains("private-host"), "{out}");
        }
    }

    #[tokio::test]
    async fn dispatch_agents_fall_back_independently_to_main_provider() {
        let failing: Arc<dyn LlmProvider> = Arc::new(FailingProvider("http 429: secret-api-body"));
        let main: Arc<dyn LlmProvider> = Arc::new(provider::ScriptedProvider::new(vec![
            provider::Completion {
                text: "main-a".into(),
                ..Default::default()
            },
            provider::Completion {
                text: "main-b".into(),
                ..Default::default()
            },
        ]));
        let agents = Agents {
            defs: vec![test_agent("explorer"), test_agent("reviewer")],
            providers: HashMap::new(),
            route_candidates: vec![AgentProvider {
                profile: crate::ModelProfile {
                    provider: "broken".into(),
                    model: "gone".into(),
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
            id: "batch-fallback".into(),
            name: "dispatch_agents".into(),
            arguments: serde_json::json!({
                "tasks":[
                    {"agent":"explorer","task":"inspect left","provider":"broken","model":"gone"},
                    {"agent":"reviewer","task":"inspect right","provider":"broken","model":"gone"}
                ]
            }),
        };
        let out = dispatch_batch_obs(&agents, &main, &call).await;
        assert!(
            out.contains("parallel sub-agent wave (2/2 completed)"),
            "{out}"
        );
        assert_eq!(out.matches("using main agent provider/model").count(), 2);
        assert!(out.contains("main-a"), "{out}");
        assert!(out.contains("main-b"), "{out}");
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
    fn load_skills_handles_bom_crlf_and_duplicate_names_deterministically() {
        let dir = std::env::temp_dir().join(format!(
            "ridge_skill_reliability_{}_{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("z-last")).unwrap();
        std::fs::create_dir_all(dir.join("a-first")).unwrap();
        std::fs::create_dir_all(dir.join("middle")).unwrap();
        std::fs::write(
            dir.join("z-last/SKILL.md"),
            "---\nname: duplicate\ndescription: later\n---\nsecond body",
        )
        .unwrap();
        std::fs::write(
            dir.join("a-first/SKILL.md"),
            "\u{feff}---\r\nname: duplicate\r\ndescription: first\r\n---\r\nfirst body\r\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("middle/SKILL.md"),
            "---\r\nname: unique\r\ndescription: stable\r\n---\r\nunique body\r\n",
        )
        .unwrap();

        let skills = load_skills(&dir);
        assert_eq!(
            skills
                .iter()
                .map(|skill| skill.name.as_str())
                .collect::<Vec<_>>(),
            vec!["duplicate", "unique"]
        );
        assert_eq!(skills[0].description, "first");
        assert_eq!(skills[0].body, "first body");
        assert_eq!(skills[1].body, "unique body");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn merge_skills_prefers_user_source_over_builtin() {
        let user = Skill {
            name: "skill-creator".into(),
            description: "local override".into(),
            body: "local body".into(),
        };
        let builtin = Skill {
            name: "skill-creator".into(),
            description: "builtin".into(),
            body: "builtin body".into(),
        };
        let extra = Skill {
            name: "extra".into(),
            description: "extra".into(),
            body: "extra body".into(),
        };
        assert_eq!(
            merge_skills(vec![user.clone(), extra.clone()], vec![builtin]),
            vec![user, extra]
        );
    }

    fn write_test_skill(root: &std::path::Path, dir: &str, name: &str, body: &str) {
        let path = root.join(dir);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(
            path.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: test\n---\n{body}"),
        )
        .unwrap();
    }

    #[test]
    fn skill_catalog_precedence_collision_alias_and_prompt_bounded() {
        let root = std::env::temp_dir().join(format!(
            "ridge_skill_catalog_{}_{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let env = root.join("env");
        let user = root.join("user");
        let command_dir = root.join("commands");
        write_test_skill(&env, "one", "shared", "env body");
        write_test_skill(&user, "one", "shared", "user body");
        std::fs::create_dir_all(&command_dir).unwrap();
        std::fs::write(command_dir.join("shared.md"), "file command body").unwrap();

        let catalog = load_skill_catalog(
            &[SkillScope::new("env", &env), SkillScope::new("user", &user)],
            std::iter::empty(),
        );
        assert_eq!(catalog.skills.len(), 1);
        assert_eq!(catalog.skills[0].body, "env body");
        assert_eq!(catalog.collisions.len(), 1);
        assert_eq!(catalog.collisions[0].winner.label, "env");
        assert_eq!(catalog.collisions[0].shadowed[0].label, "user");

        let commands = load_commands_from_catalog(&command_dir, &catalog);
        assert_eq!(
            resolve_command("shared", &commands).unwrap().body,
            "file command body"
        );
        assert_eq!(
            resolve_command("env:shared", &commands).unwrap().body,
            "env body"
        );
        assert_eq!(
            resolve_command("user:shared", &commands).unwrap().body,
            "user body"
        );
        let prompt = build_system_prompt(&catalog.skills);
        assert!(prompt.contains("env body"));
        assert!(!prompt.contains("user body"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn skill_scopes_are_bounded_deduplicated_and_stable() {
        let root = std::env::temp_dir().join(format!(
            "ridge_skill_scopes_{}_{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let cwd = root.clone();
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(root.join(".codegraph")).unwrap();
        let scopes = discover_skill_scopes(
            &cwd,
            root.join("user"),
            Some(root.join("config")),
            Some(root.join("env")),
        );
        let labels: Vec<&str> = scopes.iter().map(|scope| scope.label.as_str()).collect();
        assert_eq!(labels[..2], ["env", "config"]);
        assert_eq!(labels.last(), Some(&"user"));
        assert!(scopes.iter().any(|scope| scope.label == "cwd"));
        assert_eq!(
            scopes
                .iter()
                .filter(|scope| scope.dir.ends_with(".ridge/skills"))
                .count(),
            1
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn skill_catalog_applies_one_global_candidate_cap() {
        let root = std::env::temp_dir().join(format!(
            "ridge_skill_global_cap_{}_{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let high = root.join("high");
        let low = root.join("low");
        for index in 0..200 {
            write_test_skill(
                &high,
                &format!("high-{index:03}"),
                &format!("high-{index:03}"),
                "h",
            );
            write_test_skill(
                &low,
                &format!("low-{index:03}"),
                &format!("low-{index:03}"),
                "l",
            );
        }
        let catalog = load_skill_catalog(
            &[SkillScope::new("high", &high), SkillScope::new("low", &low)],
            vec![Skill {
                name: "fallback".into(),
                description: String::new(),
                body: "fallback".into(),
            }],
        );
        assert_eq!(catalog.candidates.len(), 256);
        assert_eq!(catalog.skills.len(), 256);
        assert!(catalog.skills.iter().any(|skill| skill.name == "high-000"));
        assert!(!catalog.skills.iter().any(|skill| skill.name == "fallback"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn load_skills_bounds_candidate_count_and_file_size() {
        let dir = std::env::temp_dir().join(format!(
            "ridge_skill_bounds_{}_{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for index in 0..300 {
            let skill_dir = dir.join(format!("skill-{index:03}"));
            std::fs::create_dir_all(&skill_dir).unwrap();
            std::fs::write(
                skill_dir.join("SKILL.md"),
                format!("---\nname: skill-{index:03}\n---\nbody"),
            )
            .unwrap();
        }
        let oversized = dir.join("zz-oversized");
        std::fs::create_dir_all(&oversized).unwrap();
        std::fs::write(
            oversized.join("SKILL.md"),
            format!("---\nname: oversized\n---\n{}", "x".repeat(128 * 1024)),
        )
        .unwrap();

        let skills = load_skills(&dir);
        assert_eq!(skills.len(), 256);
        assert!(!skills.iter().any(|skill| skill.name == "oversized"));
        let _ = std::fs::remove_dir_all(&dir);
    }

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

    #[test]
    fn load_project_rules_retains_head_tail_and_marks_oversize() {
        let file = std::env::temp_dir().join(format!(
            "ridge_global_rules_bounds_{}_{}.md",
            std::process::id(),
            line!()
        ));
        let middle = "x".repeat(super::MAX_PROJECT_RULE_BYTES as usize);
        std::fs::write(&file, format!("HEAD\n{middle}\nTAIL")).unwrap();
        let rules = load_project_rules(Some(&file)).expect("oversized rules stay explicit");
        assert!(rules.body.contains("HEAD"));
        assert!(rules.body.contains("TAIL"));
        assert!(rules.body.contains(FILE_TRUNCATION_MARKER.trim()));
        let _ = std::fs::remove_file(&file);
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

    #[test]
    fn load_commands_bounds_candidate_count_and_file_size() {
        let dir = std::env::temp_dir().join(format!(
            "ridge_command_bounds_{}_{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for index in 0..300 {
            std::fs::write(dir.join(format!("cmd-{index:03}.md")), "command body").unwrap();
        }
        let commands = load_commands(&dir, &[]);
        assert_eq!(
            commands
                .iter()
                .filter(|command| command.name.starts_with("cmd-"))
                .count(),
            MAX_COMMAND_FILES
        );
        assert!(resolve_command("cmd-255", &commands).is_some());
        assert!(resolve_command("cmd-256", &commands).is_none());
        let _ = std::fs::remove_dir_all(&dir);

        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("valid.md"), "valid body").unwrap();
        std::fs::write(
            dir.join("oversized.md"),
            "x".repeat(MAX_COMMAND_BYTES as usize + 1),
        )
        .unwrap();
        let commands = load_commands(&dir, &[]);
        assert!(resolve_command("valid", &commands).is_some());
        assert!(resolve_command("oversized", &commands).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
