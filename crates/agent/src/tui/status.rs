use std::borrow::Cow;

use super::*;

/// 上下文窗口人读化:200000 → "200K",1048576 → "1.0M"(纯函数)。
pub(crate) fn fmt_ctx(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{}K", n / 1_000)
    } else {
        n.to_string()
    }
}

// ───────────────────── 状态双栏与 ctx%(iter-31)─────────────────────

/// ctx% 分母未知时的兜底上下文窗口(现代模型常见档)。`/models` 命中当前模型即被真实窗口覆盖。
pub(crate) const DEFAULT_CTX_WINDOW: u64 = 200_000;
/// 输入框下方自定义状态条内置默认模板。config `status_bar` 留空时用它。
pub(crate) const DEFAULT_STATUS_BAR: &str = " {provider} · {model} · ctx {ctx} · {tokens} tok ";

/// 实时 token 速率(tok/s,纯函数):elapsed 为 0 → 0,防除零。
pub(crate) fn token_rate(tokens: usize, elapsed_ms: u128) -> u64 {
    (tokens as u128 * 1000).checked_div(elapsed_ms).unwrap_or(0) as u64
}

/// 实际 reasoning 首行的运行元数据；step 未收到同步点时省略，不伪造图进度。
pub(crate) fn fmt_reasoning_meta(step: usize, elapsed_s: u64, tokens: usize) -> String {
    let step = if step > 0 {
        format!("step {step} · ")
    } else {
        String::new()
    };
    format!("💭 [{step}t+{elapsed_s}s · {tokens} task tok] ")
}

/// 上下文占用百分比(纯函数):window 为 0 → 0;上限 100(压缩前估算,超窗即封顶)。
pub(crate) fn ctx_percent(used: usize, window: usize) -> u16 {
    (used * 100).checked_div(window).unwrap_or(0).min(100) as u16
}

/// 上下文压力仅依据当前已观测 `ctx%` 着色；不推断 token budget，也不触碰停机逻辑。
pub(crate) fn context_pressure_role(percent: u16) -> Role {
    match percent {
        95..=100 => Role::Error,
        80..=94 => Role::Warn,
        _ => Role::Muted,
    }
}

const TOOL_INTENT_NAME_WIDTH: u16 = 18;
const TOOL_INTENT_DETAIL_WIDTH: u16 = 24;

/// 忙碌粘条的安全工具意图:只展示低风险字段,不把命令/正文/凭据带入界面。
fn fmt_tool_intent(call: &provider::ToolCall) -> String {
    let name = clip_display_cells(&inline_display(&call.name), TOOL_INTENT_NAME_WIDTH);
    let detail = match call.name.as_str() {
        "read_file" | "write_file" | "edit_file" | "search" => call
            .arguments
            .get("path")
            .and_then(|v| v.as_str())
            .map(|path| format!(" · path={}", safe_path(path)))
            .unwrap_or_default(),
        "apply_edits" => call
            .arguments
            .get("edits")
            .and_then(|v| v.as_array())
            .map(|edits| format!(" · edits={}", edits.len()))
            .unwrap_or_default(),
        "todo_write" => call
            .arguments
            .get("todos")
            .and_then(|v| v.as_array())
            .map(|todos| format!(" · todos={}", todos.len()))
            .unwrap_or_default(),
        _ => String::new(),
    };
    format!(" · ◈ {name}{detail}")
}

fn inline_display(text: &str) -> String {
    sanitize_display_text(text)
        .chars()
        .map(|c| {
            if matches!(c, '\n' | '\r' | '\t') {
                ' '
            } else {
                c
            }
        })
        .collect()
}

fn safe_path(path: &str) -> String {
    let path = inline_display(path);
    let lower = path.to_ascii_lowercase();
    const SENSITIVE_MARKERS: &[&str] = &[
        "api_key",
        "apikey",
        "authorization",
        "bearer",
        "cookie",
        "credential",
        "password",
        "secret",
        "token",
    ];
    if SENSITIVE_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
    {
        "[redacted]".to_owned()
    } else {
        clip_display_cells(&path, TOOL_INTENT_DETAIL_WIDTH)
    }
}

