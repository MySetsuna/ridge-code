use std::borrow::Cow;
use std::sync::OnceLock;

use agent::Todo;
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span, Text},
};

use super::{
    clip_display_cells, role_color, sanitize_display_text, str_cells, todo_progress, LiveChannel,
    Role,
};

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
pub(crate) const DEFAULT_STATUS_BAR: &str = " {provider} · {model} · ctx {ctx} · {tokens} ";

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
    format!("THK[{step}t+{elapsed_s}s · {tokens} task tok] ")
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
#[cfg(test)]
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

/// 顶部活动轨文案：只承载阶段、活动、工具意图、时长、速率、todo 与队列。
/// 输入/输出 token、ctx 与 effort 专属底部遥测，避免两条状态条互相复制。
pub(crate) fn fmt_busy_signal(
    phase: &str,
    todos: &[Todo],
    elapsed_s: u64,
    rate: u64,
    queued: usize,
    pending_call: Option<&provider::ToolCall>,
) -> String {
    let tool = pending_call.map(fmt_tool_intent).unwrap_or_default();
    let phase = inline_display(phase).trim().to_owned();
    let mut s = format!("⚡ {phase}{tool} · t+{elapsed_s}s");
    if rate > 0 {
        s.push_str(&format!(" · {rate}/s"));
    }
    if let Some((d, n)) = todo_progress(todos) {
        s.push_str(&format!(" · todo {d}/{n}"));
    }
    if queued > 0 {
        s.push_str(&format!(" · ⏳{queued}"));
    }
    s
}

/// Expose only deterministic counters already produced by AgentState.  This
/// is a diagnostic projection, not a soft classifier or a second stop rule.
pub(crate) fn fmt_progress_diagnostic(
    stall: usize,
    err_streak: usize,
    explore_streak: usize,
) -> Option<String> {
    let mut signals = Vec::new();
    if explore_streak > 0 {
        signals.push(format!("inspect {explore_streak}/{}", agent::MAX_EXPLORE));
    }
    if stall > 0 {
        signals.push(format!("same {stall}/{}", agent::MAX_STALL));
    }
    if err_streak > 0 {
        signals.push(format!("errors {err_streak}/{}", agent::MAX_ERR_STREAK));
    }
    (!signals.is_empty()).then(|| signals.join(" · "))
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
        LiveChannel::Reasoning => ("[THINK]", Role::Reasoning),
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
    pub(crate) has_reasoning_history: bool,
    pub(crate) has_live_answer: bool,
    pub(crate) has_answer_history: bool,
    pub(crate) has_live_history: bool,
    pub(crate) has_tools: bool,
    pub(crate) has_history: bool,
    pub(crate) has_scrollable_tool_details: bool,
    pub(crate) has_live_output: bool,
    pub(crate) live_inspecting: bool,
}

/// Windows' legacy console cannot distinguish Shift+Enter from Enter. Keep
/// the input affordance truthful: precise Shift+Enter is advertised only when
/// the same opt-in KKP path used by `TerminalGuard` is active.
pub(crate) fn multiline_shortcut_label(precise: bool) -> &'static str {
    if precise {
        "Shift/Alt+Enter newline"
    } else {
        "Alt+Enter/Ctrl+J newline"
    }
}

/// TerminalGuard records the result of the best-effort KKP command once the
/// terminal is actually entered. Tests and non-TUI callers retain the
/// environment-based fallback until that runtime capability is known.
static PRECISE_MULTILINE_INPUT: OnceLock<bool> = OnceLock::new();

pub(crate) fn set_precise_multiline_input_enabled(enabled: bool) {
    let _ = PRECISE_MULTILINE_INPUT.set(enabled);
}

fn precise_multiline_input_enabled() -> bool {
    PRECISE_MULTILINE_INPUT.get().copied().unwrap_or_else(|| {
        !cfg!(windows) || std::env::var("RIDGE_TUI_KITTY").ok().as_deref() == Some("1")
    })
}

