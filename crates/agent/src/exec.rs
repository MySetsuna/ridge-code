use crate::guard::{
    active_sandbox_cmd, constraint_guard_shell, constraint_guard_write, jail, run_post_tool_hooks,
    run_pre_tool_hooks, sandbox_argv,
};
use crate::signals::{signal_create, signal_resolve, SIGNALS_DIR};
use crate::state::{
    EvidenceRef, Patch, RequirementStatus, RequirementUpdate, TaskContract, Todo, ToolResultStatus,
    ToolResultV1,
};
use provider::{ToolCall, ToolEffect, ToolSpec};
use sha2::{Digest, Sha256};

/// 内置工具的规格(喂给 LLM 让它按 schema 出结构化 tool_call)。
pub fn builtin_tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "run_shell".to_string(),
            description: "Run host build/test/pack. Not for files (use search/read/edit). >180s parks; poll or cancel job_id.".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"cmd":{"type":"string","description":"Command to start; omit when polling job_id"},"shell":{"type":"string","enum":["cmd","powershell","pwsh","bash","sh"],"description":"可选:执行用的 shell;省=宿主默认(见 host_env)"},"job_id":{"type":"string","description":"Poll a parked job from a previous run_shell"},"cancel_job_id":{"type":"string","description":"Cancel a parked job and return its bounded settlement"}},"required":[]}),
            effect: ToolEffect::Verify,
        },
        ToolSpec {
            name: "write_file".to_string(),
            description: "把内容整文件写入路径(覆盖)。仅用于**新建文件**;改动已有文件请用 edit_file".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"},"contents":{"type":"string"}},"required":["path","contents"]}),
            effect: ToolEffect::Edit,
        },
        ToolSpec {
            name: "edit_file".to_string(),
            description: "精准编辑:唯一 old_string→new_string。CRLF 对齐;失败用观察里的锚点再 edit。".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"},"old_string":{"type":"string"},"new_string":{"type":"string"},"expected_hash":{"type":"string","description":"可选:最近 read_file 内容的 SHA-256;不匹配则拒绝陈旧编辑"}},"required":["path","old_string","new_string"]}),
            effect: ToolEffect::Edit,
        },
        ToolSpec {
            name: "apply_edits".to_string(),
            description: "**跨文件批量**精准编辑:多处 {path, old_string, new_string} 汇总一份 diff 一次确认、**原子应用**(全成或全不改)。重构/多文件改动用它".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"edits":{"type":"array","items":{"type":"object","properties":{"path":{"type":"string"},"old_string":{"type":"string"},"new_string":{"type":"string"},"expected_hash":{"type":"string","description":"可选:该文件最近读取内容的 SHA-256"}},"required":["path","old_string","new_string"]}}},"required":["edits"]}),
            effect: ToolEffect::Edit,
        },
        ToolSpec {
            name: "read_file".to_string(),
            description: "读取文件。可选 offset(起始行,1 起)+ limit(行数)只读一段,大文件别整读".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"},"offset":{"type":"integer"},"limit":{"type":"integer"}},"required":["path"]}),
            effect: ToolEffect::Explore,
        },
        ToolSpec {
            name: "search".to_string(),
            description: "按 glob+pattern 搜 路径:行号:内容；path 可为文件或目录。定位用它,目标已明勿全库搜。".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"},"glob":{"type":"string"}},"required":["pattern"]}),
            effect: ToolEffect::Explore,
        },
        ToolSpec {
            name: "web_search".to_string(),
            description: "联网搜索,返回标题/链接/摘要(自动按网络环境选可用引擎)。查实时信息或外部资料用它;query 会发给外部搜索引擎".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}),
            effect: ToolEffect::Explore,
        },
        ToolSpec {
            name: "fetch_url".to_string(),
            description: "抓取一个网页并返回**可读正文**(去脚本/样式/标签)。配合 web_search:先搜到链接,再用它读正文、据原文作答,别只凭摘要猜".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}),
            effect: ToolEffect::Explore,
        },
        ToolSpec {
            name: "todo_write".to_string(),
            description: "维护任务清单:把计划拆成若干 {content, status}。**多步/复杂任务**开始时列清单、每完成一步更新其状态给用户看进度;简单单步不必用".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"todos":{"type":"array","items":{"type":"object","properties":{"content":{"type":"string"},"status":{"type":"string","enum":["pending","in_progress","completed"]}},"required":["content","status"]}}},"required":["todos"]}),
            effect: ToolEffect::Unknown,
        },
        ToolSpec {
            name: "contract_write".to_string(),
            description: "Record objective + requirements[{id,description}], with optional constraints/deliverables/non_goals, before work.".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"objective":{"type":"string"},"requirements":{"type":"array","items":{"type":"object","properties":{"id":{"type":"string"},"description":{"type":"string"}},"required":["description"]}},"constraints":{"type":"array","items":{"type":"string"}},"deliverables":{"type":"array","items":{"type":"string"}},"non_goals":{"type":"array","items":{"type":"string"}}},"required":["objective","requirements"]}),
            effect: ToolEffect::Unknown,
        },
        ToolSpec {
            name: "requirement_update".to_string(),
            description: "Update requirement statuses with prior evidence_call_ids. Satisfied needs real current success; never invent ids.".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"requirements":{"type":"array","items":{"type":"object","properties":{"id":{"type":"string"},"status":{"type":"string","enum":["unknown","satisfied","failed","blocked","waived"]},"evidence_call_ids":{"type":"array","items":{"type":"string"}}},"required":["id","status"]}}},"required":["requirements"]}),
            effect: ToolEffect::Unknown,
        },
        ToolSpec {
            name: "signal_write".to_string(),
            description: "记录/消解**跨会话复用**的信号(发现/摩擦/待办)。记:给 type+body;消解已处理的:给 resolve=<id>。下个会话自动继承未决信号".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"type":{"type":"string"},"body":{"type":"string"},"resolve":{"type":"string"}}}),
            effect: ToolEffect::Unknown,
        },
    ]
}

/// 从 `apply_edits` 的参数里抽出 `edits` 数组 → [`tools::Edit`] 列表(字段缺失→跳过)。
pub(crate) fn parse_edits(call: &ToolCall) -> Vec<tools::Edit> {
    let value = call.arguments.get("edits").cloned().or_else(|| {
        let path = call.arguments.get("path")?.as_str()?;
        Some(serde_json::json!([{
            "path": path,
            "old_string": call.arguments.get("old_string")?.as_str()?,
            "new_string": call.arguments.get("new_string")?.as_str()?,
        }]))
    });
    let Some(value) = value else {
        return Vec::new();
    };
    let parsed = match value {
        serde_json::Value::String(text) => serde_json::from_str(&text).ok(),
        other => Some(other),
    };
    let Some(arr) = parsed.as_ref().and_then(|v| v.as_array()) else {
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
/// 工具观察是否为**错误**(工具名前缀 ` error:` / `BLOCKED` / `permission denied` / **非零 `exit N`**)。单一真相:
/// Durable State 回填与熔断计数(`err_streak`)共用,免两处判据漂移。**非零 exit 必判错**(iter-51):
/// 此前漏判 —— 本地化(如中文 GBK)shell 报错正文无 ASCII " error:",致 `exit 1` 逃熔断计数、
/// `last_error` 亦不回填。与 verify 侧 [`tool_output_failed`] 对齐,免判据分叉。
pub(crate) fn is_error_observation(obs: &str) -> bool {
    let first = obs.lines().next().unwrap_or(obs).trim_start();
    let named_error = first.split_once(" error:").is_some_and(|(name, _)| {
        !name.is_empty()
            && name.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
            })
    });
    named_error
        || first.starts_with("BLOCKED")
        || first.starts_with("permission denied")
        || (first.starts_with("exit ") && !first.starts_with("exit 0"))
}