/// 忙碌粘条文案(需求 6,纯函数):运行态 · 已观测 step · 读秒 · token 消耗 · 速率 · 任务进度 · 待跑队列。
/// todo 空则省略进度段;`queued>0` 追加 ` · ⏳N`(iter-33)。计时/计量全由入参给定 —— 零 wall-clock,可纯测。
pub(crate) fn fmt_busy_bar(
    phase: &str,
    todos: &[Todo],
    elapsed_s: u64,
    tokens: usize,
    rate: u64,
    queued: usize,
    pending_call: Option<&provider::ToolCall>,
) -> String {
    let tool = pending_call.map(fmt_tool_intent).unwrap_or_default();
    let mut s = format!("⚡ {phase}{tool} · ⏱ {elapsed_s}s · {tokens} tok · {rate} tok/s");
    if let Some((d, n)) = todo_progress(todos) {
        s.push_str(&format!(" · todo {d}/{n}"));
    }
    if queued > 0 {
        s.push_str(&format!(" · ⏳{queued}"));
    }
    s
}

/// 忙碌态已有阶段标签的轻量上下文：只显真实观测 step，不引入新状态或布局行。
pub(crate) fn fmt_busy_phase(phase: &str, step: usize) -> Cow<'_, str> {
    if step > 0 {
        Cow::Owned(format!("{phase} · step {step}"))
    } else {
        Cow::Borrowed(phase)
    }
}

/// 实际流通道 badge：只显示已收到的 Answer/Reasoning/Tool，不映射隐藏推理或预测状态。
pub(crate) fn stream_channel_badge(channel: LiveChannel) -> (&'static str, Role) {
    match channel {
        LiveChannel::Answer => ("[ANSWER]", Role::Primary),
        LiveChannel::Reasoning => ("[THINK]", Role::Muted),
        LiveChannel::Tool => ("[TOOL]", Role::Info),
    }
}

/// 输入框状态 chrome：反馈 Enter 当前语义与已存在的检查快捷键，不改变输入状态机。
pub(crate) struct InputChromeArgs {
    pub(crate) busy: bool,
    pub(crate) queued: usize,
    pub(crate) width: u16,
    pub(crate) reasoning_expanded: bool,
    pub(crate) has_reasoning: bool,
    pub(crate) has_tools: bool,
    pub(crate) has_history: bool,
    pub(crate) has_scrollable_tool_details: bool,
    pub(crate) has_live_output: bool,
    pub(crate) live_inspecting: bool,
}