fn multiline_shortcut_hint() -> &'static str {
    multiline_shortcut_label(precise_multiline_input_enabled())
}

/// Busy narrow chrome uses a packed action rail instead of dropping the
/// takeover affordance altogether.  Priority is deliberate: queue depth and
/// Ctrl-C remain first; tool detail, reasoning inspection, and live Inspector
/// follow when their two-cell tokens fit.
fn compact_busy_actions(
    queued: usize,
    width: u16,
    has_tools: bool,
    has_reasoning: bool,
    has_live_answer: bool,
    has_live_history: bool,
    live_inspecting: bool,
) -> String {
    if width <= 8 {
        return narrow_busy_actions(width, has_tools, has_reasoning, has_live_answer);
    }
    let budget = width.saturating_sub(2);
    let mut text = format!(" Q:[{}]", compact_count(&queued.to_string()));
    append_busy_action_tokens(
        &mut text,
        BusyActionTokens {
            budget: budget as usize,
            long_enqueue: width >= 24,
            has_tools,
            has_reasoning,
            has_live_answer,
            has_live_history,
            live_inspecting,
        },
    );
    format!("{text} ")
}

fn narrow_busy_actions(
    width: u16,
    has_tools: bool,
    has_reasoning: bool,
    has_live_answer: bool,
) -> String {
    let channel = if has_live_answer {
        Some(" A ")
    } else if has_reasoning {
        Some(" R ")
    } else if has_tools {
        Some(" T ")
    } else {
        None
    };
    let budget = width.saturating_sub(2) as usize;
    let mut text = "Q".to_owned();
    for token in [Some(" C"), channel].into_iter().flatten() {
        if str_cells(&text) + str_cells(token) > budget {
            break;
        }
        text.push_str(token);
    }
    text
}

struct BusyActionTokens {
    budget: usize,
    long_enqueue: bool,
    has_tools: bool,
    has_reasoning: bool,
    has_live_answer: bool,
    has_live_history: bool,
    live_inspecting: bool,
}

fn append_busy_action_tokens(text: &mut String, options: BusyActionTokens) {
    let BusyActionTokens {
        budget,
        long_enqueue,
        has_tools,
        has_reasoning,
        has_live_answer,
        has_live_history,
        live_inspecting,
    } = options;
    let enqueue = if long_enqueue { "↵ queue" } else { "↵" };
    for token in [
        Some(enqueue),
        Some("^C"),
        has_live_answer.then_some("^A"),
        live_inspecting.then_some("^Space"),
        has_tools.then_some("^O"),
        has_reasoning.then_some("^R"),
        has_live_history.then_some("^I"),
    ]
    .into_iter()
    .flatten()
    {
        if str_cells(text) + str_cells(token) > budget {
            break;
        }
        text.push_str(token);
    }
}

/// When the user is auditing live output, put the return-to-follow action at
/// the left edge. The ordinary busy rail is clipped from the right, so a
/// trailing `Alt+End follow` hint disappears exactly when it is needed.
fn busy_live_inspection_actions(
    queued: usize,
    width: u16,
    has_tools: bool,
    has_reasoning: bool,
    has_live_answer: bool,
    has_live_history: bool,
) -> String {
    let takeover = if width >= 72 {
        "Esc/^C takeover"
    } else {
        "^C takeover"
    };
    let mut text = format!(
        " HOLD · Alt+End follow · {takeover} · Q:[{}]",
        compact_count(&queued.to_string())
    );
    append_inspection_actions(
        &mut text,
        width,
        has_tools,
        has_reasoning,
        has_live_answer,
        has_live_history,
    );
    text
}