#[allow(dead_code)]
pub(crate) fn durable_updates(call: &ToolCall, observation: &str) -> Vec<Patch> {
    durable_updates_with_effect(call, ToolEffect::from_name(&call.name), observation)
}

/// Durable update path for a resolved dynamic tool.  The ordinary wrapper
/// above keeps built-in callers/source compatibility; MCP dispatch supplies
/// its trusted effect metadata here.
pub(crate) fn durable_updates_with_effect(
    call: &ToolCall,
    effect: ToolEffect,
    observation: &str,
) -> Vec<Patch> {
    durable_updates_with_native_result(
        call,
        effect,
        observation,
        tool_result_v1(call, effect, observation),
    )
}

/// Durable update path for a result produced at an execution boundary. The
/// caller supplies the typed fields directly; this function must not decode
/// them from the display observation again.
pub(crate) fn durable_updates_with_native_result(
    call: &ToolCall,
    effect: ToolEffect,
    observation: &str,
    typed_result: ToolResultV1,
) -> Vec<Patch> {
    let mut patches = Vec::new();
    let failed = typed_result.status.is_error();
    if failed {
        let line = observation
            .lines()
            .next()
            .unwrap_or(observation)
            .to_string();
        patches.push(Patch::SetLastError(Some(line)));
    }
    let arg = |k: &str| call.arguments.get(k).and_then(|v| v.as_str());
    let observed_paths = if !failed {
        observation_changed_paths(observation)
    } else {
        Vec::new()
    };
    let mut advances_revision = false;
    match call.name.as_str() {
        "write_file" | "edit_file" if !failed => {
            if let Some(path) = arg("path") {
                advances_revision = true;
                patches.push(Patch::RecordModified(path.to_string()));
                patches.push(Patch::SetLastError(None));
            }
        }
        "apply_edits" if !failed => {
            let edits = parse_edits(call);
            if !edits.is_empty() {
                advances_revision = true;
                patches.extend(edits.into_iter().map(|e| Patch::RecordModified(e.path)));
                patches.push(Patch::SetLastError(None));
            }
        }
        "read_file" if !failed => {
            if let Some(path) = arg("path") {
                patches.push(Patch::RecordRead(path.to_string()));
            }
        }
        "run_shell" => patches.extend(shell_job_updates(call, observation)),
        _ if !failed && effect.is_edit() => {
            let mut paths = argument_paths(call);
            paths.extend(observed_paths.iter().cloned());
            paths.sort();
            paths.dedup();
            patches.extend(paths.into_iter().map(Patch::RecordModified));
            if !patches.is_empty() {
                advances_revision = true;
                patches.push(Patch::SetLastError(None));
            }
        }
        // An unannotated dynamic tool may still prove a mutation through a
        // bounded structured result. Plain prose never upgrades Unknown.
        _ if !failed && effect == ToolEffect::Unknown && !observed_paths.is_empty() => {
            advances_revision = true;
            patches.extend(observed_paths.into_iter().map(Patch::RecordModified));
            patches.push(Patch::SetLastError(None));
        }
        _ if !failed && effect.is_explore() => {
            let paths = argument_paths(call);
            patches.extend(paths.into_iter().map(Patch::RecordRead));
        }
        _ => {}
    }
    if advances_revision {
        patches.insert(0, Patch::AdvanceWorkspaceRevision);
    }
    let summary = observation
        .lines()
        .next()
        .unwrap_or(observation)
        .chars()
        .take(512)
        .collect();
    patches.push(Patch::RecordEvidence(EvidenceRef {
        call_id: call.id.clone(),
        tool: call.name.clone(),
        succeeded: typed_result.status.is_success(),
        workspace_revision: 0,
        summary,
    }));
    patches.push(Patch::RecordToolResult(typed_result));
    patches
}

/// Upgrade an unknown dynamic action only when its result carries explicit,
/// machine-readable changed paths. This is the same evidence used by durable
/// state, so handoff and completion cannot disagree about what happened.
pub(crate) fn effective_tool_effect(effect: ToolEffect, observation: &str) -> ToolEffect {
    if effect == ToolEffect::Unknown
        && !is_error_observation(observation)
        && !observation_changed_paths(observation).is_empty()
    {
        ToolEffect::Edit
    } else {
        effect
    }
}

fn observation_changed_paths(observation: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(observation.trim()) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    collect_observation_paths(&value, &mut paths);
    paths
}

fn collect_observation_paths(value: &serde_json::Value, paths: &mut Vec<String>) {
    const MAX_PATHS: usize = 64;
    if paths.len() >= MAX_PATHS {
        return;
    }
    match value {
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                if is_changed_path_key(key) {
                    collect_path_values(value, paths);
                } else {
                    collect_observation_paths(value, paths);
                }
                if paths.len() >= MAX_PATHS {
                    break;
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_observation_paths(value, paths);
                if paths.len() >= MAX_PATHS {
                    break;
                }
            }
        }
        _ => {}
    }
}

fn collect_path_values(value: &serde_json::Value, paths: &mut Vec<String>) {
    match value {
        serde_json::Value::String(path) => {
            let path = path.trim();
            if !path.is_empty() && !paths.iter().any(|existing| existing == path) {
                paths.push(path.to_string());
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_path_values(value, paths);
            }
        }
        serde_json::Value::Object(_) => collect_observation_paths(value, paths),
        _ => {}
    }
}

fn is_changed_path_key(key: &str) -> bool {
    matches!(
        key,
        "modified_files"
            | "modifiedFiles"
            | "changed_files"
            | "changedFiles"
            | "changed_paths"
            | "changedPaths"
            | "written_files"
            | "writtenFiles"
    )
}

fn argument_paths(call: &ToolCall) -> Vec<String> {
    let mut paths = Vec::new();
    collect_argument_paths(&call.arguments, false, &mut paths);
    paths
}

fn parse_exit_code(observation: &str) -> Option<i32> {
    let rest = observation.trim_start().strip_prefix("exit ")?;
    let code = rest
        .split_once(':')
        .map(|(code, _)| code)
        .unwrap_or(rest)
        .trim();
    code.parse().ok()
}

