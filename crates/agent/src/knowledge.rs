use crate::exec::{builtin_tool_specs, execute_tool_call};
use provider::{CompletionRequest, LlmProvider, Message, Role, ToolCall, ToolSpec};
use std::collections::HashMap;
use std::sync::Arc;

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
                "task":{"type":"string","description":"交给该 sub-agent 的具体只读子任务"}
            },
            "required":["agent","task"]
        }),
    })
}

/// 执行 `dispatch_agent`:选 provider(定义指定的档案,缺则主 provider)→ 跑 sub-agent → 回结论。
pub(crate) async fn dispatch_obs(
    agents: &Agents,
    main: &Arc<dyn LlmProvider>,
    call: &ToolCall,
) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brain::{build_system_prompt, BASE_SYSTEM};

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
}