fn append_inspection_actions(
    text: &mut String,
    width: u16,
    has_tools: bool,
    has_reasoning: bool,
    has_live_answer: bool,
    has_live_history: bool,
) {
    let has_focus = has_tools || has_reasoning || has_live_answer;
    if width >= 72 && (has_tools || has_reasoning) {
        text.push_str(" · Space toggle");
    }
    if width >= 80 && has_focus {
        text.push_str(if width >= 88 {
            " · Alt+←/→ focus"
        } else {
            " · Alt<> focus"
        });
    } else if width >= 72 && has_focus {
        text.push_str(" · ←→");
    }
    append_inspection_shortcuts(
        text,
        width,
        has_tools,
        has_reasoning,
        has_live_answer,
        has_live_history,
    );
}

fn append_inspection_shortcuts(
    text: &mut String,
    width: u16,
    has_tools: bool,
    has_reasoning: bool,
    has_live_answer: bool,
    has_live_history: bool,
) {
    if width >= 64 {
        text.push_str(" · ^Enter front");
    }
    if width >= 80 && has_tools {
        text.push_str(" · ^O details");
    }
    if width >= 80 && has_live_answer {
        text.push_str(" · ^A answer");
    }
    if width >= 88 && has_reasoning {
        text.push_str(" · ^R");
    }
    if width >= 96 && has_live_history {
        text.push_str(" · ^I");
    }
    if width >= 104 {
        text.push_str(" · Ctrl+T activity");
    }
}

/// Idle narrow chrome keeps submit plus archive entry points discoverable even
/// when the session has no live tool/reasoning block. Answer and reasoning
/// history are the primary audit surfaces; tool/live inspection follows when
/// cells remain.
fn compact_idle_history_actions(
    width: u16,
    has_answer_history: bool,
    has_reasoning_history: bool,
    has_tools: bool,
    has_live_history: bool,
) -> String {
    let budget = width.saturating_sub(2) as usize;
    // At tiny widths, save two cells for the submit and audit tokens rather
    // than letting the first label crowd out the only actionable shortcuts.
    let mut text = if width < 24 {
        " In".to_owned()
    } else {
        " Input".to_owned()
    };
    let tab_hint = if width >= 40 { " Tab complete" } else { "⇥" };
    for token in [
        Some(" ↵"),
        Some(tab_hint),
        has_answer_history.then_some(" ^A"),
        has_reasoning_history.then_some(" ^R"),
        has_tools.then_some(" ^O"),
        has_live_history.then_some(" ^I"),
    ]
    .into_iter()
    .flatten()
    {
        if str_cells(&text) + str_cells(token) > budget {
            break;
        }
        text.push_str(token);
    }
    format!("{text} ")
}

/// Idle input still needs a truthful submit/completion rail when no archive
/// exists to occupy the title.  Keep the full labels for roomier frames and
/// reserve compact glyphs for the same two actions on narrow terminals.
fn compact_idle_input_actions(width: u16) -> String {
    let text = if width >= 40 {
        " Input · Enter send · Tab complete "
    } else if width >= 24 {
        " Input · ↵ send · ⇥ "
    } else {
        " In ↵ ⇥ "
    };
    clip_display_cells(text, width.saturating_sub(2))
}

struct InputChromeHints {
    multiline: &'static str,
    reasoning: Option<&'static str>,
    reasoning_suffix: String,
    answer_suffix: &'static str,
    answer_prefix: &'static str,
    focus: &'static str,
    toggle: &'static str,
    toggle_separator: &'static str,
    scroll: &'static str,
    live: &'static str,
    inspect: &'static str,
    inspect_compact: &'static str,
    wide_live_prefix: String,
}