/// Compatibility projection for every current transport. Built-ins and MCP
/// may still supply text today, but only this boundary is allowed to decode it
/// into state. New native transports can construct [`ToolResultV1`] directly.
pub(crate) fn tool_result_v1(
    call: &ToolCall,
    effect: ToolEffect,
    observation: &str,
) -> ToolResultV1 {
    let first = observation
        .lines()
        .next()
        .unwrap_or(observation)
        .trim_start();
    let status = if first.starts_with("BLOCKED") || first.starts_with("permission denied") {
        ToolResultStatus::Blocked
    } else if parse_running_job_id(observation).is_some() {
        ToolResultStatus::Running
    } else if is_error_observation(observation) {
        ToolResultStatus::Error
    } else {
        ToolResultStatus::Success
    };
    let exit_code = parse_exit_code(observation);
    let mut changed_paths = if status.is_success() && effect.is_edit() {
        argument_paths(call)
    } else {
        Vec::new()
    };
    if status.is_success() && effect == ToolEffect::Unknown {
        changed_paths.extend(observation_changed_paths(observation));
        changed_paths.sort();
        changed_paths.dedup();
    }
    let summary = observation
        .lines()
        .next()
        .unwrap_or(observation)
        .chars()
        .take(512)
        .collect();
    ToolResultV1 {
        schema_version: 1,
        call_id: call.id.clone(),
        tool: call.name.clone(),
        effect,
        status,
        exit_code,
        retryable: status == ToolResultStatus::Error
            && exit_code.is_none_or(|code| matches!(code, 1 | 124 | 429 | 500..=599)),
        changed_paths,
        output_truncated: false,
        read_offset: None,
        read_limit: None,
        match_count: None,
        workspace_revision: 0,
        summary,
    }
}

fn collect_argument_paths(value: &serde_json::Value, path_context: bool, paths: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                let key_path = is_path_key(key);
                if key_path {
                    collect_argument_paths(value, true, paths);
                } else if key == "edits" || key == "changes" || key == "files" {
                    collect_argument_paths(value, false, paths);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_argument_paths(value, path_context, paths);
            }
        }
        serde_json::Value::String(path) if path_context => {
            let path = path.trim();
            if !path.is_empty() && !paths.iter().any(|existing| existing == path) {
                paths.push(path.to_string());
            }
        }
        _ => {}
    }
}

fn is_path_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    matches!(
        key.as_str(),
        "path"
            | "paths"
            | "file"
            | "files"
            | "filename"
            | "filenames"
            | "file_path"
            | "file_paths"
            | "source"
            | "destination"
            | "target"
            | "targets"
    ) || key.ends_with("_path")
        || key.ends_with("_paths")
        || key.ends_with("_file")
        || key.ends_with("_files")
}

fn shell_job_updates(call: &ToolCall, observation: &str) -> Vec<Patch> {
    if let Some(id) = parse_running_job_id(observation) {
        return vec![Patch::AddLiveShellJob(id)];
    }
    settled_job_id(call, observation)
        .map(Patch::RemoveLiveShellJob)
        .into_iter()
        .collect()
}

fn settled_job_id(call: &ToolCall, observation: &str) -> Option<String> {
    let from_arg = call
        .arguments
        .get("job_id")
        .or_else(|| call.arguments.get("cancel_job_id"))
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim();
    if !from_arg.is_empty() {
        return Some(from_arg.to_string());
    }
    observation
        .split("unknown job ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

pub(crate) fn parse_running_job_id(observation: &str) -> Option<String> {
    let rest = observation.strip_prefix("job ")?;
    let id = rest.split_whitespace().next()?;
    rest.contains(" running").then(|| id.to_string())
}

/// 从 `todo_write` 的参数里抽出 `todos` 数组 → [`Todo`] 列表(status 缺省 `pending`)。
pub(crate) fn parse_todos(call: &ToolCall) -> Vec<Todo> {
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

fn bounded_text(value: &str, label: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{label} cannot be empty"));
    }
    if value.chars().count() > 1_000 {
        return Err(format!("{label} is too long"));
    }
    Ok(value.to_string())
}

fn bounded_text_list(call: &ToolCall, key: &str) -> Result<Vec<String>, String> {
    let Some(value) = call.arguments.get(key) else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| format!("{key} must be an array"))?;
    if array.len() > 32 {
        return Err(format!("{key} has too many items"));
    }
    array
        .iter()
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| format!("{key} items must be strings"))
                .and_then(|value| bounded_text(value, key))
        })
        .collect()
}

fn valid_requirement_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().count() <= 64
        && id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
}

/// Parse a bounded task contract. Initial requirements are always `unknown`;
/// only `requirement_update` can make a completion claim after real evidence.
pub(crate) fn parse_task_contract(call: &ToolCall) -> Result<TaskContract, String> {
    let objective = call
        .arguments
        .get("objective")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "objective must be a string".to_string())
        .and_then(|value| bounded_text(value, "objective"))?;
    let requirements = call
        .arguments
        .get("requirements")
        .and_then(|value| value.as_array())
        .ok_or_else(|| "requirements must be an array".to_string())?;
    if requirements.is_empty() || requirements.len() > 32 {
        return Err("requirements must contain 1 to 32 items".to_string());
    }
    let mut parsed = Vec::with_capacity(requirements.len());
    for (index, value) in requirements.iter().enumerate() {
        let object = value
            .as_object()
            .ok_or_else(|| "each requirement must be an object".to_string())?;
        let id = object
            .get("id")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("R{}", index + 1));
        if !valid_requirement_id(&id)
            || parsed
                .iter()
                .any(|requirement: &crate::state::Requirement| requirement.id == id)
        {
            return Err(format!("requirement id `{id}` is invalid or duplicated"));
        }
        let description = object
            .get("description")
            .and_then(|value| value.as_str())
            .ok_or_else(|| format!("requirement `{id}` needs description"))
            .and_then(|value| bounded_text(value, "requirement description"))?;
        parsed.push(crate::state::Requirement {
            id,
            description,
            status: RequirementStatus::Unknown,
            evidence_call_ids: Vec::new(),
        });
    }
    Ok(TaskContract {
        objective,
        requirements: parsed,
        constraints: bounded_text_list(call, "constraints")?,
        deliverables: bounded_text_list(call, "deliverables")?,
        non_goals: bounded_text_list(call, "non_goals")?,
    })
}

fn parse_requirement_status(value: &str) -> Option<RequirementStatus> {
    match value.trim().to_ascii_lowercase().as_str() {
        "unknown" => Some(RequirementStatus::Unknown),
        "satisfied" => Some(RequirementStatus::Satisfied),
        "failed" => Some(RequirementStatus::Failed),
        "blocked" => Some(RequirementStatus::Blocked),
        "waived" => Some(RequirementStatus::Waived),
        _ => None,
    }
}

/// Parse status updates but deliberately do not trust them yet. The graph
/// validates ids and evidence references against [`AgentState`]'s ledger.
pub(crate) fn parse_requirement_updates(call: &ToolCall) -> Result<Vec<RequirementUpdate>, String> {
    let items = call
        .arguments
        .get("requirements")
        .and_then(|value| value.as_array())
        .ok_or_else(|| "requirements must be an array".to_string())?;
    if items.is_empty() || items.len() > 32 {
        return Err("requirements must contain 1 to 32 items".to_string());
    }
    let mut updates = Vec::with_capacity(items.len());
    for item in items {
        let object = item
            .as_object()
            .ok_or_else(|| "each requirement update must be an object".to_string())?;
        let id = object
            .get("id")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|id| valid_requirement_id(id))
            .ok_or_else(|| "requirement update needs a valid id".to_string())?
            .to_string();
        if updates
            .iter()
            .any(|update: &RequirementUpdate| update.id == id)
        {
            return Err(format!("duplicate requirement update `{id}`"));
        }
        let status = object
            .get("status")
            .and_then(|value| value.as_str())
            .and_then(parse_requirement_status)
            .ok_or_else(|| format!("requirement `{id}` has an invalid status"))?;
        let evidence_call_ids = object
            .get("evidence_call_ids")
            .map(|value| {
                let values = value
                    .as_array()
                    .ok_or_else(|| "evidence_call_ids must be an array".to_string())?;
                if values.len() > 8 {
                    return Err("too many evidence_call_ids".to_string());
                }
                values
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .ok_or_else(|| "evidence_call_ids must be strings".to_string())
                            .and_then(|value| bounded_text(value, "evidence call id"))
                    })
                    .collect()
            })
            .transpose()?
            .unwrap_or_default();
        updates.push(RequirementUpdate {
            id,
            status,
            evidence_call_ids,
        });
    }
    Ok(updates)
}

