use std::borrow::Cow;
use std::sync::OnceLock;

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
        // At the smallest usable width the top chrome may be collapsed to
        // preserve an editable draft. Keep queue depth, takeover, and the
        // freshest observed channel in the input title instead of losing the
        // Answer/Reasoning/Tool signal with right-side clipping.
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
        return text;
    }
    let budget = width.saturating_sub(2);
    let mut text = format!(" Q:[{}]", compact_count(&queued.to_string()));
    let enqueue = if width >= 24 { "↵ queue" } else { "↵" };
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
        if str_cells(&text) + str_cells(token) > budget as usize {
            break;
        }
        text.push_str(token);
    }
    format!("{text} ")
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
    if width >= 72 && (has_tools || has_reasoning) {
        text.push_str(" · Space toggle");
    }
    if width >= 80 && (has_tools || has_reasoning || has_live_answer) {
        let focus = if width >= 88 {
            " · Alt+←/→ focus"
        } else {
            " · Alt<> focus"
        };
        text.push_str(focus);
    } else if width >= 72 && (has_tools || has_reasoning || has_live_answer) {
        // Keep the action discoverable at the exact boundary where the full
        // label would clip the HOLD/follow/takeover priority rail.
        text.push_str(" · ←→");
    }
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
    text
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
    for token in [
        Some(" ↵"),
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

pub(crate) fn input_chrome(args: InputChromeArgs) -> (String, Role) {
    let InputChromeArgs {
        busy,
        queued,
        width,
        reasoning_expanded,
        has_reasoning,
        has_reasoning_history,
        has_live_answer,
        has_answer_history,
        has_live_history,
        has_tools,
        has_history,
        has_scrollable_tool_details,
        has_live_output,
        live_inspecting,
    } = args;
    if busy && live_inspecting && has_live_output && width >= 48 {
        let text = busy_live_inspection_actions(
            queued,
            width,
            has_tools,
            has_reasoning,
            has_live_answer,
            has_live_history,
        );
        return (
            clip_display_cells(&text, width.saturating_sub(2)),
            Role::Primary,
        );
    }
    let multiline_hint = multiline_shortcut_hint();
    let reasoning_hint = if has_reasoning {
        if reasoning_expanded {
            Some("Ctrl+R collapse")
        } else {
            Some("Ctrl+R reasoning")
        }
    } else if has_reasoning_history {
        Some("Ctrl+R history")
    } else {
        None
    };
    let reasoning_suffix = reasoning_hint
        .map(|hint| format!(" · {hint}"))
        .unwrap_or_default();
    let answer_suffix = if has_live_answer {
        " · Ctrl+A focus"
    } else if has_answer_history {
        " · Ctrl+A answers"
    } else {
        ""
    };
    let answer_prefix = if has_live_answer {
        "Ctrl+A focus · "
    } else if has_answer_history {
        "Ctrl+A answers · "
    } else {
        ""
    };
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
    let live_hint = if !has_live_output {
        ""
    } else if live_inspecting {
        if has_tools || has_reasoning {
            " · HOLD Ctrl+Space/Alt+End follow · Alt+←/→ focus · Space toggle"
        } else {
            " · HOLD Ctrl+Space/Alt+End follow"
        }
    } else {
        " · Ctrl+Space hold · PgUp/PgDn page"
    };
    let inspect_hint = if has_live_history {
        " · Ctrl+I inspect"
    } else {
        ""
    };
    let inspect_compact = if has_live_history { " ^I" } else { "" };
    // Put the live viewport control first on the wide input chrome. The
    // tail is clipped on ordinary terminals; intervention must remain visible.
    let wide_live_prefix = live_hint
        .strip_prefix(" · ")
        .map(|hint| format!("{hint} · "))
        .unwrap_or_default();
    let wide_live_prefix = if has_live_history {
        format!("Ctrl+I inspect · {wide_live_prefix}")
    } else {
        wide_live_prefix
    };
    let text = match (busy, width) {
        (true, width) if width >= 96 && has_tools => format!(
                " Queue [{queued}]{reasoning_suffix}{answer_suffix}{toggle_separator}{toggle_hint}{focus_hint}{inspect_hint} · Ctrl+T activity · Enter queue · Ctrl+Enter front · Ctrl+C takeover{scroll_hint}{live_hint}"
        ),
        (true, width) if width >= 72 && has_tools => {
            // 工具运行时优先保留不打断的前插、接管、详情、思考与焦点动作。
            // 96 列以下采用与窄栏一致的键位缩写，避免把 Alt+↑/↓ 裁成半个动作。
            let reasoning = if has_reasoning {
                if reasoning_expanded {
                    " · ^R collapse"
                } else {
                    " · ^R"
                }
            } else {
                ""
            };
            let enqueue = if width >= 88 {
                " · Enter queue"
            } else {
                " · ↵ queue"
            };
            let front = if width >= 88 { "^Enter front" } else { "^Enter" };
            let details = if width >= 80 { "^O details" } else { "^O" };
            format!(
                " Queue [{queued}]{enqueue} · {front} · ^C takeover{reasoning} · {details} · Alt+↑/↓{inspect_hint}"
            )
        }
        (true, width) if width >= 72 => format!(
            " Queue [{queued}] · Ctrl+Enter front · Ctrl+C takeover · Enter{reasoning_suffix}{answer_suffix}{toggle_separator}{toggle_hint}{inspect_hint}{live_hint} · Ctrl+T activity"
        ),
        (true, width) if width >= 56 && has_tools => {
            let reasoning = if has_reasoning { " · ^R" } else { "" };
            let answer = if has_live_answer { " · ^A" } else { "" };
            let (queue_separator, front, takeover) = if width >= 64 {
                ("", "^Enter front", "^C takeover")
            } else {
                (" · ", "^Enter", "^C")
            };
            format!(
                " Q:[{queued}]{queue_separator}↵ queue · {front} · {takeover} · ^O details{reasoning}{answer}{inspect_compact} "
            )
        }
        (true, width) if width >= 56 => {
                format!(" Queue [{queued}] · Enter queue · Ctrl+Enter front · Ctrl+C takeover{reasoning_suffix}{answer_suffix}{inspect_hint}{live_hint} ")
        }
        (true, width) => compact_busy_actions(
            queued,
            width,
            has_tools || has_history,
            has_reasoning,
            has_live_answer,
            has_live_history,
            live_inspecting,
        ),
        (false, width) if width >= 88 => {
            let full = format!(
                " Input ({answer_prefix}{wide_live_prefix}Enter send · {multiline_hint} · Tab complete{focus_hint}{toggle_separator}{toggle_hint}{scroll_hint}{reasoning_suffix} · Ctrl+T activity) "
            );
            if str_cells(&full) <= width.saturating_sub(2) as usize {
                full
            } else {
                // Keep the reasoning/archive and activity actions whole when
                // the full Windows keyboard legend would clip the last label.
                format!(
                    " Input ({answer_prefix}{wide_live_prefix}Enter · Ctrl+J newline · Tab{focus_hint}{toggle_separator}{toggle_hint}{scroll_hint}{reasoning_suffix} · Ctrl+T activity) "
                )
            }
        }
        (false, width) if width >= 56 => format!(
            " Input · Enter send{reasoning_suffix}{answer_suffix}{focus_hint}{toggle_separator}{toggle_hint}{inspect_hint}{scroll_hint}{live_hint} "
        ),
        (false, width)
            if width >= 18
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
        (false, width) if width >= 18 && has_reasoning => {
            let text = if width < 32 {
                " In ↵ Ctrl+R ".to_owned()
            } else {
                format!(
                    " Input · ↵ · {} ",
                    reasoning_hint.unwrap_or("Ctrl+R reasoning")
                )
            };
            clip_display_cells(&text, width.saturating_sub(2))
        }
        (false, width)
            if width >= 14
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
        (false, width) if width >= 14 && has_reasoning => {
            compact_idle_history_actions(width, false, true, false, has_live_history)
        }
        (false, _) => " Input ".to_owned(),
    };
    (
        clip_display_cells(&text, width.saturating_sub(2)),
        // Busy is an active mode, not a warning; keep the single cyan focus
        // accent available for motion and current interaction.
        Role::Primary,
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