impl InputChromeHints {
    fn from_args(args: &InputChromeArgs) -> Self {
        let reasoning = reasoning_hint(
            args.has_reasoning,
            args.reasoning_expanded,
            args.has_reasoning_history,
        );
        let reasoning_suffix = reasoning
            .map(|hint| format!(" · {hint}"))
            .unwrap_or_default();
        let answer_suffix = answer_suffix(args.has_live_answer, args.has_answer_history);
        let answer_prefix = answer_prefix(args.has_live_answer, args.has_answer_history);
        let focus = if args.has_tools {
            " · Alt+↑/↓ focus"
        } else {
            ""
        };
        let toggle = toggle_hint(args.has_tools, args.has_history);
        let toggle_separator = if toggle.is_empty() { "" } else { " · " };
        let scroll = if args.has_scrollable_tool_details {
            " · Alt+PgUp/PgDn scroll"
        } else {
            ""
        };
        let live = live_hint(
            args.has_live_output,
            args.live_inspecting,
            args.has_tools,
            args.has_reasoning,
        );
        let inspect = if args.has_live_history {
            " · Ctrl+I inspect"
        } else {
            ""
        };
        let inspect_compact = if args.has_live_history { " ^I" } else { "" };
        let wide_live_prefix = wide_live_prefix(live, args.has_live_history);
        Self {
            multiline: multiline_shortcut_hint(),
            reasoning,
            reasoning_suffix,
            answer_suffix,
            answer_prefix,
            focus,
            toggle,
            toggle_separator,
            scroll,
            live,
            inspect,
            inspect_compact,
            wide_live_prefix,
        }
    }
}

fn reasoning_hint(has_reasoning: bool, expanded: bool, has_history: bool) -> Option<&'static str> {
    if has_reasoning {
        Some(if expanded {
            "Ctrl+R collapse"
        } else {
            "Ctrl+R reasoning"
        })
    } else if has_history {
        Some("Ctrl+R history")
    } else {
        None
    }
}

fn answer_suffix(has_live: bool, has_history: bool) -> &'static str {
    if has_live {
        " · Ctrl+A focus"
    } else if has_history {
        " · Ctrl+A answers"
    } else {
        ""
    }
}

fn answer_prefix(has_live: bool, has_history: bool) -> &'static str {
    if has_live {
        "Ctrl+A focus · "
    } else if has_history {
        "Ctrl+A answers · "
    } else {
        ""
    }
}

fn toggle_hint(has_tools: bool, has_history: bool) -> &'static str {
    if has_tools {
        "Ctrl+O details"
    } else if has_history {
        "Ctrl+O history"
    } else {
        ""
    }
}

fn live_hint(
    has_output: bool,
    inspecting: bool,
    has_tools: bool,
    has_reasoning: bool,
) -> &'static str {
    if !has_output {
        ""
    } else if inspecting && (has_tools || has_reasoning) {
        " · HOLD Ctrl+Space/Alt+End follow · Alt+←/→ focus · Space toggle"
    } else if inspecting {
        " · HOLD Ctrl+Space/Alt+End follow"
    } else {
        " · Ctrl+Space hold · PgUp/PgDn page"
    }
}

fn wide_live_prefix(live: &str, has_history: bool) -> String {
    let prefix = live
        .strip_prefix(" · ")
        .map(|hint| format!("{hint} · "))
        .unwrap_or_default();
    if has_history {
        format!("Ctrl+I inspect · {prefix}")
    } else {
        prefix
    }
}

pub(crate) fn input_chrome(args: InputChromeArgs) -> (String, Role) {
    if args.busy && args.live_inspecting && args.has_live_output && args.width >= 48 {
        let text = busy_live_inspection_actions(
            args.queued,
            args.width,
            args.has_tools,
            args.has_reasoning,
            args.has_live_answer,
            args.has_live_history,
        );
        return (
            clip_display_cells(&text, args.width.saturating_sub(2)),
            Role::Primary,
        );
    }
    let hints = InputChromeHints::from_args(&args);
    let text = input_chrome_text(&args, &hints);
    (
        clip_display_cells(&text, args.width.saturating_sub(2)),
        // Busy is an active mode, not a warning; keep the single cyan focus
        // accent available for motion and current interaction.
        Role::Primary,
    )
}

fn input_chrome_text(args: &InputChromeArgs, hints: &InputChromeHints) -> String {
    if args.busy {
        busy_input_chrome_text(args, hints)
    } else {
        idle_input_chrome_text(args, hints)
    }
}

