use crate::guard::{
    active_sandbox_cmd, constraint_guard_shell, constraint_guard_write, jail, run_post_tool_hooks,
    run_pre_tool_hooks, sandbox_argv,
};
use crate::signals::{signal_create, signal_resolve, SIGNALS_DIR};
use crate::state::{Patch, Todo};
use provider::{ToolCall, ToolSpec};

/// 内置工具的规格(喂给 LLM 让它按 schema 出结构化 tool_call)。
pub fn builtin_tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "run_shell".to_string(),
            description: "Run host build/test/pack. Not for files (use search/read/edit). >180s parks; poll or cancel job_id.".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"cmd":{"type":"string","description":"Command to start; omit when polling job_id"},"shell":{"type":"string","enum":["cmd","powershell","pwsh","bash","sh"],"description":"可选:执行用的 shell;省=宿主默认(见 host_env)"},"job_id":{"type":"string","description":"Poll a parked job from a previous run_shell"},"cancel_job_id":{"type":"string","description":"Cancel a parked job and return its bounded settlement"}},"required":[]}),
        },
        ToolSpec {
            name: "write_file".to_string(),
            description: "把内容整文件写入路径(覆盖)。仅用于**新建文件**;改动已有文件请用 edit_file".to_string(),
            schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"},"contents":{"type":"string"}},"required":["path","contents"]}),
        },
        ToolSpec {
            name: "edit_file".to_string(),
            description: "精准编辑:唯一 old_string→new_string。CRLF 对齐;失败用观察里的锚点再 edit。".to_string(),
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
            description: "按 glob+pattern 搜 路径:行号:内容；path 可为文件或目录。定位用它,目标已明勿全库搜。".to_string(),
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

pub(crate) fn durable_updates(call: &ToolCall, observation: &str) -> Vec<Patch> {
    let mut patches = Vec::new();
    if is_error_observation(observation) {
        let line = observation
            .lines()
            .next()
            .unwrap_or(observation)
            .to_string();
        patches.push(Patch::SetLastError(Some(line)));
    }
    let arg = |k: &str| call.arguments.get(k).and_then(|v| v.as_str());
    match call.name.as_str() {
        "write_file" | "edit_file" if !is_error_observation(observation) => {
            if let Some(path) = arg("path") {
                patches.push(Patch::RecordModified(path.to_string()));
                patches.push(Patch::SetLastError(None));
            }
        }
        "apply_edits" if !is_error_observation(observation) => {
            let edits = parse_edits(call);
            if !edits.is_empty() {
                patches.extend(edits.into_iter().map(|e| Patch::RecordModified(e.path)));
                patches.push(Patch::SetLastError(None));
            }
        }
        "read_file" if !is_error_observation(observation) => {
            if let Some(path) = arg("path") {
                patches.push(Patch::RecordRead(path.to_string()));
            }
        }
        "run_shell" => patches.extend(shell_job_updates(call, observation)),
        _ => {}
    }
    patches
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

struct ToolResult {
    observation: String,
    run_post_hooks: bool,
}

fn tool_result(observation: String) -> ToolResult {
    ToolResult {
        observation,
        run_post_hooks: true,
    }
}

fn blocked_result(observation: String) -> ToolResult {
    ToolResult {
        observation,
        run_post_hooks: false,
    }
}

fn execute_shell_tool(call: &ToolCall) -> ToolResult {
    let cancel_job_id = tool_arg(call, "cancel_job_id").trim();
    if !cancel_job_id.is_empty() {
        return match tools::cancel_shell_job(cancel_job_id) {
            Ok(observation) => tool_result(format_shell_observation(
                observation,
                "",
                tools::default_shell(),
            )),
            Err(error) => tool_result(format!("shell error: {error}")),
        };
    }
    let job_id = tool_arg(call, "job_id").trim();
    if !job_id.is_empty() {
        return match tools::poll_shell_job(job_id) {
            Ok(observation) => tool_result(format_shell_observation(
                observation,
                "",
                tools::default_shell(),
            )),
            Err(error) => tool_result(format!("shell error: {error}")),
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
            Ok(result) => tool_result(format_finished_shell(result, cmd, tools::default_shell())),
            Err(error) => tool_result(format!("shell error: {error}")),
        };
    }
    let shell = tool_arg(call, "shell");
    let used = if shell.is_empty() {
        tools::default_shell()
    } else {
        shell
    };
    match tools::run_or_park_shell((!shell.is_empty()).then_some(shell), cmd) {
        Ok(observation) => tool_result(format_shell_observation(observation, cmd, used)),
        Err(error) => tool_result(format!("shell error: {error}")),
    }
}

fn format_shell_observation(
    observation: tools::ShellObservation,
    cmd: &str,
    used_shell: &str,
) -> String {
    match observation {
        tools::ShellObservation::Finished(result) => format_finished_shell(result, cmd, used_shell),
        tools::ShellObservation::Running(progress) => {
            format!(
                "job {} running elapsed={}s\nstdout_tail:\n{}\nstderr_tail:\n{}\nCall run_shell with job_id=\"{}\" to poll. Do not restart this command. A live job blocks completion.",
                progress.id,
                progress.elapsed_ms / 1000,
                tail_text(&progress.stdout, 2000),
                tail_text(&progress.stderr, 1000),
                progress.id
            )
        }
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
        Ok(()) => tool_result(format!("wrote {} bytes to {path}", contents.len())),
        Err(error) => tool_result(format!("write error: {error}")),
    }
}

fn execute_edit_file_tool(call: &ToolCall) -> ToolResult {
    let path = tool_arg(call, "path");
    if let Err(error) = jail(path) {
        return blocked_result(error);
    }
    match tools::edit_file(
        path,
        tool_arg(call, "old_string"),
        tool_arg(call, "new_string"),
    ) {
        Ok(()) => tool_result(format!("edited {path}")),
        Err(error) => tool_result(format!("edit error: {error}")),
    }
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
    match tools::apply_edits(&edits) {
        Ok(count) => tool_result(format!("applied {count} 个文件的批量编辑")),
        Err(error) => tool_result(format!("apply_edits error: {error}")),
    }
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
    match result {
        Ok(contents) => tool_result(contents),
        Err(error) => tool_result(format!("read error: {error}")),
    }
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
    match tools::search(
        value_or("path", "."),
        tool_arg(call, "pattern"),
        &value_or("glob", "*"),
    ) {
        Ok(contents) if contents.is_empty() => tool_result("(no matches)".to_string()),
        Ok(contents) => tool_result(contents),
        Err(error) => tool_result(format!("search error: {error}")),
    }
}

fn execute_signal_tool(call: &ToolCall) -> ToolResult {
    let resolve = tool_arg(call, "resolve");
    if !resolve.is_empty() {
        return match signal_resolve(SIGNALS_DIR, resolve) {
            Ok(true) => blocked_result(format!("signal resolved: {resolve}")),
            Ok(false) => blocked_result(format!("signal 未找到: {resolve}")),
            Err(error) => blocked_result(format!("signal error: {error}")),
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
        Err(error) => tool_result(format!("signal error: {error}")),
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
        "signal_write" => execute_signal_tool(call),
        other => tool_result(format!(
            "tool error: 未知工具 `{other}`;请只调用系统所列工具"
        )),
    }
}

pub fn execute_tool_call(call: &ToolCall) -> String {
    if let Some(blocked) = run_pre_tool_hooks(call) {
        return blocked;
    }
    tracing::debug!(tool = %call.name, "tool call");
    let result = execute_tool_body(call);
    if result.run_post_hooks {
        run_post_tool_hooks(call);
    }
    tracing::debug!(
        tool = %call.name,
        ok = !is_error_observation(&result.observation),
        "tool done"
    );
    result.observation
}

#[cfg(test)]
mod tests {
    use super::{builtin_tool_specs, durable_updates, execute_tool_call, unix_syntax_hint};
    use crate::brain::{tool_output_failed, tool_output_ok};
    use crate::context::durable_state_block;
    use crate::exec::is_error_observation;
    use crate::observe::preview_call;
    use crate::{build_llm_agent_gated, shell_tool, AgentState, AutoDeny, McpTools, MAX_STEPS};
    use langgraph::GraphState;
    use provider::ToolCall;
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
            if started.elapsed() > std::time::Duration::from_secs(8) {
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