/// 失败的 run_shell 观察是否该附「Unix 语法撞 PowerShell」纠错提示:
/// 弱模型惯发 `ls -la`/`grep`/`cat`/`&&`/`~/` 等 bash 语法,撞上 Windows 默认 PowerShell 条条失败、
/// 空耗回合(半途而废主因之一)。命中 Unix 特征 **且** 用的是 PowerShell/cmd → 给一句可执行的自愈路径。
/// 已传 bash/sh/pwsh 则不提示(那不是 PS 语法问题)。
fn unix_syntax_hint(cmd: &str, shell_used: &str) -> Option<&'static str> {
    if !matches!(shell_used, "powershell" | "cmd") {
        return None;
    }
    const UNIXISMS: [&str; 14] = [
        "ls -",
        " -la",
        " -al",
        "grep ",
        "cat ",
        "head -",
        "tail -",
        "rm -",
        "mkdir -p",
        "~/",
        "/dev/null",
        "export ",
        "sed -",
        " && ",
    ];
    UNIXISMS.iter().any(|p| cmd.contains(p)).then_some(
        "  💡 失败疑因把 Unix/bash 语法用在 PowerShell:改用 PS 写法(ls、Select-String、Get-Content;\
         多命令用 `;` 串联而非 `&&`),或给 run_shell 传 shell:\"bash\"(若 host_env 列了 bash)重试。",
    )
}

/// 执行一个结构化工具调用,返回给模型看的观察结果(observation)。用真实的 `tools` crate 干活。
/// iter-40:前后各串一层 hook(pre_tool 可拦截 / post_tool fire-and-forget)。
fn tool_arg<'a>(call: &'a ToolCall, key: &str) -> &'a str {
    call.arguments
        .get(key)
        .and_then(|value| value.as_str())
        .unwrap_or("")
}

/// Native result emitted by built-in tools.  Text remains the provider/UI
/// observation, but the outcome fields are decided at the execution site
/// rather than parsed from that text later.
pub(crate) struct ExecutedToolResult {
    pub observation: String,
    pub typed: ToolResultV1,
}

struct ToolResult {
    observation: String,
    run_post_hooks: bool,
    status: ToolResultStatus,
    exit_code: Option<i32>,
    retryable: bool,
    changed_paths: Vec<String>,
    output_truncated: bool,
    read_offset: Option<usize>,
    read_limit: Option<usize>,
    match_count: Option<usize>,
}

fn tool_result(observation: String) -> ToolResult {
    ToolResult {
        observation,
        run_post_hooks: true,
        status: ToolResultStatus::Success,
        exit_code: None,
        retryable: false,
        changed_paths: Vec::new(),
        output_truncated: false,
        read_offset: None,
        read_limit: None,
        match_count: None,
    }
}

fn changed_result(observation: String, changed_paths: Vec<String>) -> ToolResult {
    ToolResult {
        changed_paths,
        ..tool_result(observation)
    }
}

fn error_result(observation: String) -> ToolResult {
    ToolResult {
        observation,
        run_post_hooks: true,
        status: ToolResultStatus::Error,
        exit_code: None,
        retryable: true,
        changed_paths: Vec::new(),
        output_truncated: false,
        read_offset: None,
        read_limit: None,
        match_count: None,
    }
}

fn blocked_result(observation: String) -> ToolResult {
    ToolResult {
        observation,
        run_post_hooks: false,
        status: ToolResultStatus::Blocked,
        exit_code: None,
        retryable: false,
        changed_paths: Vec::new(),
        output_truncated: false,
        read_offset: None,
        read_limit: None,
        match_count: None,
    }
}

fn running_result(observation: String) -> ToolResult {
    ToolResult {
        observation,
        run_post_hooks: true,
        status: ToolResultStatus::Running,
        exit_code: None,
        retryable: false,
        changed_paths: Vec::new(),
        output_truncated: false,
        read_offset: None,
        read_limit: None,
        match_count: None,
    }
}

fn finished_shell_result(result: tools::ShellResult, cmd: &str, used_shell: &str) -> ToolResult {
    let code = result.code;
    let observation = format_finished_shell(result, cmd, used_shell);
    if code == 0 {
        ToolResult {
            exit_code: Some(code),
            ..tool_result(observation)
        }
    } else {
        ToolResult {
            observation,
            run_post_hooks: true,
            status: ToolResultStatus::Error,
            exit_code: Some(code),
            retryable: matches!(code, 1 | 124 | 429 | 500..=599),
            changed_paths: Vec::new(),
            output_truncated: false,
            read_offset: None,
            read_limit: None,
            match_count: None,
        }
    }
}

fn execute_shell_tool(call: &ToolCall) -> ToolResult {
    let cancel_job_id = tool_arg(call, "cancel_job_id").trim();
    if !cancel_job_id.is_empty() {
        return match tools::cancel_shell_job(cancel_job_id) {
            Ok(observation) => format_shell_observation(observation, "", tools::default_shell()),
            Err(error) => error_result(format!("shell error: {error}")),
        };
    }
    let job_id = tool_arg(call, "job_id").trim();
    if !job_id.is_empty() {
        return match tools::poll_shell_job(job_id) {
            Ok(observation) => format_shell_observation(observation, "", tools::default_shell()),
            Err(error) => error_result(format!("shell error: {error}")),
        };
    }
    let cmd = tool_arg(call, "cmd");
    if cmd.is_empty() {
        return blocked_result("run_shell error: 缺少 cmd 或 job_id".into());
    }
    if let Some(why) = tools::is_dangerous_command(cmd) {
        tracing::warn!(tool = %call.name, reason = %why, "blocked dangerous command");
        return blocked_result(format!("BLOCKED (dangerous: {why}) — 拒绝执行 `{cmd}`"));
    }
    if let Some(message) = constraint_guard_shell(cmd) {
        return blocked_result(message);
    }
    if let Some(sandbox) = active_sandbox_cmd() {
        let cwd = std::env::current_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        tracing::debug!(sandbox = %sandbox, "run_shell via sandbox");
        return match tools::run_argv(&sandbox_argv(&sandbox, cmd, &cwd)) {
            Ok(result) => finished_shell_result(result, cmd, tools::default_shell()),
            Err(error) => error_result(format!("shell error: {error}")),
        };
    }
    let shell = tool_arg(call, "shell");
    let used = if shell.is_empty() {
        tools::default_shell()
    } else {
        shell
    };
    match tools::run_or_park_shell((!shell.is_empty()).then_some(shell), cmd) {
        Ok(observation) => format_shell_observation(observation, cmd, used),
        Err(error) => error_result(format!("shell error: {error}")),
    }
}