fn busy_input_chrome_text(args: &InputChromeArgs, hints: &InputChromeHints) -> String {
    let InputChromeArgs {
        queued,
        width,
        reasoning_expanded,
        has_reasoning,
        has_tools,
        has_live_answer,
        live_inspecting,
        ..
    } = *args;
    match (width, has_tools) {
        (width, true) if width >= 96 => busy_wide_tools_chrome(queued, hints),
        (width, true) if width >= 72 => {
            busy_tools_chrome(queued, width, has_reasoning, reasoning_expanded, hints)
        }
        (width, _) if width >= 72 => busy_wide_busy_chrome(queued, hints),
        (width, true) if width >= 56 => {
            busy_compact_tools_chrome(queued, width, has_reasoning, has_live_answer, hints)
        }
        (width, _) if width >= 56 => busy_compact_chrome(queued, hints),
        (width, _) => compact_busy_actions(
            queued,
            width,
            has_tools || args.has_history,
            has_reasoning,
            has_live_answer,
            args.has_live_history,
            live_inspecting,
        ),
    }
}

fn busy_wide_tools_chrome(queued: usize, hints: &InputChromeHints) -> String {
    format!(
        " Queue [{queued}]{reasoning_suffix}{answer_suffix}{toggle_separator}{toggle_hint}{focus_hint}{inspect_hint} · Ctrl+T activity · Enter queue · Ctrl+Enter front · Ctrl+C takeover{scroll_hint}{live_hint} · Ctrl+Shift+Enter steer",
        reasoning_suffix = hints.reasoning_suffix,
        answer_suffix = hints.answer_suffix,
        toggle_separator = hints.toggle_separator,
        toggle_hint = hints.toggle,
        focus_hint = hints.focus,
        inspect_hint = hints.inspect,
        scroll_hint = hints.scroll,
        live_hint = hints.live,
    )
}

fn busy_tools_chrome(
    queued: usize,
    width: u16,
    has_reasoning: bool,
    reasoning_expanded: bool,
    hints: &InputChromeHints,
) -> String {
    let reasoning = busy_reasoning_hint(has_reasoning, reasoning_expanded);
    let enqueue = if width >= 88 {
        " · Enter queue"
    } else {
        " · ↵ queue"
    };
    let front = if width >= 88 {
        "^Enter front"
    } else {
        "^Enter"
    };
    let details = if width >= 80 { "^O details" } else { "^O" };
    format!(
        " Queue [{queued}]{enqueue} · {front} · ^C takeover{reasoning} · {details} · Alt+↑/↓{}",
        hints.inspect
    )
}

fn busy_wide_busy_chrome(queued: usize, hints: &InputChromeHints) -> String {
    format!(
        " Queue [{queued}] · Ctrl+Enter front · Ctrl+C takeover · Enter{reasoning_suffix}{answer_suffix}{toggle_separator}{toggle}{inspect}{live} · Ctrl+T activity · Ctrl+Shift+Enter steer",
        reasoning_suffix = hints.reasoning_suffix,
        answer_suffix = hints.answer_suffix,
        toggle_separator = hints.toggle_separator,
        toggle = hints.toggle,
        inspect = hints.inspect,
        live = hints.live,
    )
}

fn busy_compact_tools_chrome(
    queued: usize,
    width: u16,
    has_reasoning: bool,
    has_live_answer: bool,
    hints: &InputChromeHints,
) -> String {
    let reasoning = if has_reasoning { " · ^R" } else { "" };
    let answer = if has_live_answer { " · ^A" } else { "" };
    let (queue_separator, front, takeover) = if width >= 64 {
        ("", "^Enter front", "^C takeover")
    } else {
        (" · ", "^Enter", "^C")
    };
    format!(
        " Q:[{queued}]{queue_separator}↵ queue · {front} · {takeover} · ^O details{reasoning}{answer}{} ",
        hints.inspect_compact
    )
}