pub(crate) fn input_chrome(args: InputChromeArgs) -> (String, Role) {
    let InputChromeArgs {
        busy,
        queued,
        width,
        reasoning_expanded,
        has_reasoning,
        has_tools,
        has_history,
        has_scrollable_tool_details,
        has_live_output,
        live_inspecting,
    } = args;
    let reasoning_hint = if !has_reasoning {
        None
    } else if reasoning_expanded {
        Some("Ctrl+R collapse")
    } else {
        Some("Ctrl+R reasoning")
    };
    let reasoning_suffix = reasoning_hint
        .map(|hint| format!(" · {hint}"))
        .unwrap_or_default();
    let focus_hint = if has_tools {
        " · Alt+↑/↓ focus"
    } else {
        ""
    };
    let toggle_hint = if has_tools {
        "Ctrl+O details"
    } else if has_history {
        "Ctrl+O history"
    } else {
        ""
    };
    let toggle_separator = if toggle_hint.is_empty() { "" } else { " · " };
    let scroll_hint = if has_scrollable_tool_details {
        " · Alt+PgUp/PgDn scroll"
    } else {
        ""
    };
    let live_hint = if !has_live_output || has_scrollable_tool_details {
        ""
    } else if live_inspecting {
        " · Alt+End follow"
    } else {
        " · Alt+PgUp inspect"
    };
    let text = match (busy, width) {
        (true, width) if width >= 96 && has_tools => format!(
            " Queue [{queued}] · Enter enqueue · Ctrl+C cancel{reasoning_suffix}{focus_hint}{toggle_separator}{toggle_hint}{scroll_hint}{live_hint}"
        ),
        (true, width) if width >= 72 && has_tools => {
            // 工具运行时压缩动作词，但同时保留 focus/details 与真实 reasoning 入口。
            format!(
                " Queue [{queued}] · Enter · Ctrl+C · Alt+↑/↓ focus · {toggle_hint}{scroll_hint}{live_hint}{reasoning_suffix}"
            )
        }
        (true, width) if width >= 72 => format!(
            " Queue [{queued}] · Enter enqueue · Ctrl+C cancel{reasoning_suffix}{toggle_separator}{toggle_hint}{live_hint}"
        ),
        (true, width) if width >= 56 && has_tools => {
            format!(
                " Q:[{queued}] · Alt+↑/↓ focus · Ctrl+O details{scroll_hint}{live_hint}{reasoning_suffix} "
            )
        }
        (true, width) if width >= 56 => {
                format!(" Queue [{queued}] · Enter enqueue · Ctrl+C cancel{reasoning_suffix}{live_hint} ")
        }
        (true, width) if width >= 18 && (has_tools || has_history) => {
            let reasoning = if has_reasoning { " ^R" } else { "" };
            format!(" Q:[{queued}] ^O{reasoning} ")
        }
        (true, width) if width >= 18 && has_reasoning => {
            format!(" Q:[{queued}] · {} ", reasoning_hint.unwrap_or("Ctrl+R reasoning"))
        }
        (true, width) if width >= 14 && (has_tools || has_history) => {
            format!(" Q:[{queued}] ^O ")
        }
        (true, width) if width >= 14 && has_reasoning => format!(" Q:[{queued}] · ^R "),
        (true, _) => format!(" Q:[{queued}] "),
        (false, width) if width >= 88 => format!(
            " Input (Enter send · Shift/Alt+Enter newline · Tab complete{focus_hint}{toggle_separator}{toggle_hint}{scroll_hint}{live_hint}{reasoning_suffix}) "
        ),
        (false, width) if width >= 56 => {
            format!(" Input{reasoning_suffix}{focus_hint}{toggle_separator}{toggle_hint}{scroll_hint}{live_hint} ")
        }
        (false, width) if width >= 18 && (has_tools || has_history) => {
            let reasoning = if has_reasoning { " ^R" } else { "" };
            format!(" Input ^O{reasoning} ")
        }
        (false, width) if width >= 18 && has_reasoning => {
            format!(" Input · {} ", reasoning_hint.unwrap_or("Ctrl+R reasoning"))
        }
        (false, width) if width >= 14 && (has_tools || has_history) => " In ^O ".to_owned(),
        (false, width) if width >= 14 && has_reasoning => " In · ^R ".to_owned(),
        (false, _) => " Input ".to_owned(),
    };
    (
        clip_display_cells(&text, width.saturating_sub(2)),
        if busy { Role::Warn } else { Role::Primary },
    )
}

/// 自定义底栏占位替换用变量(需求 3)。
pub(crate) struct StatusVars {
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) ctx: String,
    pub(crate) tokens: String,
    pub(crate) cwd: String,
}

/// 底栏模板渲染(需求 3,纯函数):替换 `{provider}{model}{ctx}{tokens}{cwd}`,
/// 未知占位原样保留(不吞字符,便于用户排错)。
pub(crate) fn render_status_template(tmpl: &str, v: &StatusVars) -> String {
    tmpl.replace("{provider}", &v.provider)
        .replace("{model}", &v.model)
        .replace("{ctx}", &v.ctx)
        .replace("{tokens}", &v.tokens)
        .replace("{cwd}", &v.cwd)
}

/// 当前工作目录末段名(状态栏用),取不到 → 空串。
pub(crate) fn cwd_name() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|x| x.to_string_lossy().to_string()))
        .unwrap_or_default()
}

/// 一帧的实时体征(iter-31):由主环据 `Instant`/token 计量算好后传入 `draw`,
/// draw 只消费数值 —— 计时逻辑不入 draw,便于纯测各格式化函数。
pub(crate) struct Vitals {
    pub(crate) step: usize,
    pub(crate) elapsed_s: u64,
    pub(crate) task_tokens: usize,
    pub(crate) rate: u64,
    /// 当前 history 估算 token(ctx% 分子)。
    pub(crate) ctx_used: usize,
    /// 待跑排队条数(iter-33),忙碌粘条显 ⏳N。
    pub(crate) queued: usize,
}