fn format_shell_observation(
    observation: tools::ShellObservation,
    cmd: &str,
    used_shell: &str,
) -> ToolResult {
    match observation {
        tools::ShellObservation::Finished(result) => finished_shell_result(result, cmd, used_shell),
        tools::ShellObservation::Running(progress) => running_result(format!(
                "job {} running elapsed={}s\nstdout_tail:\n{}\nstderr_tail:\n{}\nCall run_shell with job_id=\"{}\" to poll. Do not restart this command. A live job blocks completion.",
                progress.id,
                progress.elapsed_ms / 1000,
                tail_text(&progress.stdout, 2000),
                tail_text(&progress.stderr, 1000),
                progress.id
            )),
    }
}

fn format_finished_shell(result: tools::ShellResult, cmd: &str, used_shell: &str) -> String {
    let mut observation = format!(
        "exit {}: {}{}",
        result.code,
        result.stdout.trim(),
        result.stderr.trim()
    );
    if result.code != 0 {
        if let Some(hint) = unix_syntax_hint(cmd, used_shell) {
            observation.push('\n');
            observation.push_str(hint);
        }
    }
    if let Some(hint) = file_editor_shell_hint(cmd) {
        observation.push('\n');
        observation.push_str(hint);
    }
    observation
}

fn tail_text(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_string();
    }
    text.chars().skip(total - max_chars).collect()
}

fn file_editor_shell_hint(cmd: &str) -> Option<&'static str> {
    let lower = cmd.to_ascii_lowercase();
    const MARKERS: [&str; 8] = [
        "get-content",
        "set-content",
        "select-string",
        "rg ",
        "rg.exe",
        "[io.file]::",
        "out-file",
        "add-content",
    ];
    MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
        .then_some(
            "  💡 Use search/read_file/edit_file for source; run_shell is for build/test/package.",
        )
}

fn execute_write_file_tool(call: &ToolCall) -> ToolResult {
    let path = tool_arg(call, "path");
    if let Err(error) = jail(path) {
        return blocked_result(error);
    }
    let contents = tool_arg(call, "contents");
    if let Some(message) = constraint_guard_write(path, contents) {
        return blocked_result(message);
    }
    match tools::write_file(path, contents) {
        Ok(()) => changed_result(
            format!("wrote {} bytes to {path}", contents.len()),
            vec![path.to_string()],
        ),
        Err(error) => error_result(format!("write error: {error}")),
    }
}

fn execute_edit_file_tool(call: &ToolCall) -> ToolResult {
    let path = tool_arg(call, "path");
    if let Err(error) = jail(path) {
        return blocked_result(error);
    }
    if let Some(expected) = call
        .arguments
        .get("expected_hash")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
    {
        let current = match tools::read_file(path) {
            Ok(current) => current,
            Err(error) => return error_result(format!("edit precondition read error: {error}")),
        };
        let actual = file_sha256(&current);
        if !actual.eq_ignore_ascii_case(expected.trim()) {
            return blocked_result(format!(
                "BLOCKED (stale edit): expected_hash mismatch for {path}; reread the file before editing"
            ));
        }
    }
    match tools::edit_file(
        path,
        tool_arg(call, "old_string"),
        tool_arg(call, "new_string"),
    ) {
        Ok(()) => changed_result(format!("edited {path}"), vec![path.to_string()]),
        Err(error) => error_result(format!("edit error: {error}")),
    }
}