fn busy_compact_chrome(queued: usize, hints: &InputChromeHints) -> String {
    format!(
        " Queue [{queued}] · Enter queue · Ctrl+Enter front · Ctrl+C takeover{reasoning_suffix}{answer_suffix}{inspect}{live} · Ctrl+Shift+Enter steer ",
        reasoning_suffix = hints.reasoning_suffix,
        answer_suffix = hints.answer_suffix,
        inspect = hints.inspect,
        live = hints.live,
    )
}

fn busy_reasoning_hint(has_reasoning: bool, expanded: bool) -> &'static str {
    if !has_reasoning {
        ""
    } else if expanded {
        " · ^R collapse"
    } else {
        " · ^R"
    }
}

fn idle_input_chrome_text(args: &InputChromeArgs, hints: &InputChromeHints) -> String {
    let InputChromeArgs {
        width,
        reasoning_expanded: _,
        has_reasoning,
        has_reasoning_history,
        has_tools,
        has_history,
        has_answer_history,
        has_live_history,
        ..
    } = *args;
    match width {
        width if width >= 88 => wide_idle_input_chrome(args, hints),
        width if width >= 56 => format!(
            " Input · Enter send · Tab complete{reasoning_suffix}{answer_suffix}{focus}{toggle_separator}{toggle}{inspect}{scroll}{live} ",
            reasoning_suffix = hints.reasoning_suffix,
            answer_suffix = hints.answer_suffix,
            focus = hints.focus,
            toggle_separator = hints.toggle_separator,
            toggle = hints.toggle,
            inspect = hints.inspect,
            scroll = hints.scroll,
            live = hints.live,
        ),
        width if width >= 18
            && (has_tools
                || has_history
                || has_reasoning_history
                || has_answer_history
                || has_live_history) => compact_idle_history_actions(
            width,
            has_answer_history,
            has_reasoning || has_reasoning_history,
            has_tools || has_history,
            has_live_history,
        ),
        width if width >= 18 && has_reasoning => {
            let text = if width < 32 {
                " In ↵ Ctrl+R ".to_owned()
            } else {
                format!(
                    " Input · ↵ · {} ",
                    hints.reasoning.unwrap_or("Ctrl+R reasoning")
                )
            };
            clip_display_cells(&text, width.saturating_sub(2))
        }
        width if width >= 10
            && (has_tools
                || has_history
                || has_reasoning_history
                || has_answer_history
                || has_live_history) => compact_idle_history_actions(
            width,
            has_answer_history,
            has_reasoning || has_reasoning_history,
            has_tools || has_history,
            has_live_history,
        ),
        width if width >= 14 && has_reasoning => {
            compact_idle_history_actions(width, false, true, false, has_live_history)
        }
        width if width >= 10 => compact_idle_input_actions(width),
        _ => " Input ".to_owned(),
    }
}