fn file_sha256(contents: &str) -> String {
    Sha256::digest(contents.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn execute_apply_edits_tool(call: &ToolCall) -> ToolResult {
    let edits = parse_edits(call);
    if edits.is_empty() {
        return blocked_result(format!(
            "apply_edits error: 缺少 edits —— 传 edits: [{{path, old_string, new_string}}]; 失败后从最近 read 的锚点原样复制,勿重启全库侦察. args={}",
            call.arguments
        ));
    }
    for edit in &edits {
        if let Err(message) = jail(&edit.path) {
            return blocked_result(message);
        }
    }
    if let Some(message) = check_batch_preconditions(call) {
        return blocked_result(message);
    }
    match tools::apply_edits(&edits) {
        Ok(count) => changed_result(
            format!("applied {count} 个文件的批量编辑"),
            edits.into_iter().map(|edit| edit.path).collect(),
        ),
        Err(error) => error_result(format!("apply_edits error: {error}")),
    }
}

fn check_batch_preconditions(call: &ToolCall) -> Option<String> {
    let mut checks = Vec::new();
    if let Some(items) = call
        .arguments
        .get("edits")
        .and_then(|value| value.as_array())
    {
        for item in items {
            let path = item
                .get("path")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let expected = item
                .get("expected_hash")
                .and_then(|value| value.as_str())
                .filter(|value| !value.trim().is_empty())
                .unwrap_or("");
            if !path.is_empty() && !expected.is_empty() {
                checks.push((path, expected));
            }
        }
    } else if let (Some(path), Some(expected)) = (
        call.arguments.get("path").and_then(|value| value.as_str()),
        call.arguments
            .get("expected_hash")
            .and_then(|value| value.as_str()),
    ) {
        if !path.is_empty() && !expected.trim().is_empty() {
            checks.push((path, expected));
        }
    }
    for (path, expected) in checks {
        let current = match tools::read_file(path) {
            Ok(current) => current,
            Err(error) => return Some(format!("edit precondition read error: {error}")),
        };
        if !file_sha256(&current).eq_ignore_ascii_case(expected.trim()) {
            return Some(format!(
                "BLOCKED (stale edit): expected_hash mismatch for {path}; reread the file before editing"
            ));
        }
    }
    None
}

fn execute_read_file_tool(call: &ToolCall) -> ToolResult {
    let number = |key: &str| call.arguments.get(key).and_then(|value| value.as_u64());
    let (offset, limit) = (number("offset"), number("limit"));
    let result = if offset.is_some() || limit.is_some() {
        tools::read_file_range(
            tool_arg(call, "path"),
            offset.unwrap_or(1).max(1) as usize,
            limit.unwrap_or(2000) as usize,
        )
    } else {
        tools::read_file(tool_arg(call, "path"))
    };
    let mut output = match result {
        Ok(contents) => tool_result(contents),
        Err(error) => error_result(format!("read error: {error}")),
    };
    output.read_offset = offset.map(|value| value as usize);
    output.read_limit = limit.map(|value| value as usize);
    output
}

fn execute_search_tool(call: &ToolCall) -> ToolResult {
    let value_or = |key: &str, default: &'static str| {
        let value = tool_arg(call, key);
        if value.is_empty() {
            default.to_string()
        } else {
            value.to_string()
        }
    };
    let mut output = match tools::search(
        value_or("path", "."),
        tool_arg(call, "pattern"),
        &value_or("glob", "*"),
    ) {
        Ok(contents) if contents.is_empty() => tool_result("(no matches)".to_string()),
        Ok(contents) => tool_result(contents),
        Err(error) => error_result(format!("search error: {error}")),
    };
    if output.status.is_success() {
        output.match_count = Some(if output.observation == "(no matches)" {
            0
        } else {
            output
                .observation
                .lines()
                .filter(|line| !line.starts_with("… (命中超过"))
                .count()
        });
        output.output_truncated = output.observation.contains("已截断");
    }
    output
}

fn execute_signal_tool(call: &ToolCall) -> ToolResult {
    let resolve = tool_arg(call, "resolve");
    if !resolve.is_empty() {
        return match signal_resolve(SIGNALS_DIR, resolve) {
            // Resolving bookkeeping is a completed control-plane action,
            // not a blocked task.  Marking it Blocked poisoned the final
            // typed result and prevented otherwise successful read-only runs
            // from completing.
            Ok(true) => tool_result(format!("signal resolved: {resolve}")),
            Ok(false) => error_result(format!("signal 未找到: {resolve}")),
            Err(error) => error_result(format!("signal error: {error}")),
        };
    }
    let body = tool_arg(call, "body");
    if body.is_empty() {
        return blocked_result("signal error: 缺少 body".to_string());
    }
    let kind = if tool_arg(call, "type").is_empty() {
        "note"
    } else {
        tool_arg(call, "type")
    };
    match signal_create(SIGNALS_DIR, kind, body, "manual") {
        Ok(id) => tool_result(format!("signal recorded: {id}")),
        Err(error) => error_result(format!("signal error: {error}")),
    }
}

fn execute_tool_body(call: &ToolCall) -> ToolResult {
    match call.name.as_str() {
        "run_shell" => execute_shell_tool(call),
        "write_file" => execute_write_file_tool(call),
        "edit_file" => execute_edit_file_tool(call),
        "apply_edits" => execute_apply_edits_tool(call),
        "read_file" => execute_read_file_tool(call),
        "search" => execute_search_tool(call),
        "todo_write" => tool_result(format!("已更新任务清单 {} 项", parse_todos(call).len())),
        "contract_write" => match parse_task_contract(call) {
            Ok(contract) => tool_result(format!(
                "task contract recorded: {} requirement(s)",
                contract.requirements.len()
            )),
            Err(error) => error_result(format!("contract error: {error}")),
        },
        "requirement_update" => match parse_requirement_updates(call) {
            Ok(updates) => tool_result(format!(
                "requirement update parsed: {} item(s)",
                updates.len()
            )),
            Err(error) => error_result(format!("requirement error: {error}")),
        },
        "signal_write" => execute_signal_tool(call),
        other => error_result(format!(
            "tool error: 未知工具 `{other}`;请只调用系统所列工具"
        )),
    }
}

fn native_tool_result(
    call: &ToolCall,
    effect: ToolEffect,
    result: ToolResult,
) -> ExecutedToolResult {
    let mut changed_paths = result.changed_paths;
    changed_paths.sort();
    changed_paths.dedup();
    let summary = result
        .observation
        .lines()
        .next()
        .unwrap_or(&result.observation)
        .chars()
        .take(512)
        .collect();
    ExecutedToolResult {
        typed: ToolResultV1 {
            schema_version: 1,
            call_id: call.id.clone(),
            tool: call.name.clone(),
            effect,
            status: result.status,
            exit_code: result.exit_code,
            retryable: result.retryable,
            changed_paths,
            output_truncated: result.output_truncated,
            read_offset: result.read_offset,
            read_limit: result.read_limit,
            match_count: result.match_count,
            workspace_revision: 0,
            summary,
        },
        observation: result.observation,
    }
}

/// Execute a built-in tool and retain its execution-site outcome metadata.
/// Dynamic/MCP transports use the compatibility projection until they expose
/// the same schema natively.
pub(crate) fn execute_tool_call_native(call: &ToolCall, effect: ToolEffect) -> ExecutedToolResult {
    if let Some(blocked) = run_pre_tool_hooks(call) {
        return native_tool_result(call, effect, blocked_result(blocked));
    }
    tracing::debug!(tool = %call.name, "tool call");
    let result = execute_tool_body(call);
    if result.run_post_hooks {
        run_post_tool_hooks(call);
    }
    tracing::debug!(
        tool = %call.name,
        status = ?result.status,
        "tool done"
    );
    native_tool_result(call, effect, result)
}

/// Compatibility API for tests and legacy callers that only consume display
/// observations. Runtime graph code uses [`execute_tool_call_native`].
pub fn execute_tool_call(call: &ToolCall) -> String {
    execute_tool_call_native(call, ToolEffect::from_name(&call.name)).observation
}

#[cfg(test)]
mod tests {
    use super::{
        builtin_tool_specs, durable_updates, durable_updates_with_effect, execute_tool_call,
        execute_tool_call_native, file_sha256, parse_requirement_updates, parse_task_contract,
        tool_result_v1, unix_syntax_hint,
    };
    use crate::brain::{tool_output_failed, tool_output_ok};
    use crate::context::durable_state_block;
    use crate::exec::is_error_observation;
    use crate::observe::preview_call;
    use crate::{build_llm_agent_gated, shell_tool, AgentState, AutoDeny, McpTools, MAX_STEPS};
    use langgraph::GraphState;
    use provider::{ToolCall, ToolEffect};
    use std::sync::Arc;

    /// Unix 语法撞 PowerShell 的纠错提示:命中 bash 特征且用 PS/cmd → 提示;已用 bash 或本就是 PS 命令 → 不提示。
    #[test]
    fn unix_syntax_hint_only_fires_for_bashism_on_powershell() {
        assert!(unix_syntax_hint("ls -la ~/.ridge", "powershell").is_some());
        assert!(unix_syntax_hint("cat foo && grep bar", "cmd").is_some());
        // 已显式用 bash → 不是 PS 语法问题,不提示。
        assert!(unix_syntax_hint("ls -la ~/.ridge", "bash").is_none());
        // 纯 PowerShell 命令(无 bash 特征)失败 → 不误报(如真实构建/命令错)。
        assert!(unix_syntax_hint("Get-ChildItem C:\\code", "powershell").is_none());
        assert!(unix_syntax_hint("cargo build", "powershell").is_none());
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

    /// iter-44:核心动作可观测 —— 危险命令拦截被 tracing 观测到(确定性、无文件副作用)。
    /// 线程本地 subscriber 捕获:execute_tool_call 同步、在测试线程跑,故可捕。
    #[test]
    fn execute_tool_call_traces_blocked_dangerous_command() {
        use std::io::Write;
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct BufWriter(Arc<Mutex<Vec<u8>>>);
        impl Write for BufWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for BufWriter {
            type Writer = BufWriter;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let sub = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .with_writer(BufWriter(buf.clone()))
            .finish();

        let out = tracing::subscriber::with_default(sub, || {
            let call = ToolCall {
                id: "d".to_string(),
                name: "run_shell".to_string(),
                arguments: serde_json::json!({"cmd": "rm -rf /"}),
            };
            execute_tool_call(&call)
        });
        assert!(out.starts_with("BLOCKED"), "{out}"); // 无文件副作用
        let logged = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(logged.contains("run_shell"), "logged: {logged}");
        let low = logged.to_lowercase();
        assert!(
            low.contains("blocked") || low.contains("dangerous"),
            "logged: {logged}"
        );
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

    #[test]
    fn edit_file_rejects_stale_expected_hash_before_side_effect() {
        let path = std::env::current_dir().unwrap().join(format!(
            "target/ridge-stale-edit-{}.txt",
            std::process::id()
        ));
        std::fs::write(&path, "before\n").unwrap();
        let call = ToolCall {
            id: "stale-edit".into(),
            name: "edit_file".into(),
            arguments: serde_json::json!({
                "path": path.to_string_lossy(),
                "old_string": "before",
                "new_string": "after",
                "expected_hash": "0000000000000000000000000000000000000000000000000000000000000000"
            }),
        };
        let blocked = execute_tool_call(&call);
        assert!(blocked.starts_with("BLOCKED (stale edit)"), "{blocked}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "before\n");

        let mut fresh = call.clone();
        fresh.arguments["expected_hash"] = serde_json::Value::String(file_sha256("before\n"));
        let edited = execute_tool_call(&fresh);
        assert!(edited.starts_with("edited"), "{edited}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "after\n");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn batch_edit_rejects_any_stale_precondition_atomically() {
        let dir = std::env::current_dir()
            .unwrap()
            .join(format!("target/ridge-stale-batch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("first.txt");
        let second = dir.join("second.txt");
        std::fs::write(&first, "one\n").unwrap();
        std::fs::write(&second, "two\n").unwrap();
        let call = ToolCall {
            id: "stale-batch".into(),
            name: "apply_edits".into(),
            arguments: serde_json::json!({"edits": [
                {"path": first.to_string_lossy(), "old_string": "one", "new_string": "ONE", "expected_hash": file_sha256("one\n")},
                {"path": second.to_string_lossy(), "old_string": "two", "new_string": "TWO", "expected_hash": "00"}
            ]}),
        };
        let blocked = execute_tool_call(&call);
        assert!(blocked.starts_with("BLOCKED (stale edit)"), "{blocked}");
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "one\n");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "two\n");
        let _ = std::fs::remove_dir_all(dir);
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

    #[test]
    fn task_contract_and_requirement_updates_are_strictly_parsed() {
        let contract_call = ToolCall {
            id: "contract".into(),
            name: "contract_write".into(),
            arguments: serde_json::json!({
                "objective": "ship a verified change",
                "requirements": [
                    {"id": "edit", "description": "make the requested edit"},
                    {"description": "run the target test"}
                ],
                "constraints": ["do not change tests"]
            }),
        };
        let contract = parse_task_contract(&contract_call).expect("valid contract");
        assert_eq!(contract.requirements[0].id, "edit");
        assert_eq!(contract.requirements[1].id, "R2");
        assert!(contract
            .requirements
            .iter()
            .all(|requirement| requirement.status == crate::RequirementStatus::Unknown));

        let update_call = ToolCall {
            id: "update".into(),
            name: "requirement_update".into(),
            arguments: serde_json::json!({"requirements": [{
                "id": "edit", "status": "satisfied", "evidence_call_ids": ["edit-1"]
            }]}),
        };
        let updates = parse_requirement_updates(&update_call).expect("valid update");
        assert_eq!(updates[0].id, "edit");
        assert_eq!(updates[0].status, crate::RequirementStatus::Satisfied);
        assert!(parse_requirement_updates(&ToolCall {
            id: "bad".into(),
            name: "requirement_update".into(),
            arguments: serde_json::json!({"requirements": [{"id": "bad id", "status": "done"}]}),
        })
        .is_err());
    }

    #[test]
    fn tool_result_v1_is_typed_and_never_treats_running_as_success() {
        let edit = ToolCall {
            id: "edit-1".into(),
            name: "edit_file".into(),
            arguments: serde_json::json!({"path": "src/lib.rs"}),
        };
        let changed = tool_result_v1(&edit, ToolEffect::Edit, "edited src/lib.rs");
        assert_eq!(changed.status, crate::ToolResultStatus::Success);
        assert_eq!(changed.changed_paths, vec!["src/lib.rs"]);

        let shell = ToolCall {
            id: "shell-1".into(),
            name: "run_shell".into(),
            arguments: serde_json::json!({"cmd": "cargo test"}),
        };
        let running = tool_result_v1(
            &shell,
            ToolEffect::Verify,
            "job sh-1 running elapsed=1s\nCall run_shell with job_id=\"sh-1\" to poll.",
        );
        assert_eq!(running.status, crate::ToolResultStatus::Running);
        assert!(!running.status.is_success());
        assert!(running.status.blocks_completion());
        let failed = tool_result_v1(&shell, ToolEffect::Verify, "exit 1: failed");
        assert_eq!(failed.status, crate::ToolResultStatus::Error);
        assert_eq!(failed.exit_code, Some(1));
        assert!(failed.retryable);
        assert!(failed.status.blocks_completion());
    }

    #[test]
    fn native_builtin_result_does_not_infer_status_from_file_contents() {
        let path = std::env::current_dir().unwrap().join(format!(
            "target/ridge-native-result-{}.txt",
            std::process::id()
        ));
        std::fs::write(
            &path,
            "read error: this is ordinary file content\nsecond line",
        )
        .unwrap();
        let call = ToolCall {
            id: "native-read".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": path.to_string_lossy(), "offset": 1, "limit": 1}),
        };
        let result = execute_tool_call_native(&call, ToolEffect::Explore);
        assert_eq!(result.typed.status, crate::ToolResultStatus::Success);
        assert!(!result.typed.retryable);
        assert_eq!(result.typed.read_offset, Some(1));
        assert_eq!(result.typed.read_limit, Some(1));
        assert!(result.observation.starts_with("read error:"));

        let search = ToolCall {
            id: "native-search".into(),
            name: "search".into(),
            arguments: serde_json::json!({
                "path": "target",
                "pattern": "ridgecode-no-such-marker-9e4f"
            }),
        };
        let search_result = execute_tool_call_native(&search, ToolEffect::Explore);
        assert_eq!(search_result.typed.match_count, Some(0));
        assert!(!search_result.typed.output_truncated);
        let _ = std::fs::remove_file(path);
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

    /// 熔断漏判修复(iter-51):非零 `exit N` 必判错(即便正文无 ASCII " error:",如中文 GBK 报错),
    /// 与 verify 侧 tool_output_failed 对齐;`exit 0` 不误判。
    #[test]
    fn nonzero_exit_is_error_observation() {
        assert!(is_error_observation(
            "exit 1: 文件名、目录名或卷标语法不正确。"
        ));
        assert!(is_error_observation("exit 127: 'ls' 不是内部或外部命令"));
        assert!(!is_error_observation("exit 0: 一切正常"), "exit 0 不该误判");
        assert_eq!(
            is_error_observation("exit 1: 文件名、目录名或卷标语法不正确。"),
            tool_output_failed("exit 1: 文件名、目录名或卷标语法不正确。")
        );
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
    fn dynamic_edit_backfills_paths_but_unknown_action_does_not() {
        let dynamic = ToolCall {
            id: "mcp-edit".into(),
            name: "records__opaque_action".into(),
            arguments: serde_json::json!({"file_path": "src/dynamic.rs"}),
        };
        let mut state = AgentState::new("edit src/dynamic.rs");
        for patch in durable_updates_with_effect(&dynamic, ToolEffect::Edit, "updated record") {
            state.apply(patch);
        }
        assert!(state.modified_files.contains("src/dynamic.rs"));

        let unknown = ToolCall {
            id: "mcp-unknown".into(),
            name: "records__mystery".into(),
            arguments: serde_json::json!({"path": "src/unknown.rs"}),
        };
        let mut unknown_state = AgentState::new("edit src/unknown.rs");
        for patch in
            durable_updates_with_effect(&unknown, ToolEffect::Unknown, "completed successfully")
        {
            unknown_state.apply(patch);
        }
        assert!(unknown_state.modified_files.is_empty());
        assert!(unknown_state.last_read_paths.is_empty());
    }

    #[test]
    fn unknown_action_upgrades_only_on_structured_changed_paths() {
        let call = ToolCall {
            id: "mcp-edit".into(),
            name: "records__opaque_action".into(),
            arguments: serde_json::json!({}),
        };
        let observation = r#"{"changed_paths":["src/record.rs"]}"#;
        assert_eq!(
            super::effective_tool_effect(ToolEffect::Unknown, observation),
            ToolEffect::Edit
        );
        let mut state = AgentState::new("edit src/record.rs");
        for patch in durable_updates_with_effect(&call, ToolEffect::Unknown, observation) {
            state.apply(patch);
        }
        assert!(state.modified_files.contains("src/record.rs"));
        assert_eq!(
            super::effective_tool_effect(ToolEffect::Unknown, "completed successfully"),
            ToolEffect::Unknown
        );
    }

    #[test]
    fn durable_state_block_stays_bounded_over_steps() {
        let mut st = AgentState::new("t");
        let block_len = |st: &AgentState| {
            durable_state_block(st)
                .map(|b| b.chars().count())
                .unwrap_or(0)
        };
        let mut max_len = 0;
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
            max_len = max_len.max(now);
            assert!(now <= 512, "事实块应有界(step {i} 的 {now} chars 超出 512)");
        }
        assert!(max_len > 0, "有工具结果时应注入事实块");
        assert_eq!(st.modified_files.len(), 2, "去重后仅 2 个文件");
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
        use provider::{Completion, ScriptedProvider, ToolCall};
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
        // 近上限起跑:被拒→verify 失败→重试,本会一路到步上限;seed 令两步即触达软中止,快且断言不变。
        let start = AgentState {
            steps: MAX_STEPS - 2,
            ..AgentState::new("build")
        };
        let out = app.invoke(start).await.unwrap();

        assert!(out.messages.iter().any(|m| m.contains("permission denied")));
        assert!(!out.approved, "被拒的工具没真跑,拿不到 exit 0");
    }

    #[test]
    fn execute_edit_file_reports_reusable_anchor_then_succeeds() {
        let path = std::env::current_dir()
            .unwrap()
            .join(format!("ridge-edit-exec-{}.txt", std::process::id()));
        std::fs::write(&path, "alpha\r\nbeta\r\ngamma\r\n").unwrap();
        let path_str = path.to_string_lossy().to_string();
        let miss = execute_tool_call(&ToolCall {
            id: "e1".into(),
            name: "edit_file".into(),
            arguments: serde_json::json!({
                "path": path_str,
                "old_string": "nope",
                "new_string": "x"
            }),
        });
        assert!(miss.contains("file anchor"), "{miss}");
        assert!(miss.contains("beta"), "{miss}");
        let hit = execute_tool_call(&ToolCall {
            id: "e2".into(),
            name: "edit_file".into(),
            arguments: serde_json::json!({
                "path": path_str,
                "old_string": "beta\ngamma",
                "new_string": "BETA\nGAMMA"
            }),
        });
        assert!(hit.contains("edited"), "{hit}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "alpha\r\nBETA\r\nGAMMA\r\n"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn execute_run_shell_polls_parked_job_through_shipped_entry() {
        #[cfg(windows)]
        let cmd = "Start-Sleep -Seconds 1; Write-Output exec-park";
        #[cfg(not(windows))]
        let cmd = "sleep 1; echo exec-park";
        #[cfg(windows)]
        let shell = Some("powershell");
        #[cfg(not(windows))]
        let shell = Some("sh");
        let first = tools::run_or_park_shell_with_limits(
            shell,
            cmd,
            std::time::Duration::from_millis(200),
            std::time::Duration::from_secs(10),
        )
        .unwrap();
        let id = match first {
            tools::ShellObservation::Running(progress) => progress.id,
            tools::ShellObservation::Finished(result) => {
                assert_eq!(result.code, 0, "{}{}", result.stdout, result.stderr);
                return;
            }
        };
        let started = std::time::Instant::now();
        let done = loop {
            let obs = execute_tool_call(&ToolCall {
                id: "s2".into(),
                name: "run_shell".into(),
                arguments: serde_json::json!({ "job_id": id }),
            });
            if obs.starts_with("exit ") {
                break obs;
            }
            assert!(!obs.contains("timed out after 180000ms"), "{obs}");
            // The parked shell itself is allowed up to ten seconds.  Keep
            // the test margin above that contract so a cold PowerShell
            // startup on a busy Windows runner cannot become a false
            // timeout (the previous eight-second assertion was shorter than
            // the production limit it was testing).
            if started.elapsed() > std::time::Duration::from_secs(15) {
                panic!("poll did not finish: {obs}");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        assert!(done.starts_with("exit 0:"), "{done}");
        assert!(done.contains("exec-park"), "{done}");
    }

    /// 30 秒任务超时上限 -> parked;shipped `execute_tool_call` 的 `cancel_job_id` 入口能取消它;
    /// 取消观察(非 error)驱动 [`durable_updates`] 回 RemoveLiveShellJob,`apply` 后 live 表清空。
    #[test]
    fn execute_run_shell_cancels_parked_job_through_shipped_entry_and_clears_state() {
        #[cfg(windows)]
        let cmd = "Start-Sleep -Seconds 30; Write-Output exec-cancel";
        #[cfg(not(windows))]
        let cmd = "sleep 30; echo exec-cancel";
        #[cfg(windows)]
        let shell = "powershell";
        #[cfg(not(windows))]
        let shell = "sh";
        let first = tools::run_or_park_shell_with_limits(
            Some(shell),
            cmd,
            std::time::Duration::from_millis(200),
            std::time::Duration::from_secs(10),
        )
        .unwrap();
        let id = match first {
            tools::ShellObservation::Running(progress) => progress.id,
            tools::ShellObservation::Finished(result) => {
                // 30 秒命令在 200ms 内就结束 = 环境根本没起真 shell,取消路径无从谈起。
                panic!("30s command finished early: {}", result.stdout);
            }
        };

        // 构造 run_shell 起始调用与 shipped 格式一致的 running 观察,
        // 逐个 apply durable_updates 后 live_shell_jobs 应有该 id。
        let start_call = ToolCall {
            id: "s1".into(),
            name: "run_shell".into(),
            arguments: serde_json::json!({ "cmd": cmd, "shell": shell }),
        };
        let running = format!("job {id} running elapsed=0s");
        let mut state = AgentState::new("cancel-job");
        for patch in durable_updates(&start_call, &running) {
            state.apply(patch);
        }
        assert_eq!(state.live_shell_jobs, vec![id.clone()]);

        // shipped 入口取消:canceled 观察非 error,不下 last_error;
        // durable_updates 以 cancel_job_id 参数定位 RemoveLiveShellJob。
        let cancel_call = ToolCall {
            id: "s3".into(),
            name: "run_shell".into(),
            arguments: serde_json::json!({ "cancel_job_id": id.clone() }),
        };
        let observation = execute_tool_call(&cancel_call);
        assert!(
            observation.contains("command cancelled"),
            "取消观察应含 command cancelled: {observation}"
        );
        for patch in durable_updates(&cancel_call, &observation) {
            state.apply(patch);
        }
        assert!(state.live_shell_jobs.is_empty(), "取消后 live 表应清空");
    }
}