fn wide_idle_input_chrome(args: &InputChromeArgs, hints: &InputChromeHints) -> String {
    let full = format!(
        " Input ({answer_prefix}{wide_live_prefix}Enter send · {multiline} · Tab complete{focus}{toggle_separator}{toggle}{scroll}{reasoning_suffix} · Ctrl+T activity) ",
        answer_prefix = hints.answer_prefix,
        wide_live_prefix = hints.wide_live_prefix,
        multiline = hints.multiline,
        focus = hints.focus,
        toggle_separator = hints.toggle_separator,
        toggle = hints.toggle,
        scroll = hints.scroll,
        reasoning_suffix = hints.reasoning_suffix,
    );
    if str_cells(&full) <= args.width.saturating_sub(2) as usize {
        full
    } else {
        format!(
            " Input ({answer_prefix}{wide_live_prefix}Enter · Ctrl+J newline · Tab{focus}{toggle_separator}{toggle}{scroll}{reasoning_suffix} · Ctrl+T activity) ",
            answer_prefix = hints.answer_prefix,
            wide_live_prefix = hints.wide_live_prefix,
            focus = hints.focus,
            toggle_separator = hints.toggle_separator,
            toggle = hints.toggle,
            scroll = hints.scroll,
            reasoning_suffix = hints.reasoning_suffix,
        )
    }
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

/// 窄终端底栏的固定优先级投影：上下文、输入/输出 token、effort 优先，
/// 让默认模板不会因折行吃掉 Answer 槽；宽屏仍完整尊重用户模板。
#[allow(clippy::too_many_arguments)]
pub(crate) fn compact_status_line(
    width: u16,
    provider: &str,
    model: &str,
    ctx: &str,
    total_tokens: usize,
    input_tokens: &str,
    output_tokens: &str,
    effort: &str,
) -> String {
    let input = compact_count(input_tokens);
    let output = compact_count(output_tokens);
    let total = compact_count(&total_tokens.to_string());
    let effort_full = inline_display(effort);
    let effort_short = compact_effort(effort);
    let ctx = inline_display(ctx);
    let provider = clip_display_cells(&inline_display(provider), 10);
    let model = clip_display_cells(&inline_display(model), 18);

    let line = match width {
        72.. => {
            format!("{provider}/{model} · C{ctx} · I{input} O{output} · E{effort_full} · T{total}")
        }
        48.. => format!("M:{model} · C{ctx} · I{input} O{output} · E{effort_full}"),
        32.. => format!("C{ctx} · I{input} O{output} · E{effort_short}"),
        20.. => format!("I{input} O{output} E{effort_short}"),
        _ => format!("E{effort_short} I{input} O{output}"),
    };
    clip_display_cells(&line, width)
}

/// 仅给紧凑遥测增加语义层级：标签弱化、数值保持清晰，文本与宽度
/// 仍由 `compact_status_line` 决定；非紧凑的用户自定义模板原样呈现。
pub(crate) fn status_line_projection(text: &str) -> Text<'static> {
    let segments = text.split(" · ").collect::<Vec<_>>();
    let compact = segments.len() >= 2
        && segments.iter().any(|segment| segment.starts_with('C'))
        && segments.iter().any(|segment| segment.starts_with('E'))
        && segments
            .iter()
            .any(|segment| segment.contains("I") || segment.contains("O"));
    if !compact {
        return Text::from(text.to_owned());
    }

    let mut spans = Vec::new();
    for (index, segment) in segments.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(
                " · ",
                Style::default()
                    .fg(role_color(Role::Label))
                    .add_modifier(Modifier::DIM),
            ));
        }
        for (token_index, token) in segment.split_whitespace().enumerate() {
            if token_index > 0 {
                spans.push(Span::raw(" "));
            }
            let mut chars = token.chars();
            let Some(label) = chars.next() else {
                continue;
            };
            if matches!(label, 'C' | 'I' | 'O' | 'E' | 'T' | 'M') {
                spans.push(Span::styled(
                    label.to_string(),
                    Style::default()
                        .fg(role_color(Role::Label))
                        .add_modifier(Modifier::DIM),
                ));
                spans.push(Span::styled(
                    chars.collect::<String>(),
                    Style::default().fg(role_color(Role::Metric)),
                ));
            } else {
                spans.push(Span::styled(
                    token.to_owned(),
                    Style::default().fg(role_color(Role::Metric)),
                ));
            }
        }
    }
    Text::from(Line::from(spans))
}

fn compact_count(value: &str) -> String {
    let value = value.trim();
    let approximate = value.starts_with('~');
    let digits = value.strip_prefix('~').unwrap_or(value);
    let Some(number) = digits.parse::<u64>().ok() else {
        return clip_display_cells(value, 8);
    };
    let compact = fmt_ctx(number);
    if approximate {
        format!("~{compact}")
    } else {
        compact
    }
}

fn compact_effort(effort: &str) -> String {
    match effort.trim().to_ascii_lowercase().as_str() {
        "default" => "def".to_owned(),
        "minimal" => "min".to_owned(),
        "low" => "lo".to_owned(),
        "medium" => "med".to_owned(),
        "high" => "hi".to_owned(),
        "xhigh" | "extra-high" | "extra high" => "xhi".to_owned(),
        other => clip_display_cells(other, 5),
    }
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
