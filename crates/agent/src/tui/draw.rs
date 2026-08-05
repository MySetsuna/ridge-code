use std::borrow::Cow;

use ratatui::buffer::Buffer;
use unicode_segmentation::UnicodeSegmentation;

use super::*;

/// Shared interactive surface grammar. Titles, roles, and dimensions remain
/// owned by each caller; only the stable frame language lives here.
fn rounded_surface_block() -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
}

fn snapshot_symbol(symbol: &str) -> String {
    symbol
        .chars()
        .map(|ch| if matches!(ch, '\r' | '\n') { ' ' } else { ch })
        .collect()
}

fn snapshot_color(color: Option<Color>) -> String {
    match color.unwrap_or(Color::Reset) {
        Color::Reset => "reset".to_string(),
        Color::Black => "black".to_string(),
        Color::Red => "red".to_string(),
        Color::Green => "green".to_string(),
        Color::Yellow => "yellow".to_string(),
        Color::Blue => "blue".to_string(),
        Color::Magenta => "magenta".to_string(),
        Color::Cyan => "cyan".to_string(),
        Color::Gray => "gray".to_string(),
        Color::DarkGray => "dark-gray".to_string(),
        Color::LightRed => "light-red".to_string(),
        Color::LightGreen => "light-green".to_string(),
        Color::LightYellow => "light-yellow".to_string(),
        Color::LightBlue => "light-blue".to_string(),
        Color::LightMagenta => "light-magenta".to_string(),
        Color::LightCyan => "light-cyan".to_string(),
        Color::White => "white".to_string(),
        Color::Indexed(value) => format!("indexed:{value}"),
        Color::Rgb(red, green, blue) => format!("#{red:02x}{green:02x}{blue:02x}"),
    }
}

fn snapshot_roles(style: Style) -> Vec<&'static str> {
    [
        ("primary", Role::Primary),
        ("command", Role::Command),
        ("reasoning", Role::Reasoning),
        ("info", Role::Info),
        ("success", Role::Success),
        ("error", Role::Error),
        ("warn", Role::Warn),
        ("border", Role::Border),
        ("muted", Role::Muted),
        ("diff-add", Role::DiffAdd),
        ("diff-del", Role::DiffDel),
    ]
    .into_iter()
    .filter_map(|(name, role)| (style.fg == Some(role_color(role))).then_some(name))
    .collect()
}

fn snapshot_modifiers(modifiers: Modifier) -> Vec<&'static str> {
    [
        ("bold", Modifier::BOLD),
        ("dim", Modifier::DIM),
        ("italic", Modifier::ITALIC),
        ("underlined", Modifier::UNDERLINED),
        ("slow-blink", Modifier::SLOW_BLINK),
        ("rapid-blink", Modifier::RAPID_BLINK),
        ("reversed", Modifier::REVERSED),
        ("hidden", Modifier::HIDDEN),
        ("crossed-out", Modifier::CROSSED_OUT),
    ]
    .into_iter()
    .filter_map(|(name, flag)| modifiers.contains(flag).then_some(name))
    .collect()
}

fn snapshot_style(style: Style) -> serde_json::Value {
    serde_json::json!({
        "fg": snapshot_color(style.fg),
        "bg": snapshot_color(style.bg),
        "roles": snapshot_roles(style),
        "modifiers": snapshot_modifiers(style.add_modifier),
    })
}

fn snapshot_styled_rows(buffer: &Buffer) -> Vec<Vec<serde_json::Value>> {
    let area = buffer.area();
    let width = area.width as usize;
    (0..area.height as usize)
        .map(|y| {
            let start = y.saturating_mul(width);
            let end = start.saturating_add(width).min(buffer.content().len());
            let mut runs = Vec::new();
            let mut run_start = 0usize;
            let mut run_text = String::new();
            let mut run_style = None;
            for (x, cell) in buffer.content()[start..end].iter().enumerate() {
                let style = cell.style();
                if run_style != Some(style) {
                    if let Some(previous) = run_style {
                        runs.push(serde_json::json!({
                            "x": run_start,
                            "cells": x.saturating_sub(run_start),
                            "text": run_text,
                            "style": snapshot_style(previous),
                        }));
                        run_text = String::new();
                    }
                    run_start = x;
                    run_style = Some(style);
                }
                run_text.push_str(&snapshot_symbol(cell.symbol()));
            }
            if let Some(style) = run_style {
                runs.push(serde_json::json!({
                    "x": run_start,
                    "cells": end.saturating_sub(start).saturating_sub(run_start),
                    "text": run_text,
                    "style": snapshot_style(style),
                }));
            }
            runs
        })
        .collect()
}

fn snapshot_payload(buffer: &Buffer, render_us: u128, ui: &Ui, vitals: &Vitals) -> String {
    let area = buffer.area();
    let width = area.width as usize;
    let mut rows = Vec::with_capacity(area.height as usize);
    for y in 0..area.height as usize {
        let mut row = String::with_capacity(width);
        let start = y.saturating_mul(width);
        let end = start.saturating_add(width).min(buffer.content().len());
        for cell in &buffer.content()[start..end] {
            row.push_str(&snapshot_symbol(cell.symbol()));
        }
        rows.push(row);
    }
    serde_json::json!({
        "format": "ridgecode-tui-frame",
        "version": 2,
        "rect": {
            "x": area.x,
            "y": area.y,
            "width": area.width,
            "height": area.height,
        },
        "render_us": render_us,
        "state": {
            "busy": ui.busy,
            "waiting": ui.waiting,
            "phase": ui.phase,
            "activity": ui.activity,
            "activity_kind": ui
                .activity_history
                .back()
                .map(|entry| entry.kind.tag())
                .unwrap_or("SYS"),
            "activity_sequence": ui
                .activity_history
                .back()
                .map(|entry| entry.sequence)
                .unwrap_or(0),
            "activity_history": ui
                .activity_history
                .iter()
                .map(|entry| {
                    serde_json::json!({
                        "sequence": entry.sequence,
                        "kind": entry.kind.tag(),
                        "text": entry.text,
                    })
                })
                .collect::<Vec<_>>(),
            "live_view": if ui.transcript.is_inspecting() { "hold" } else { "follow" },
            "reasoning_expanded": ui.transcript.is_reasoning_expanded(),
            "live_focus": ui.transcript.focused_block().map(|focus| match focus {
                LiveBlockFocus::Answer(id) => format!("answer:{id}"),
                LiveBlockFocus::Reasoning(id) => format!("reasoning:{id}"),
                LiveBlockFocus::Tool(id) => format!("tool:{id}"),
            }),
            "live_trace": phase_trace_label(&ui.transcript).unwrap_or_default(),
            "queued": vitals.queued,
            "queue": ui.queued.iter().take(4).cloned().collect::<Vec<_>>(),
            "input_tokens": ui.input_tokens,
            "output_tokens": ui.output_tokens,
            "stream_tokens": ui.stream_tokens,
            "effort": ui.effort.as_deref().unwrap_or("default"),
            "live_blocks": ui.transcript.inspector_rows().len(),
            "reasoning_history": ui.reasoning_history.len(),
            "answer_history": ui.answer_history.len(),
            "presentation": ui
                .presentation
                .records()
                .iter()
                .map(|record| {
                    serde_json::json!({
                        "id": record.id,
                        "channel": record.channel.tag(),
                        "status": record.status.tag(),
                        "step": record.step,
                        "elapsed_s": record.elapsed_s,
                        "tokens": record.tokens,
                        "chars": record.chars,
                    })
                })
                .collect::<Vec<_>>(),
            "step": vitals.step,
            "elapsed_s": vitals.elapsed_s,
            "rate": vitals.rate,
        },
        "panel": ui.panel.as_ref().map(|panel| {
            serde_json::json!({
                "kind": panel_kind_label(panel.kind),
                "query": panel.query.clone(),
                "selected": panel.selected().map(|row| row.key.clone()),
                "visible_rows": panel.view.len(),
                "detail_open": panel.detail_open,
                "detail_scroll": panel.detail_scroll,
            })
        }),
        "telemetry": {
            "phase_duration_ms": ui
                .activity_started
                .map(|started| started.elapsed().as_millis())
                .unwrap_or(0),
            "token_velocity": vitals.rate,
            "last_render_us": render_us,
        },
        "rows": rows,
        "styled_rows": snapshot_styled_rows(buffer),
    })
    .to_string()
}

/// Opt-in frame capture for terminal hosts that cannot expose their cell buffer.
/// Default path is absent, so normal rendering performs no extra allocation or I/O.
fn dump_frame_snapshot(
    frame: &mut ratatui::Frame,
    draw_started: std::time::Instant,
    ui: &Ui,
    vitals: &Vitals,
) {
    let Some(path) = std::env::var_os("RIDGE_TUI_SNAPSHOT")
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
    else {
        return;
    };
    let elapsed = draw_started.elapsed();
    if elapsed > std::time::Duration::from_millis(16) {
        tracing::warn!(
            render_us = elapsed.as_micros(),
            "RIDGE_TUI_SNAPSHOT exceeded 16ms"
        );
    }
    let payload = snapshot_payload(frame.buffer_mut(), elapsed.as_micros(), ui, vitals);
    let mut write_error = None;
    for attempt in 0..4 {
        match std::fs::write(&path, &payload) {
            Ok(()) => return,
            Err(error) if error.raw_os_error() == Some(32) && attempt < 3 => {
                write_error = Some(error);
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            Err(error) => {
                write_error = Some(error);
                break;
            }
        }
    }
    if let Some(error) = write_error {
        // A live harness may hold the previous frame briefly while reading it.
        // Dropping that diagnostic frame is preferable to polluting the TUI
        // with a warning or making the opt-in observer affect interaction.
        if error.raw_os_error() != Some(32) {
            tracing::warn!(?path, %error, "failed to write RIDGE_TUI_SNAPSHOT");
        }
    }
}

fn modal_rect(area: Rect) -> Rect {
    let width = if area.width >= 24 {
        area.width.saturating_sub(4).clamp(20, 80)
    } else {
        area.width.max(1)
    };
    let height = if area.height >= 8 {
        area.height.saturating_sub(2).max(6)
    } else {
        area.height.max(1)
    };
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

/// LiveHistory is the read-only transcript audit surface.  It owns the full
/// frame so search/detail navigation has stable geometry while the inline
/// terminal keeps its native scrollback and selection semantics.
pub(crate) fn panel_rect_for_kind(area: Rect, kind: PanelKind) -> Rect {
    if kind == PanelKind::LiveHistory {
        area
    } else {
        modal_rect(area)
    }
}

fn panel_kind_label(kind: PanelKind) -> &'static str {
    match kind {
        PanelKind::Config => "Config",
        PanelKind::Provider => "Provider",
        PanelKind::Tools => "Tools",
        PanelKind::ToolHistory => "History",
        PanelKind::ReasoningHistory => "Reasoning",
        PanelKind::AnswerHistory => "Answers",
        PanelKind::LiveHistory => "Audit",
        PanelKind::Activity => "Activity",
        PanelKind::Queue => "Queue",
        PanelKind::Models => "Models",
        PanelKind::Agent => "Agents",
        PanelKind::Login => "Login",
        PanelKind::Mcp => "MCP",
        PanelKind::Skills => "Skills",
    }
}

pub(crate) fn panel_title_role(kind: PanelKind) -> Role {
    match kind {
        PanelKind::AnswerHistory | PanelKind::LiveHistory => Role::Primary,
        PanelKind::ReasoningHistory
        | PanelKind::ToolHistory
        | PanelKind::Activity
        | PanelKind::Queue => Role::Info,
        _ => Role::Primary,
    }
}

fn panel_title(panel: &Panel, width: u16) -> String {
    let label = if panel.kind == PanelKind::ReasoningHistory && width < 24 {
        "Think"
    } else {
        panel_kind_label(panel.kind)
    };
    let raw = if width >= 24 {
        format!(" {} ", panel.title)
    } else if width >= 16 {
        format!(" {label} · Esc ")
    } else if width >= 10 {
        format!(" Esc · {label} ")
    } else {
        " Esc ".to_owned()
    };
    clip_display_cells(&raw, width)
}

fn panel_micro_hint(kind: PanelKind) -> &'static str {
    match kind {
        PanelKind::LiveHistory => "↕·␠·Del·Esc",
        PanelKind::Queue => "↕·Del·Esc",
        PanelKind::Activity
        | PanelKind::ToolHistory
        | PanelKind::ReasoningHistory
        | PanelKind::AnswerHistory => "↕·Enter↗·Esc",
        _ => "↕·↵·Esc",
    }
}

pub(crate) fn panel_hint(panel: &Panel, width: u16) -> String {
    let full = if panel.editing.is_some() {
        if panel.kind == PanelKind::Login {
            "Enter verify & connect · Esc cancel"
        } else {
            "Enter save · Esc cancel"
        }
    } else {
        match panel.kind {
            PanelKind::Config => "↑↓ select · Enter edit · type to filter · Esc close",
            PanelKind::Models | PanelKind::Provider => {
                "↑↓ select · Enter switch · type to filter · Esc close"
            }
            PanelKind::Login => "↑↓ pick provider · Enter enter key · type to filter · Esc close",
            PanelKind::Queue => "select · Delete remove · Ctrl+I inspect · type to filter · Esc close",
            PanelKind::ToolHistory | PanelKind::ReasoningHistory | PanelKind::AnswerHistory => {
                "↑↓/PgUp/PgDn select · Alt+PgUp/PgDn scroll detail · Home/End jump · Enter expand · Esc close"
            }
            PanelKind::LiveHistory => {
                "Ctrl+Space hold/follow · ↑↓/PgUp/PgDn select · Alt+PgUp/PgDn scroll detail · Home/End jump · Enter/Space expand · Delete pending · Ctrl+Q queue · Esc close"
            }
            PanelKind::Activity => {
                "↑↓ select · Alt+PgUp/PgDn scroll detail · Enter expand · type to filter · Esc close"
            }
            PanelKind::Tools
            | PanelKind::Agent
            | PanelKind::Mcp
            | PanelKind::Skills => {
                "↑↓ scroll · type to filter · Esc close"
            }
        }
    };
    let compact = if panel.editing.is_some() {
        "Enter · Esc"
    } else if panel.kind == PanelKind::Queue {
        "select · Del remove · Ctrl+I · Esc"
    } else if matches!(
        panel.kind,
        PanelKind::Activity
            | PanelKind::ToolHistory
            | PanelKind::ReasoningHistory
            | PanelKind::AnswerHistory
    ) {
        if width >= 25 {
            "Enter expand · Esc close"
        } else if width >= 17 {
            "↕ · Enter↗ · Esc"
        } else {
            "↕ Enter↗ · Esc"
        }
    } else if panel.kind == PanelKind::LiveHistory {
        if width >= 24 {
            "^Space hold/follow · ↑↓ · Enter/Space · Del pending · Ctrl+Q · Esc"
        } else if width >= 17 {
            "^Space·Enter↗·Esc"
        } else {
            "^Sp·Enter↗·Esc"
        }
    } else {
        "↑↓ · Enter · Esc"
    };
    let text = if width >= 64 {
        full
    } else if width >= 32 {
        match panel.kind {
            PanelKind::ToolHistory
            | PanelKind::ReasoningHistory
            | PanelKind::AnswerHistory
            | PanelKind::Activity => "↑↓ · Enter expand · Esc close",
            _ => full,
        }
    } else if width >= 14 {
        compact
    } else {
        panel_micro_hint(panel.kind)
    };
    // Keep the audit attention switch discoverable while a read-only panel
    // covers the live stream.  Reserve its cells before clipping the longer
    // panel-specific hint, so the affordance cannot disappear at the right
    // edge and ordinary panel hints remain truthful at narrow widths.
    if width >= 64 && panel.allows_attention_switch() {
        let attention = if width >= 96 {
            " · ^A answers · ^R think · ^O tools · ^T activity"
        } else {
            " · ^A/^R/^O/^T audit"
        };
        let budget = width.saturating_sub(str_cells(attention) as u16);
        format!("{}{attention}", clip_hint_with_close(text, budget))
    } else {
        clip_hint_with_close(text, width)
    }
}

/// Preserve the close affordance when a long full hint is clipped from the
/// right.  A covered audit panel must always advertise its escape hatch.
fn clip_hint_with_close(text: &str, width: u16) -> String {
    if width < 24 && text.contains("Enter↗") {
        return clip_display_cells(text, width);
    }
    if width < 24 && text.contains("Enter") {
        if text.contains("^Space") {
            return clip_display_cells("^Space·Enter·Esc", width);
        }
        return clip_display_cells("↕ · Enter · Esc", width);
    }
    if width < 32 && text.contains("hold/follow") {
        return clip_display_cells("^Space · Esc", width);
    }
    let Some(esc) = text.rfind("Esc") else {
        return clip_display_cells(text, width);
    };
    let close_start = text[..esc].rfind(" · ").unwrap_or(esc);
    let (body, close) = text.split_at(close_start);
    let close_cells = str_cells(close);
    if width < close_cells as u16 {
        return clip_display_cells(text, width);
    }
    format!(
        "{}{}",
        clip_display_cells(body, width.saturating_sub(close_cells as u16)),
        close
    )
}

fn panel_query(query: &str, width: u16) -> String {
    let prefix = if width >= 12 { "🔍 " } else { "> " };
    clip_display_cells(&format!("{prefix}{query}"), width)
}

/// Panel metadata should break at a word boundary when one fits.  Long
/// unbroken tokens still hard-wrap, so paths/CJK remain bounded without
/// turning `step` into `st`/`ep` in narrow audit rows.
fn wrap_panel_lines(text: &str, width: u16) -> Vec<String> {
    let width = width.max(1) as usize;
    let mut lines = Vec::new();
    for logical in text.split('\n') {
        let graphemes = logical.graphemes(true).collect::<Vec<_>>();
        if graphemes.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut start = 0;
        while start < graphemes.len() {
            let line_start = start;
            let mut end = start;
            let mut cells = 0;
            let mut last_break = None;
            while end < graphemes.len() {
                let used = str_cells(graphemes[end]);
                if cells > 0 && cells + used > width {
                    break;
                }
                cells += used;
                end += 1;
                if graphemes[end - 1].chars().all(char::is_whitespace) {
                    last_break = Some(end);
                }
            }
            let cut = if end < graphemes.len() {
                last_break
                    .filter(|&break_at| break_at > line_start)
                    .unwrap_or(end)
            } else {
                end
            };
            lines.push(graphemes[line_start..cut].concat().trim_end().to_owned());
            start = cut.max(line_start + 1);
            while start < graphemes.len() && graphemes[start].chars().all(char::is_whitespace) {
                start += 1;
            }
        }
    }
    lines
}

fn panel_item(text: String, width: u16) -> ListItem<'static> {
    let lines = wrap_panel_lines(&text, width)
        .into_iter()
        .map(Line::from)
        .collect::<Vec<_>>();
    ListItem::new(Text::from(lines))
}

fn panel_item_styled(text: String, width: u16, style: Style) -> ListItem<'static> {
    let lines = wrap_panel_lines(&text, width)
        .into_iter()
        .map(|line| Line::from(Span::styled(line, style)))
        .collect::<Vec<_>>();
    ListItem::new(Text::from(lines))
}

/// Keep audit surfaces visually aligned with the live stream without adding
/// execution state to `PanelRow`.  The key is already a presentation label;
/// this classifier only chooses a rail/title palette for that projection.
fn audit_channel(kind: PanelKind, key: &str) -> (Role, &'static str) {
    match kind {
        PanelKind::ReasoningHistory => (Role::Info, "THINK"),
        PanelKind::AnswerHistory => (Role::Primary, "ANSWER"),
        PanelKind::ToolHistory => (Role::Info, "TOOL"),
        PanelKind::Activity => (Role::Info, "EVENT"),
        PanelKind::LiveHistory if key.contains("Reasoning") || key.contains('💭') => {
            (Role::Info, "THINK")
        }
        PanelKind::LiveHistory if key.contains("Answer") || key.contains('🤖') => {
            (Role::Primary, "ANSWER")
        }
        PanelKind::LiveHistory if key.contains("pending") || key.contains("⏳") => {
            (Role::Warn, "PENDING")
        }
        PanelKind::LiveHistory if key.contains('⚙') => (Role::Info, "TOOL"),
        _ => (Role::Info, "DETAIL"),
    }
}

/// Render a row with a one-cell semantic rail.  Wrapping reserves the rail's
/// display width, so the added chrome cannot push CJK/emoji text past a narrow
/// terminal edge.
fn panel_item_with_rail(
    text: String,
    width: u16,
    rail: &str,
    rail_role: Role,
    text_style: Style,
) -> ListItem<'static> {
    let rail = if width > str_cells(rail) as u16 {
        rail
    } else {
        ""
    };
    let text_width = width.saturating_sub(str_cells(rail) as u16).max(1);
    let rail_style = Style::default().fg(role_color(rail_role));
    let lines = wrap_panel_lines(&text, text_width)
        .into_iter()
        .map(|line| {
            Line::from(vec![
                Span::styled(rail.to_owned(), rail_style),
                Span::styled(line, text_style),
            ])
        })
        .collect::<Vec<_>>();
    ListItem::new(Text::from(lines))
}

fn audit_row_style(kind: PanelKind, key: &str, selected: bool) -> Style {
    let (role, _) = audit_channel(kind, key);
    let mut style = Style::default().fg(role_color(role));
    if kind == PanelKind::ReasoningHistory && !selected {
        style = style.add_modifier(Modifier::DIM);
    }
    if selected {
        style = style.add_modifier(Modifier::BOLD);
    }
    style
}

/// Project an expanded audit body through the existing Markdown renderer.
/// This is modal-only work: the live tail keeps its cache, while an opened
/// detail pane gains the same heading/code/inline emphasis as the stream.
fn audit_detail_text(text: &str, kind: PanelKind, key: &str) -> Text<'static> {
    let (role, _) = audit_channel(kind, key);
    let base = Style::default().fg(role_color(role));
    let mut in_code = false;
    let mut alert_role = None;
    let source_lines = text.split('\n').collect::<Vec<_>>();
    let edges = super::render::alert_edges(source_lines.iter().copied());
    let lines = source_lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let (mut spans, next_code) =
                super::render::md_line_spans_with_alert(line, in_code, &mut alert_role);
            in_code = next_code;
            if let Some(edge) = edges.get(index).copied().flatten() {
                super::render::apply_alert_edge(&mut spans, edge);
            }
            let spans = spans
                .into_iter()
                .map(|mut span| {
                    span.style = base.patch(span.style);
                    span
                })
                .collect::<Vec<_>>();
            Line::from(spans)
        })
        .collect::<Vec<_>>();
    Text::from(lines)
}

/// Activity rows are telemetry, not prose: narrow panels keep the tag and
/// latest state on one physical row so scrolling never hides the timeline.
pub(crate) fn compact_activity_item(row: &PanelRow, width: u16) -> String {
    if width >= 48 {
        return format!("{:<8} {}", row.key, row.value);
    }
    let tag = row.key.split_whitespace().next().unwrap_or("SYS");
    let marker = if row.key.contains(" now") {
        "›"
    } else {
        "·"
    };
    let prefix = format!("{tag}{marker} ");
    let body_width = width.saturating_sub(str_cells(&prefix) as u16);
    if body_width == 0 {
        return clip_display_cells(&prefix, width);
    }
    format!("{prefix}{}", clip_display_cells(&row.value, body_width))
}

pub(crate) const MAX_PENDING_PREVIEW_ROWS: usize = 3;
pub(crate) const MAX_PENDING_PREVIEW_CHARS: usize = 2_048;

/// 输入框上方的有界待推送预览；队列仍由主环拥有，渲染只借用并做 cell 折行。
pub(crate) fn pending_queue_lines(
    queue: &std::collections::VecDeque<String>,
    width: u16,
) -> Vec<Line<'static>> {
    if queue.is_empty() || width == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    // Reserve the last row for an overflow affordance even for one huge
    // pasted intent.  Previously a single message could fill the cap and the
    // appended affordance silently made a three-row preview become four.
    let preview_rows = MAX_PENDING_PREVIEW_ROWS.saturating_sub(1).max(1);
    let mut shown_messages = 0usize;
    let mut truncated = false;
    for (index, message) in queue.iter().enumerate() {
        let prefix = if index == 0 {
            "⏭ next ".to_owned()
        } else {
            format!("⏳ [{}] ", index + 1)
        };
        let body_width = width.saturating_sub(str_cells(&prefix) as u16).max(1);
        // The visible preview is bounded by rows, so do not wrap an entire
        // pasted command just to discard all but its first few rows.  The
        // extra character probe is capped by the same preview budget and
        // keeps the queue's full message untouched for later execution.
        let preview_char_limit = (body_width as usize)
            .saturating_mul(preview_rows)
            .saturating_add(1)
            .min(MAX_PENDING_PREVIEW_CHARS);
        let preview = message.chars().take(preview_char_limit).collect::<String>();
        let message_clipped = message.chars().nth(preview_char_limit).is_some();
        let wrapped = wrap_input(&preview, preview.chars().count(), body_width).0;
        let mut shown = false;
        for (row, text) in wrapped.into_iter().enumerate() {
            if lines.len() == preview_rows {
                truncated = true;
                break;
            }
            let lead = if row == 0 {
                prefix.clone()
            } else {
                " ".repeat(str_cells(&prefix))
            };
            lines.push(Line::from(Span::styled(
                format!("{lead}{text}"),
                Style::default()
                    // Queued work is active user intent, not a warning.
                    .fg(role_color(Role::Primary))
                    .add_modifier(if row == 0 {
                        Modifier::BOLD
                    } else {
                        Modifier::DIM
                    }),
            )));
            shown = true;
        }
        if shown {
            shown_messages += 1;
        }
        truncated |= message_clipped;
        if truncated {
            break;
        }
    }
    if truncated {
        let omitted = queue.len().saturating_sub(shown_messages);
        let overflow = if omitted > 0 {
            format!("… +{omitted} pending · Ctrl+Enter push now")
        } else {
            "… more queued text · Ctrl+Enter push now".to_owned()
        };
        lines.push(Line::from(Span::styled(
            clip_display_cells(&overflow, width),
            Style::default().fg(role_color(Role::Muted)),
        )));
    }
    lines
}

/// A compact, low-saturation breadcrumb of observed phases.  It is a
/// presentation-only projection: Ctrl+T remains the detailed activity view.
fn activity_breadcrumb(
    history: &std::collections::VecDeque<ActivityEntry>,
    remaining: usize,
) -> Option<String> {
    if history.is_empty() || remaining < 16 {
        return None;
    }
    let mut tags = history
        .iter()
        .rev()
        .take(4)
        .map(|entry| entry.kind.tag())
        .collect::<Vec<_>>();
    tags.reverse();
    let full = format!(" ⟦{}⟧ ", tags.join("›"));
    if str_cells(&full) <= remaining {
        return Some(full);
    }
    tags = tags.into_iter().rev().take(3).collect();
    tags.reverse();
    let compact = format!(" ⟦{}⟧ ", tags.join("›"));
    (str_cells(&compact) <= remaining).then_some(compact)
}

fn activity_signal_chip(ui: &Ui, remaining: usize) -> Option<(String, Role)> {
    let entry = ui.activity_history.back()?;
    let redundant = matches!(
        (entry.kind, ui.transcript.active_channel()),
        (ActivityKind::Reasoning, Some(LiveChannel::Reasoning))
            | (ActivityKind::Answer, Some(LiveChannel::Answer))
            | (ActivityKind::Tool, Some(LiveChannel::Tool))
    );
    if redundant || entry.kind == ActivityKind::System {
        return None;
    }
    let (tag, role) = match entry.kind {
        ActivityKind::Plan => ("PLAN", Role::Reasoning),
        ActivityKind::Reasoning => ("THK", Role::Reasoning),
        ActivityKind::Answer => ("ANS", Role::Primary),
        ActivityKind::Tool => ("TLS", Role::Info),
        ActivityKind::Verification => ("CHK", Role::Info),
        ActivityKind::Conclusion => ("SUM", Role::Success),
        ActivityKind::Waiting => ("WAIT", Role::Warn),
        ActivityKind::Approval => ("ASK", Role::Warn),
        ActivityKind::Queue => ("QUE", Role::Info),
        ActivityKind::Takeover => ("TAKE", Role::Primary),
        ActivityKind::Completed => ("DONE", Role::Success),
        ActivityKind::Error => ("ERR", Role::Error),
        ActivityKind::System => return None,
    };
    let full = format!(" ⟦{tag}⟧ ");
    if str_cells(&full) <= remaining {
        Some((full, role))
    } else {
        None
    }
}

fn live_channel_tag(channel: LiveChannel) -> &'static str {
    match channel {
        LiveChannel::Reasoning => "THK",
        LiveChannel::Tool => "TLS",
        LiveChannel::Answer => "ANS",
    }
}

fn phase_trace_text(transcript: &LiveTranscript, width: u16) -> Option<String> {
    let trace = transcript.phase_trace();
    if trace.len() < 2 {
        return None;
    }
    let compact = width < 60;
    Some(
        trace
            .into_iter()
            .map(|channel| {
                if compact {
                    match channel {
                        LiveChannel::Reasoning => "T",
                        LiveChannel::Tool => "L",
                        LiveChannel::Answer => "A",
                    }
                } else {
                    live_channel_tag(channel)
                }
            })
            .collect::<Vec<_>>()
            .join("›"),
    )
}

fn phase_trace_label(transcript: &LiveTranscript) -> Option<String> {
    phase_trace_text(transcript, u16::MAX)
}

/// Wide-only phase trace: a compact, low-noise breadcrumb of observed model
/// channels.  Narrow frames keep the output viewport and critical status
/// affordances; the full Activity/Live Inspector remains the detail surface.
fn phase_trace_chip(transcript: &LiveTranscript, remaining: usize) -> Option<String> {
    let label = phase_trace_label(transcript)?;
    let full = format!(" ⟪{label}⟫ ");
    (str_cells(&full) <= remaining).then_some(full)
}

fn draw_device_auth(frame: &mut ratatui::Frame, area: Rect, status: &str) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let width = area.width.saturating_sub(4).clamp(32, 80).min(area.width);
    let height = area.height.min(7);
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    let lines = vec![
        Line::from(Span::styled(
            status.to_owned(),
            Style::default()
                .fg(role_color(Role::Warn))
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(format!(
            "Open: {}",
            provider::oauth::OPENAI_DEVICE_VERIFICATION_URL
        )),
        Line::from("Waiting for authorization · Esc cancel"),
    ];
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(
                rounded_surface_block()
                    .title(" Codex device auth ")
                    .border_style(Style::default().fg(role_color(Role::Warn))),
            )
            .wrap(Wrap { trim: false }),
        rect,
    );
}

pub(crate) fn detail_match_scroll(text: &str, query: &str, width: u16, visible_rows: u16) -> u16 {
    let query = query.trim().to_lowercase();
    if query.is_empty() || width == 0 || visible_rows == 0 {
        return 0;
    }
    let mut offset = 0usize;
    for line in text.lines() {
        if let Some(match_row) = detail_match_row(line, &query, width as usize) {
            let centered = offset
                .saturating_add(match_row)
                .saturating_sub(visible_rows as usize / 2);
            return centered.min(u16::MAX as usize) as u16;
        }
        offset = offset.saturating_add(wrapped_rows(line, width));
    }
    0
}

#[cfg(test)]
pub(crate) fn detail_scroll_position(
    text: &str,
    query: &str,
    width: u16,
    visible_rows: u16,
    adjustment: i16,
) -> u16 {
    detail_scroll_position_with_total_rows(
        text,
        query,
        width,
        visible_rows,
        wrapped_rows(text, width),
        adjustment,
    )
}

/// Scroll an audit detail using the physical row count of the widget that
/// will render it.  The public compatibility helper above keeps pure callers
/// on the old text-only estimate; the modal path supplies the cached count so
/// its viewport cannot drift from Ratatui's actual wrapping.
pub(crate) fn detail_scroll_position_with_total_rows(
    text: &str,
    query: &str,
    width: u16,
    visible_rows: u16,
    total_rows: usize,
    adjustment: i16,
) -> u16 {
    let search_scroll = detail_match_scroll(text, query, width, visible_rows);
    let requested_scroll = if adjustment < 0 {
        search_scroll.saturating_sub(adjustment.unsigned_abs())
    } else {
        search_scroll.saturating_add(adjustment as u16)
    };
    let max_scroll = total_rows
        .saturating_sub(visible_rows as usize)
        .min(u16::MAX as usize) as u16;
    requested_scroll.min(max_scroll)
}

fn detail_match_row(line: &str, query: &str, width: usize) -> Option<usize> {
    let start = line.to_lowercase().find(query)?;
    let mut row = 0;
    let mut cells = 0;
    for (byte, c) in line.char_indices() {
        let char_width = char_cells(c);
        if byte >= start {
            if cells + char_width > width && cells > 0 {
                row += 1;
            }
            return Some(row);
        }
        if cells + char_width > width && cells > 0 {
            row += 1;
            cells = 0;
        }
        cells += char_width;
    }
    Some(row)
}

/// 交互页模态绘制(iter-35):居中框(≤80 宽)= 搜索/编辑行 + 过滤列表(选中高亮)+ 提示行。
fn standard_panel_items(panel: &Panel, width: u16) -> Vec<ListItem<'static>> {
    if panel.kind == PanelKind::Models {
        let mut items = Vec::new();
        let mut last_group: Option<&str> = None;
        for &i in &panel.view {
            let row = &panel.rows[i];
            let group = row.key.split_once(" · ").map(|(g, _)| g);
            if group != last_group {
                let header = format!(" ── {} ──", group.unwrap_or(""));
                items.push(panel_item_styled(
                    header,
                    width,
                    Style::default()
                        .fg(role_color(Role::Muted))
                        .add_modifier(Modifier::BOLD),
                ));
                last_group = group;
            }
            let name = row
                .key
                .split_once(" · ")
                .map(|(_, model)| model)
                .unwrap_or(&row.key);
            let line = if row.value.is_empty() {
                format!("  {name}")
            } else {
                format!("  {name:<18} {}", row.value)
            };
            items.push(panel_item(line, width));
        }
        items
    } else if panel.kind == PanelKind::Activity {
        panel
            .view
            .iter()
            .map(|&index| panel_item(compact_activity_item(&panel.rows[index], width), width))
            .collect()
    } else {
        panel
            .view
            .iter()
            .map(|&index| {
                let row = &panel.rows[index];
                let line = if row.value.is_empty() {
                    row.key.clone()
                } else {
                    format!("{:<18} {}", row.key, row.value)
                };
                panel_item(line, width)
            })
            .collect()
    }
}

fn detail_panel_items(panel: &Panel, width: u16) -> Vec<ListItem<'static>> {
    let selected_index = panel.selected_index();
    panel
        .view
        .iter()
        .map(|&index| {
            let row = &panel.rows[index];
            let selected = selected_index == Some(index);
            if panel.kind == PanelKind::Activity {
                return panel_item_styled(
                    compact_activity_item(row, width),
                    width,
                    Style::default().fg(role_color(Role::Info)),
                );
            }
            let marker = if row.value.is_empty() {
                "· "
            } else if panel.kind == PanelKind::ReasoningHistory {
                if selected {
                    "┃ "
                } else {
                    "│ "
                }
            } else if panel.kind == PanelKind::LiveHistory && selected {
                "┃ "
            } else if panel.detail_open && selected {
                "▾ "
            } else {
                "▸ "
            };
            let style = audit_row_style(panel.kind, &row.key, selected);
            let (role, _) = audit_channel(panel.kind, &row.key);
            if row.value.is_empty() {
                panel_item_styled(format!("{marker}{}", row.key), width, style)
            } else {
                panel_item_with_rail(row.key.clone(), width, marker, role, style)
            }
        })
        .collect()
}

#[cfg(test)]
pub(crate) fn draw_panel(frame: &mut ratatui::Frame, area: Rect, panel: &Panel) {
    let mut panel_cache = PanelLayoutCache::default();
    draw_panel_with_cache(frame, area, panel, &mut panel_cache);
}

fn draw_panel_with_cache(
    frame: &mut ratatui::Frame,
    area: Rect,
    panel: &Panel,
    panel_cache: &mut PanelLayoutCache,
) {
    // A modal owns the audit viewport. Clear the full supplied surface first;
    // clearing only the inset panel rectangle leaks the live/idle projection
    // through narrow gutters and makes the border look corrupted.
    frame.render_widget(Clear, area);
    if panel.supports_detail() {
        draw_history_detail_panel(frame, area, panel, panel_cache);
        return;
    }
    let rect = panel_rect_for_kind(area, panel.kind);
    let block = rounded_surface_block()
        .title(panel_title(panel, rect.width.saturating_sub(2)))
        .title_style(
            Style::default()
                .fg(role_color(panel_title_role(panel.kind)))
                .add_modifier(Modifier::BOLD),
        )
        .border_style(Style::default().fg(role_color(Role::Border)));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let show_query = inner.height >= 3;
    let show_hint = inner.height >= 2;
    let mut constraints = Vec::with_capacity(3);
    if show_query {
        constraints.push(Constraint::Length(1)); // 搜索/编辑行
    }
    constraints.push(Constraint::Min(1)); // 列表
    if show_hint {
        constraints.push(Constraint::Length(1)); // 提示
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);
    let list_index = usize::from(show_query);
    // 搜索行(编辑态显编辑缓冲;登录页的 key 输入掩码,防肩窥)。
    let (head, head_color) = match &panel.editing {
        Some(buf) if panel.kind == PanelKind::Login => (
            format!("✎ API key: {}", "•".repeat(buf.chars().count())),
            role_color(Role::Warn),
        ),
        Some(buf) => (format!("✎ new value: {buf}"), role_color(Role::Warn)),
        None => (
            panel_query(&panel.query, inner.width),
            role_color(Role::Muted),
        ),
    };
    if show_query {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                clip_display_cells(&head, rows[0].width),
                Style::default().fg(head_color),
            ))),
            rows[0],
        );
    }
    // 过滤列表:key 左对齐 + 右列值。Models 页加 provider 分栏标题。
    let sel_in_items = if panel.kind == PanelKind::Models {
        let mut idx = 0;
        let mut last_group: Option<&str> = None;
        for (vi, &i) in panel.view.iter().enumerate() {
            let row = &panel.rows[i];
            let group = row.key.split_once(" · ").map(|(group, _)| group);
            if group != last_group {
                idx += 1;
                last_group = group;
            }
            if vi == panel.sel {
                break;
            }
            idx += 1;
        }
        (!panel.view.is_empty()).then_some(idx)
    } else {
        (!panel.view.is_empty()).then_some(panel.sel)
    };
    let (items, selected_item) = panel_cache.items.viewport(
        panel,
        rows[list_index].width,
        rows[list_index].height,
        sel_in_items,
    );
    let mut state = ListState::default();
    state.select(selected_item);
    frame.render_stateful_widget(
        List::new(items).highlight_style(selection_style()),
        rows[list_index],
        &mut state,
    );
    if show_hint {
        let hint_index = rows.len() - 1;
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                panel_hint(panel, rows[hint_index].width),
                Style::default().fg(role_color(Role::Muted)),
            ))),
            rows[hint_index],
        );
    }
}

/// 历史块检视器:原生 scrollback 保持静态,当前摘要列表与有界详情在 live modal 中交互。
/// Narrow audit modal: once a detail is explicitly open, spend the scarce
/// vertical budget on the selected Answer/Reasoning/Tool body instead of
/// painting a list that can only show a few rows. Selection, query, scroll,
/// and Enter/Esc semantics remain owned by `Panel`; this is only a projection.
fn draw_narrow_history_detail(
    frame: &mut ratatui::Frame,
    inner: Rect,
    panel: &Panel,
    panel_cache: &mut PanelLayoutCache,
    detail: &str,
    detail_key: &str,
) {
    let show_query = inner.height >= 3;
    let show_hint = inner.height >= 2;
    let mut constraints = Vec::with_capacity(3);
    if show_query {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(1));
    if show_hint {
        constraints.push(Constraint::Length(1));
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);
    let detail_index = usize::from(show_query);

    if show_query {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                panel_query(&panel.query, rows[0].width),
                Style::default().fg(role_color(Role::Muted)),
            ))),
            rows[0],
        );
    }

    let detail_area = rows[detail_index];
    let (detail_role, detail_label) = audit_channel(panel.kind, detail_key);
    let detail_block = Block::default()
        .borders(Borders::TOP)
        .title(clip_display_cells(
            &format!(" {detail_label} ▾ · {detail_key} "),
            detail_area.width.saturating_sub(1),
        ))
        .title_style(
            Style::default()
                .fg(role_color(detail_role))
                .add_modifier(Modifier::BOLD),
        )
        .border_style(Style::default().fg(role_color(detail_role)));
    let detail_inner = detail_block.inner(detail_area);
    let detail_width = detail_inner.width.max(1);
    let total_rows = panel_cache.detail.prepare(
        panel.content_revision,
        panel.selected_index().unwrap_or(usize::MAX),
        detail,
        panel.kind,
        detail_key,
        detail_width,
    );
    let visible_rows = detail_inner.height.max(1);
    let detail_scroll = detail_scroll_position_with_total_rows(
        detail,
        &panel.query,
        detail_width,
        visible_rows,
        total_rows,
        panel.detail_scroll,
    );
    frame.render_widget(
        Paragraph::new(panel_cache.detail.text())
            .style(Style::default().fg(role_color(detail_role)))
            .block(detail_block)
            .wrap(Wrap { trim: false })
            .scroll((detail_scroll, 0)),
        detail_area,
    );

    if show_hint {
        let hint_index = rows.len() - 1;
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                panel_hint(panel, rows[hint_index].width),
                Style::default().fg(role_color(Role::Muted)),
            ))),
            rows[hint_index],
        );
    }
}

fn draw_history_detail_panel(
    frame: &mut ratatui::Frame,
    area: Rect,
    panel: &Panel,
    panel_cache: &mut PanelLayoutCache,
) {
    let rect = panel_rect_for_kind(area, panel.kind);
    let block = rounded_surface_block()
        .title(panel_title(panel, rect.width.saturating_sub(2)))
        .title_style(
            Style::default()
                .fg(role_color(panel_title_role(panel.kind)))
                .add_modifier(Modifier::BOLD),
        )
        .border_style(Style::default().fg(role_color(Role::Border)));
    let inner = block.inner(rect);
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);

    let selected_row = panel.selected();
    let selected_detail = selected_row
        .filter(|row| panel.detail_open && !row.value.is_empty())
        .map(|row| row.value.clone());
    // Wide audit frames have enough horizontal room to keep the block rail and
    // the selected Answer/Reasoning detail visible at once.  Narrow frames
    // retain the vertical stack so the detail column never becomes unreadable.
    if inner.width >= 72 && inner.height >= 8 {
        if let Some(detail) = selected_detail.as_deref() {
            draw_history_detail_split(
                frame,
                inner,
                panel,
                panel_cache,
                detail,
                selected_row.map(|row| row.key.as_str()).unwrap_or_default(),
            );
            return;
        }
    }
    // In a narrow modal, an explicitly opened detail is the user's audit
    // target.  Remove only the competing list projection; the selected row,
    // query, scroll offset, and all key semantics remain unchanged.
    if inner.width < 72 && inner.height >= 6 {
        if let (Some(detail), Some(row)) = (selected_detail.as_deref(), selected_row) {
            draw_narrow_history_detail(frame, inner, panel, panel_cache, detail, row.key.as_str());
            return;
        }
    }
    let show_query = inner.height >= 3;
    let show_hint = inner.height >= 2;
    let max_detail = inner.height.saturating_sub(3).max(1) as usize;
    let detail_width = inner.width.saturating_sub(1).max(1);
    let detail_selected_index = panel.selected_index().unwrap_or(usize::MAX);
    let detail_rows = selected_detail
        .as_deref()
        .filter(|_| show_query && show_hint && inner.height >= 4)
        .map(|text| {
            panel_cache.detail.prepare(
                panel.content_revision,
                detail_selected_index,
                text,
                panel.kind,
                selected_row.map(|row| row.key.as_str()).unwrap_or_default(),
                detail_width,
            )
        });
    let detail_height = selected_detail
        .as_deref()
        .filter(|_| show_query && show_hint && inner.height >= 4)
        .zip(detail_rows)
        .map(|(_, rows)| {
            let minimum = if max_detail >= 2 { 2 } else { 1 };
            (rows.saturating_add(1)).min(max_detail).max(minimum) as u16
        })
        .unwrap_or(0);
    let mut constraints = Vec::with_capacity(4);
    if show_query {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(1));
    if detail_height > 0 {
        constraints.push(Constraint::Length(detail_height));
    }
    if show_hint {
        constraints.push(Constraint::Length(1));
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);
    let list_index = usize::from(show_query);
    if show_query {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                panel_query(&panel.query, rows[0].width),
                Style::default().fg(role_color(Role::Muted)),
            ))),
            rows[0],
        );
    }

    let (items, selected_item) = panel_cache.items.viewport(
        panel,
        rows[list_index].width,
        rows[list_index].height,
        (!panel.view.is_empty()).then_some(panel.sel),
    );
    let mut state = ListState::default();
    state.select(selected_item);
    frame.render_stateful_widget(
        List::new(items).highlight_style(selection_style()),
        rows[list_index],
        &mut state,
    );

    if detail_height > 0 {
        if let Some(detail) = selected_detail {
            let detail_index = list_index + 1;
            let detail_key = selected_row.map(|row| row.key.as_str()).unwrap_or_default();
            let (detail_role, detail_label) = audit_channel(panel.kind, detail_key);
            let detail_block = Block::default()
                .borders(Borders::TOP | Borders::LEFT)
                .title(format!(" {detail_label} "))
                .title_style(
                    Style::default()
                        .fg(role_color(detail_role))
                        .add_modifier(Modifier::BOLD),
                )
                .border_style(Style::default().fg(role_color(detail_role)));
            let detail_inner = detail_block.inner(rows[detail_index]);
            let visible_rows = detail_inner.height.max(1);
            let detail_scroll = detail_scroll_position_with_total_rows(
                &detail,
                &panel.query,
                detail_inner.width.max(1),
                visible_rows,
                detail_rows.unwrap_or(1),
                panel.detail_scroll,
            );
            frame.render_widget(
                Paragraph::new(panel_cache.detail.text())
                    .style(Style::default().fg(role_color(detail_role)))
                    .block(detail_block)
                    .wrap(Wrap { trim: false })
                    .scroll((detail_scroll, 0)),
                rows[detail_index],
            );
        }
    }
    if show_hint {
        let hint_index = rows.len() - 1;
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                panel_hint(panel, rows[hint_index].width),
                Style::default().fg(role_color(Role::Muted)),
            ))),
            rows[hint_index],
        );
    }
}

fn draw_history_detail_split(
    frame: &mut ratatui::Frame,
    inner: Rect,
    panel: &Panel,
    panel_cache: &mut PanelLayoutCache,
    detail: &str,
    detail_key: &str,
) {
    let show_query = inner.height >= 3;
    let show_hint = inner.height >= 2;
    let mut constraints = Vec::with_capacity(3);
    if show_query {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(1));
    if show_hint {
        constraints.push(Constraint::Length(1));
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);
    let list_index = usize::from(show_query);
    if show_query {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                panel_query(&panel.query, rows[0].width),
                Style::default().fg(role_color(Role::Muted)),
            ))),
            rows[0],
        );
    }

    let content = rows[list_index];
    let split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Min(1)])
        .split(content);
    let (items, selected_item) = panel_cache.items.viewport(
        panel,
        split[0].width,
        split[0].height,
        (!panel.view.is_empty()).then_some(panel.sel),
    );
    let mut state = ListState::default();
    state.select(selected_item);
    frame.render_stateful_widget(
        List::new(items).highlight_style(selection_style()),
        split[0],
        &mut state,
    );

    let (detail_role, detail_label) = audit_channel(panel.kind, detail_key);
    let detail_block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {detail_label} "))
        .title_style(
            Style::default()
                .fg(role_color(detail_role))
                .add_modifier(Modifier::BOLD),
        )
        .border_style(Style::default().fg(role_color(detail_role)));
    let detail_inner = detail_block.inner(split[1]);
    let detail_width = detail_inner.width.max(1);
    let total_rows = panel_cache.detail.prepare(
        panel.content_revision,
        panel.selected_index().unwrap_or(usize::MAX),
        detail,
        panel.kind,
        detail_key,
        detail_width,
    );
    let visible_rows = detail_inner.height.max(1);
    let detail_scroll = detail_scroll_position_with_total_rows(
        detail,
        &panel.query,
        detail_width,
        visible_rows,
        total_rows,
        panel.detail_scroll,
    );
    frame.render_widget(
        Paragraph::new(panel_cache.detail.text())
            .style(Style::default().fg(role_color(detail_role)))
            .block(detail_block)
            .wrap(Wrap { trim: false })
            .scroll((detail_scroll, 0)),
        split[1],
    );

    if show_hint {
        let hint_index = rows.len() - 1;
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                panel_hint(panel, rows[hint_index].width),
                Style::default().fg(role_color(Role::Muted)),
            ))),
            rows[hint_index],
        );
    }
}

#[derive(Default)]
struct LiveRowState {
    code_before: bool,
    alert_role: Option<Role>,
    previous_kind: Option<LiveLineKind>,
    previous_focused_tool: bool,
    focused_block: Option<LiveBlockFocus>,
    block_end: bool,
    max_rows: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DetailLayoutKey {
    panel_revision: u64,
    selected_index: usize,
    width: u16,
}

/// Presentation-only cache for the currently expanded audit detail.
///
/// A modal has one selected detail, so a single entry is enough: it avoids
/// rescanning and reparsing a long Markdown block on every telemetry redraw,
/// while panel rebuilds, selection changes, and width changes invalidate it.
#[derive(Default)]
pub(crate) struct DetailLayoutCache {
    key: Option<DetailLayoutKey>,
    text: Option<Text<'static>>,
    rows: usize,
    #[cfg(test)]
    rebuilds: usize,
}

impl DetailLayoutCache {
    pub(crate) fn prepare(
        &mut self,
        panel_revision: u64,
        selected_index: usize,
        detail: &str,
        kind: PanelKind,
        row_key: &str,
        width: u16,
    ) -> usize {
        let key = DetailLayoutKey {
            panel_revision,
            selected_index,
            width,
        };
        if self.key != Some(key) {
            let text = audit_detail_text(detail, kind, row_key);
            self.rows = Paragraph::new(text.clone())
                .wrap(Wrap { trim: false })
                .line_count(width)
                .max(1);
            self.text = Some(text);
            self.key = Some(key);
            #[cfg(test)]
            {
                self.rebuilds += 1;
            }
        }
        self.rows
    }

    pub(crate) fn text(&self) -> Text<'static> {
        self.text.clone().unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn rebuilds(&self) -> usize {
        self.rebuilds
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PanelItemsKey {
    panel_revision: u64,
    kind: PanelKind,
    width: u16,
    query: String,
    /// Standard list rows are selection-independent: ListState paints the
    /// highlight, while detail rows encode selection in their item marker.
    selected: Option<usize>,
    detail_open: bool,
}

/// Presentation-only cache for all list items in the open panel. Selection
/// changes only invalidate detail panels; standard lists reuse wrapped rows.
///
/// Panel rows are already bounded by their owning histories. Caching the
/// wrapped `ListItem`s removes repeated full-list cell wrapping during token
/// redraws, while query/selection/detail/width changes remain explicit cache
/// boundaries. The cache owns no execution or queue state.
#[derive(Default)]
pub(crate) struct PanelItemsCache {
    key: Option<PanelItemsKey>,
    items: Vec<ListItem<'static>>,
    heights: Vec<usize>,
    #[cfg(test)]
    rebuilds: usize,
}

impl PanelItemsCache {
    fn ensure_items(&mut self, panel: &Panel, width: u16) {
        let key = PanelItemsKey {
            panel_revision: panel.content_revision,
            kind: panel.kind,
            width,
            query: panel.query.clone(),
            selected: panel.supports_detail().then_some(panel.sel),
            detail_open: panel.supports_detail() && panel.detail_open,
        };
        if self.key.as_ref() != Some(&key) {
            self.items = if panel.supports_detail() {
                detail_panel_items(panel, width)
            } else {
                standard_panel_items(panel, width)
            };
            self.heights = self.items.iter().map(ListItem::height).collect();
            self.key = Some(key);
            #[cfg(test)]
            {
                self.rebuilds += 1;
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn items(&mut self, panel: &Panel, width: u16) -> Vec<ListItem<'static>> {
        self.ensure_items(panel, width);
        self.items.clone()
    }

    /// Project only the physical list-item window that can reach this frame.
    /// The cache still owns one bounded panel snapshot; the renderer never
    /// receives off-screen items, so multi-line rows do not turn a narrow
    /// modal into an avoidable full-list render.
    pub(crate) fn viewport(
        &mut self,
        panel: &Panel,
        width: u16,
        height: u16,
        selected: Option<usize>,
    ) -> (Vec<ListItem<'static>>, Option<usize>) {
        self.ensure_items(panel, width);
        let (start, end) = panel_viewport_range(&self.heights, height as usize, selected);
        let local_selected = selected
            .filter(|&index| index >= start && index < end)
            .map(|index| index - start);
        (self.items[start..end].to_vec(), local_selected)
    }

    #[cfg(test)]
    pub(crate) fn rebuilds(&self) -> usize {
        self.rebuilds
    }
}

/// Pick a bounded item window while keeping the selected item visible.  List
/// items may already contain wrapped physical lines, so this uses their
/// cached heights instead of treating each logical row as one terminal row.
pub(crate) fn panel_viewport_range(
    heights: &[usize],
    viewport_height: usize,
    selected: Option<usize>,
) -> (usize, usize) {
    if heights.is_empty() || viewport_height == 0 {
        return (0, 0);
    }

    let selected = selected.filter(|&index| index < heights.len());
    let Some(selected) = selected else {
        let mut used = 0usize;
        let mut end = 0;
        while end < heights.len() {
            let height = heights[end].max(1);
            if used > 0 && used.saturating_add(height) > viewport_height {
                break;
            }
            used = used.saturating_add(height);
            end += 1;
        }
        return (0, end.max(1).min(heights.len()));
    };

    let selected_height = heights[selected].max(1);
    let mut start = selected;
    let mut used = selected_height;
    let before_budget = viewport_height.saturating_sub(selected_height) / 2;
    let mut before = 0usize;
    while start > 0 {
        let height = heights[start - 1].max(1);
        if before.saturating_add(height) > before_budget {
            break;
        }
        start -= 1;
        before = before.saturating_add(height);
        used = used.saturating_add(height);
    }

    let mut end = selected + 1;
    while end < heights.len() {
        let height = heights[end].max(1);
        if used.saturating_add(height) > viewport_height {
            break;
        }
        used = used.saturating_add(height);
        end += 1;
    }

    // When the selection is near the tail, fill any remaining budget with
    // older rows so the viewport does not collapse to a single item.
    while start > 0 {
        let height = heights[start - 1].max(1);
        if used.saturating_add(height) > viewport_height {
            break;
        }
        start -= 1;
        used = used.saturating_add(height);
    }
    (start, end)
}

#[derive(Default)]
pub(crate) struct PanelLayoutCache {
    pub(crate) items: PanelItemsCache,
    pub(crate) detail: DetailLayoutCache,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LiveOutputCacheKey {
    // Only values that change physical rows belong here.  Step/time/token
    // telemetry is rendered by the chrome, not duplicated inside the tail.
    revision: u64,
    width: u16,
    rows: u16,
    busy: bool,
}

struct RenderedLiveTail {
    lines: Vec<Line<'static>>,
    /// Semantic identity of the first physical row.  Used only to preserve a
    /// held viewport when a width/height change alters wrapping.
    anchor: Option<LiveLineAnchor>,
}

/// Presentation-only cache for the expensive semantic-to-physical live tail.
/// The blinking cursor is deliberately added after the cache, so animation
/// frames reuse wrapped rows without freezing the cursor style.
#[derive(Default)]
pub(crate) struct LiveOutputCache {
    key: Option<LiveOutputCacheKey>,
    paragraph: Option<Paragraph<'static>>,
    line_count: usize,
    last_line_cells: usize,
    #[cfg(test)]
    lines: Vec<Line<'static>>,
    anchor: Option<LiveLineAnchor>,
    panel: PanelLayoutCache,
    #[cfg(test)]
    rebuilds: usize,
}

impl LiveOutputCache {
    fn prepare(
        &mut self,
        transcript: &LiveTranscript,
        width: u16,
        rows: usize,
        busy: bool,
        _vitals: &Vitals,
    ) {
        let key = LiveOutputCacheKey {
            revision: transcript.render_revision(),
            width,
            rows: rows.min(u16::MAX as usize) as u16,
            busy,
        };
        if self.key != Some(key) {
            let preferred_anchor = self.key.filter(|old| {
                old.revision == key.revision
                    && (old.width != key.width || old.rows != key.rows)
                    && transcript.is_inspecting()
            });
            let rendered = render_live_tail_projection(
                transcript,
                width,
                rows,
                busy,
                preferred_anchor.and(self.anchor),
            );
            self.line_count = rendered.lines.len();
            self.last_line_cells = rendered
                .lines
                .last()
                .map(|line| {
                    line.spans
                        .iter()
                        .map(|span| str_cells(span.content.as_ref()))
                        .sum()
                })
                .unwrap_or_default();
            #[cfg(test)]
            {
                self.lines = rendered.lines.clone();
            }
            let scroll = self
                .line_count
                .saturating_sub(key.rows as usize)
                .min(u16::MAX as usize) as u16;
            self.paragraph = Some(Paragraph::new(Text::from(rendered.lines)).scroll((scroll, 0)));
            self.anchor = rendered.anchor;
            self.key = Some(key);
            #[cfg(test)]
            {
                self.rebuilds += 1;
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn lines(
        &mut self,
        transcript: &LiveTranscript,
        width: u16,
        rows: usize,
        busy: bool,
        vitals: &Vitals,
    ) -> Vec<Line<'static>> {
        self.prepare(transcript, width, rows, busy, vitals);
        self.lines.clone()
    }

    fn is_empty(&self) -> bool {
        self.line_count == 0
    }

    fn render(&self, frame: &mut ratatui::Frame, area: Rect) {
        if let Some(paragraph) = self.paragraph.as_ref() {
            frame.render_widget(paragraph, area);
        }
    }

    fn cursor_cells(&self) -> usize {
        self.last_line_cells
    }

    #[cfg(test)]
    pub(crate) fn rebuilds(&self) -> usize {
        self.rebuilds
    }
}

fn fit_live_marker(
    marker: Cow<'static, str>,
    kind: LiveLineKind,
    width: u16,
    rail_cells: usize,
) -> Option<Cow<'static, str>> {
    let available = width.saturating_sub(rail_cells as u16);
    if available == 0 {
        return None;
    }
    if str_cells(marker.as_ref()) <= available as usize {
        return Some(marker);
    }
    if kind == LiveLineKind::Reasoning && str_cells("💭 ") <= available as usize {
        // Keep the channel glyph and give the model text the remaining cells;
        // full step/token metadata remains visible in the top chrome.
        return Some(Cow::Borrowed("💭 "));
    }
    Some(Cow::Owned(clip_display_cells(marker.as_ref(), available)))
}

/// First-line phase marker for the live projection. Labels are a semantic
/// affordance, not a new transcript row: wide frames say what the user is
/// looking at, medium frames use phase-trace abbreviations, and narrow frames
/// retain compact glyphs that leave more room for model text.
pub(crate) fn live_phase_marker(
    kind: LiveLineKind,
    marker: Option<&str>,
    previous_kind: Option<LiveLineKind>,
    width: u16,
) -> Option<String> {
    let opener = match kind {
        LiveLineKind::Answer | LiveLineKind::Reasoning => marker.is_some(),
        LiveLineKind::ToolSummary => {
            marker.is_some()
                || !matches!(
                    previous_kind,
                    Some(LiveLineKind::ToolSummary | LiveLineKind::ToolDetail)
                )
        }
        LiveLineKind::ToolDetail | LiveLineKind::Splash => false,
    };
    if !opener {
        return marker.map(str::to_owned);
    }
    if width < 48 {
        return marker
            .map(str::to_owned)
            .or_else(|| (kind == LiveLineKind::ToolSummary).then(|| "◈ ".to_owned()));
    }

    let (wide, compact) = match kind {
        LiveLineKind::Answer => ("ANSWER", "ANS"),
        LiveLineKind::Reasoning => ("THINK", "THK"),
        LiveLineKind::ToolSummary => ("TOOL", "TLS"),
        LiveLineKind::ToolDetail | LiveLineKind::Splash => return marker.map(str::to_owned),
    };
    let label = if width >= 72 { wide } else { compact };
    if kind == LiveLineKind::Reasoning {
        let metadata = marker
            .and_then(|value| value.strip_prefix("💭 "))
            .unwrap_or("")
            .trim();
        if metadata.is_empty() {
            Some(format!(" {label} "))
        } else {
            Some(format!(" {label} {metadata} "))
        }
    } else {
        Some(format!(" {label} "))
    }
}

#[derive(Clone)]
struct LiveFragment {
    text: String,
    style: Style,
    cells: usize,
}

enum LiveWrapUnit {
    Word(Vec<LiveFragment>),
    Whitespace(Vec<LiveFragment>),
    Newline,
}

fn live_wrap_units(spans: Vec<Span<'static>>) -> Vec<LiveWrapUnit> {
    let mut units = Vec::new();
    let mut kind = None;
    let mut fragments = Vec::new();

    for span in spans {
        let style = span.style;
        for grapheme in span.content.as_ref().graphemes(true) {
            if grapheme == "\n" {
                if let Some(kind) = kind.take() {
                    let chunk = std::mem::take(&mut fragments);
                    units.push(if kind {
                        LiveWrapUnit::Whitespace(chunk)
                    } else {
                        LiveWrapUnit::Word(chunk)
                    });
                }
                units.push(LiveWrapUnit::Newline);
                continue;
            }

            let whitespace = grapheme.chars().all(char::is_whitespace);
            if kind.is_some_and(|current| current != whitespace) {
                let previous = kind.take().expect("live unit kind exists");
                let chunk = std::mem::take(&mut fragments);
                units.push(if previous {
                    LiveWrapUnit::Whitespace(chunk)
                } else {
                    LiveWrapUnit::Word(chunk)
                });
            }
            kind = Some(whitespace);
            fragments.push(LiveFragment {
                text: grapheme.to_owned(),
                style,
                cells: str_cells(grapheme),
            });
        }
    }

    if let Some(kind) = kind {
        units.push(if kind {
            LiveWrapUnit::Whitespace(fragments)
        } else {
            LiveWrapUnit::Word(fragments)
        });
    }
    units
}

fn live_fragments_cells(fragments: &[LiveFragment]) -> usize {
    fragments.iter().map(|fragment| fragment.cells).sum()
}

fn append_live_fragments(
    lines: &mut Vec<Vec<Span<'static>>>,
    fragments: &[LiveFragment],
    cells: &mut usize,
    width: usize,
) {
    for fragment in fragments {
        if *cells > 0 && cells.saturating_add(fragment.cells) > width {
            lines.push(Vec::new());
            *cells = 0;
        }
        lines
            .last_mut()
            .expect("live wrap always owns one row")
            .push(Span::styled(fragment.text.clone(), fragment.style));
        *cells = cells.saturating_add(fragment.cells);
    }
}

pub(crate) fn wrap_live_spans_greedy(
    spans: Vec<Span<'static>>,
    width: u16,
) -> Vec<Vec<Span<'static>>> {
    let width = width.max(1) as usize;
    let mut lines = vec![Vec::new()];
    let mut cells = 0usize;
    for span in spans {
        for grapheme in span.content.as_ref().graphemes(true) {
            if grapheme == "\n" {
                lines.push(Vec::new());
                cells = 0;
                continue;
            }
            let used = str_cells(grapheme);
            if cells > 0 && cells.saturating_add(used) > width {
                lines.push(Vec::new());
                cells = 0;
            }
            lines
                .last_mut()
                .expect("live greedy wrap always owns one row")
                .push(Span::styled(grapheme.to_owned(), span.style));
            cells = cells.saturating_add(used);
        }
    }
    lines
}

/// 将已着色的 Live 内容按终端 cell 宽拆成物理行；有空格时优先整词，
/// 续行仍保留原 span 语义；无空格长 token 继续按 grapheme/cell 硬折。
pub(crate) fn wrap_live_spans(spans: Vec<Span<'static>>, width: u16) -> Vec<Vec<Span<'static>>> {
    let width = width.max(1) as usize;
    let mut lines = vec![Vec::new()];
    let mut cells = 0usize;
    let mut row_has_body = false;
    let mut pending_whitespace = Vec::new();

    for unit in live_wrap_units(spans) {
        match unit {
            LiveWrapUnit::Newline => {
                pending_whitespace.clear();
                lines.push(Vec::new());
                cells = 0;
                row_has_body = false;
            }
            LiveWrapUnit::Whitespace(fragments) => {
                pending_whitespace = fragments;
            }
            LiveWrapUnit::Word(fragments) => {
                let whitespace_cells = live_fragments_cells(&pending_whitespace);
                let word_cells = live_fragments_cells(&fragments);
                let would_overflow = cells
                    .saturating_add(whitespace_cells)
                    .saturating_add(word_cells)
                    > width;
                if row_has_body && would_overflow {
                    lines.push(Vec::new());
                    cells = 0;
                    pending_whitespace.clear();
                } else if !row_has_body && would_overflow {
                    pending_whitespace.clear();
                }
                append_live_fragments(&mut lines, &pending_whitespace, &mut cells, width);
                append_live_fragments(&mut lines, &fragments, &mut cells, width);
                pending_whitespace.clear();
                row_has_body = true;
            }
        }
    }
    lines
}

struct LiveTailWrap {
    rows: Vec<Vec<Span<'static>>>,
    cells: usize,
    row_has_body: bool,
    pending_whitespace: Vec<LiveFragment>,
    width: usize,
    max_rows: usize,
}

impl LiveTailWrap {
    fn new(width: usize, max_rows: usize) -> Self {
        Self {
            rows: vec![Vec::new()],
            cells: 0,
            row_has_body: false,
            pending_whitespace: Vec::new(),
            width,
            max_rows,
        }
    }

    fn previous_row(&mut self) -> bool {
        if self.rows.len() == self.max_rows {
            return false;
        }
        self.rows.push(Vec::new());
        self.cells = 0;
        self.row_has_body = false;
        true
    }

    fn append_reverse_fragments(&mut self, fragments: &[LiveFragment]) -> bool {
        for fragment in fragments {
            if self.cells > 0
                && self.cells.saturating_add(fragment.cells) > self.width
                && !self.previous_row()
            {
                return false;
            }
            self.rows
                .last_mut()
                .expect("live tail wrap always owns one row")
                .push(Span::styled(fragment.text.clone(), fragment.style));
            self.cells = self.cells.saturating_add(fragment.cells);
        }
        true
    }

    fn whitespace(&mut self, fragments: Vec<LiveFragment>) {
        self.pending_whitespace = fragments;
    }

    fn word(&mut self, fragments: Vec<LiveFragment>) -> bool {
        let whitespace_cells = live_fragments_cells(&self.pending_whitespace);
        let word_cells = live_fragments_cells(&fragments);
        let would_overflow = self
            .cells
            .saturating_add(whitespace_cells)
            .saturating_add(word_cells)
            > self.width;
        if self.row_has_body && would_overflow {
            if !self.previous_row() {
                return false;
            }
            self.pending_whitespace.clear();
        } else if !self.row_has_body && would_overflow {
            self.pending_whitespace.clear();
        }

        let pending = std::mem::take(&mut self.pending_whitespace);
        if !self.append_reverse_fragments(&pending) {
            return false;
        }
        if !self.append_reverse_fragments(&fragments) {
            return false;
        }
        self.row_has_body = true;
        true
    }

    fn finish(mut self) -> Vec<Vec<Span<'static>>> {
        for row in &mut self.rows {
            row.reverse();
        }
        self.rows.reverse();
        self.rows
    }
}

/// Wrap only the newest physical rows of one logical line.
/// Long unbroken model output must not be rescanned/materialized in full on
/// every token frame; the live viewport can only show this bounded tail.
pub(crate) fn wrap_live_spans_tail(
    spans: Vec<Span<'static>>,
    width: u16,
    max_rows: usize,
) -> Vec<Vec<Span<'static>>> {
    let width = width.max(1) as usize;
    let max_rows = max_rows.max(1);
    if spans.iter().any(|span| span.content.contains('\n')) {
        let rows = wrap_live_spans(spans, width as u16);
        let skip = rows.len().saturating_sub(max_rows);
        return rows.into_iter().skip(skip).collect();
    }
    let total_cells = spans
        .iter()
        .map(|span| str_cells(span.content.as_ref()))
        .sum::<usize>();
    if total_cells <= width.saturating_mul(max_rows) {
        let rows = wrap_live_spans(spans.clone(), width as u16);
        if rows.len() <= max_rows {
            return rows;
        }
        // A bounded synthetic tail (notably the leading `…` marker) must stay
        // observable even when word-preferred breaks would waste a row.
        return wrap_live_spans_greedy(spans, width as u16);
    }
    let has_whitespace = spans.iter().any(|span| {
        span.content
            .as_ref()
            .graphemes(true)
            .any(|grapheme| grapheme.chars().all(char::is_whitespace))
    });
    if !has_whitespace {
        // Reverse traversal needs the first row's remainder to reproduce the
        // forward hard-wrap for an unbroken token (e.g. 16 cells at width 14
        // => 14 + 2, not 2 + 14).
        let mut row_limit = if total_cells > width && total_cells % width != 0 {
            total_cells % width
        } else {
            width
        };
        let mut rows = vec![Vec::new()];
        let mut cells = 0usize;

        'spans: for span in spans.iter().rev() {
            for grapheme in span.content.as_ref().graphemes(true).rev() {
                let used = str_cells(grapheme);
                if cells > 0 && cells.saturating_add(used) > row_limit {
                    if rows.len() == max_rows {
                        break 'spans;
                    }
                    rows.push(Vec::new());
                    cells = 0;
                    row_limit = width;
                }
                rows.last_mut()
                    .expect("live tail wrap always owns one row")
                    .push(Span::styled(grapheme.to_owned(), span.style));
                cells = cells.saturating_add(used);
            }
        }
        for row in &mut rows {
            row.reverse();
        }
        rows.reverse();
        return rows;
    }

    let mut wrap = LiveTailWrap::new(width, max_rows);
    let mut kind = None;
    let mut fragments = Vec::new();
    'spans: for span in spans.iter().rev() {
        for grapheme in span.content.as_ref().graphemes(true).rev() {
            let whitespace = grapheme.chars().all(char::is_whitespace);
            if kind.is_some_and(|current| current != whitespace) {
                let previous = kind.take().expect("live tail unit kind exists");
                let chunk = std::mem::take(&mut fragments);
                let keep = if previous {
                    wrap.whitespace(chunk);
                    true
                } else {
                    wrap.word(chunk)
                };
                if !keep {
                    break 'spans;
                }
            }
            kind = Some(whitespace);
            fragments.push(LiveFragment {
                text: grapheme.to_owned(),
                style: span.style,
                cells: str_cells(grapheme),
            });
        }
    }
    if let Some(kind) = kind {
        let chunk = std::mem::take(&mut fragments);
        if kind {
            wrap.whitespace(chunk);
        } else {
            wrap.word(chunk);
        }
    }
    wrap.finish()
}

fn live_continuation_rail(rail: &str) -> &'static str {
    match rail {
        "┃" => "┃",
        "▌" => "▌",
        "┆" => "┆",
        "┊" => "┊",
        "╰" => "┃",
        _ => "│",
    }
}

/// Close only an observable semantic transition; glyph width remains one cell
/// and the existing rail role supplies the lifecycle/focus color.
fn live_block_end_rail(rail: &'static str) -> &'static str {
    match rail {
        "\u{2502}" | "\u{2503}" | "\u{258c}" | "\u{2506}" => "\u{2514}",
        _ => rail,
    }
}

/// Keep live Reasoning legible in narrow terminals: italic still separates
/// thought from Answer, while DIM is reserved for roomier context chrome.
fn live_reasoning_chrome_modifier(width: u16) -> Modifier {
    let mut modifier = Modifier::ITALIC;
    if width > 40 {
        modifier |= Modifier::DIM;
    }
    modifier
}

/// 单行 Live 投影：集中处理 Answer/Reasoning/Tool 的 rail、marker、badge 与宽度预算。
/// 只消费真实 `LiveLine` 与既有体征，不拥有任务状态或输入语义。
#[cfg(test)]
fn render_live_tail_lines(
    transcript: &LiveTranscript,
    width: u16,
    max_rows: usize,
    busy: bool,
) -> Vec<Line<'static>> {
    render_live_tail_projection(transcript, width, max_rows, busy, None).lines
}

fn render_live_tail_projection(
    transcript: &LiveTranscript,
    width: u16,
    max_rows: usize,
    busy: bool,
    preferred_anchor: Option<LiveLineAnchor>,
) -> RenderedLiveTail {
    if max_rows == 0 {
        return RenderedLiveTail {
            lines: Vec::new(),
            anchor: None,
        };
    }
    let visible_lines = transcript.visible_lines(max_rows);
    let anchor_start = preferred_anchor.and_then(|anchor| {
        visible_lines
            .iter()
            .position(|line| line.anchor == Some(anchor))
    });
    let anchored = anchor_start.is_some();
    let visible_lines = if let Some(start) = anchor_start {
        visible_lines.into_iter().skip(start).collect::<Vec<_>>()
    } else {
        visible_lines
    };
    let semantic_alert_edges = alert_edges(visible_lines.iter().map(|line| line.text));
    let last_visible_line = visible_lines.len().saturating_sub(1);
    // Close only a boundary that is observable in this bounded projection.
    // The final tail row is intentionally left open: its successor may be
    // outside the viewport, so inventing a closure would misstate continuity.
    let block_ends = visible_lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let Some(current) = line.anchor.map(|anchor| anchor.focus) else {
                return false;
            };
            let Some(next) = visible_lines
                .get(index + 1)
                .and_then(|line| line.anchor.map(|anchor| anchor.focus))
            else {
                return false;
            };
            current != next
        })
        .collect::<Vec<_>>();
    // Audit focus is a render projection over existing anchored rows. It must
    // not become a second transcript or alter ordinary Follow rendering.
    let focused_block = transcript
        .is_inspecting()
        .then(|| transcript.focused_block())
        .flatten();
    let mut row_state = LiveRowState {
        focused_block,
        max_rows: max_rows.max(1),
        ..LiveRowState::default()
    };
    // The transcript already chooses a bounded logical window.  Keep a second
    // physical bound here: one logical line may wrap into many rows, and
    // collecting every wrapped row turns a narrow viewport into O(k²) work.
    // The queue retains only rows that can reach this frame; semantic state is
    // still advanced for every selected logical line so rails and code fences
    // remain correct at the physical tail.
    let mut physical_tail = VecDeque::with_capacity(max_rows);
    let mut stop_at_viewport = false;
    for (index, (line, alert_edge)) in visible_lines
        .into_iter()
        .zip(semantic_alert_edges)
        .enumerate()
    {
        row_state.block_end = block_ends.get(index).copied().unwrap_or(false);
        let line_anchor = line.anchor;
        let line_width = if busy && index == last_visible_line {
            width.saturating_sub(1)
        } else {
            width
        };
        for rendered in render_live_line_with_alert_edge(
            line,
            index,
            last_visible_line,
            line_width,
            busy,
            alert_edge,
            &mut row_state,
        ) {
            if anchored {
                if physical_tail.len() == max_rows {
                    stop_at_viewport = true;
                    break;
                }
                physical_tail.push_back((line_anchor, rendered));
            } else {
                if physical_tail.len() == max_rows {
                    physical_tail.pop_front();
                }
                physical_tail.push_back((line_anchor, rendered));
            }
        }
        if stop_at_viewport {
            break;
        }
    }
    let anchor = physical_tail.front().and_then(|(anchor, _)| *anchor);
    RenderedLiveTail {
        lines: physical_tail.into_iter().map(|(_, line)| line).collect(),
        anchor,
    }
}

#[cfg(test)]
fn render_live_line<'a>(
    line: LiveLine<'a>,
    index: usize,
    last_visible_line: usize,
    width: u16,
    busy: bool,
    state: &mut LiveRowState,
) -> Vec<Line<'static>> {
    render_live_line_with_alert_edge(line, index, last_visible_line, width, busy, None, state)
}

fn render_live_line_with_alert_edge<'a>(
    line: LiveLine<'a>,
    index: usize,
    last_visible_line: usize,
    width: u16,
    busy: bool,
    alert_edge: Option<AlertEdge>,
    state: &mut LiveRowState,
) -> Vec<Line<'static>> {
    if line.kind != LiveLineKind::Answer {
        state.alert_role = None;
    }
    if line.kind == LiveLineKind::Answer {
        // The opener may be above the bounded visible tail; use the
        // transcript's render-only context instead of guessing false.
        state.code_before = line.fence_before;
    }
    // Focus marker exists on the summary only; propagate it across the
    // contiguous visible detail tail without adding block identity to
    // the render model.
    let focused_tool = line.marker == Some("▸ ")
        || (line.kind == LiveLineKind::ToolDetail && state.previous_focused_tool);
    let focused = state
        .focused_block
        .is_some_and(|focus| line.anchor.is_some_and(|anchor| anchor.focus == focus));
    let focus_context = state.focused_block.is_some() && !focused;
    let previous_kind = state.previous_kind;
    let reasoning_role = active_reasoning_tail_role(line.kind, busy, index == last_visible_line);
    let code_before = line.kind == LiveLineKind::Answer && state.code_before;
    let fence_line = line.kind == LiveLineKind::Answer && line.text.trim_start().starts_with("```");
    let fence_label = (!code_before && fence_line)
        .then(|| fence_language(line.text))
        .flatten();
    let continuation_rail = (line.kind == LiveLineKind::Reasoning && line.continuation_before)
        .then_some(("┊", reasoning_role.unwrap_or(Role::Reasoning)));
    let rail = continuation_rail
        .or_else(|| live_code_rail(code_before, fence_line))
        .or_else(|| live_rail(line.kind, focused_tool, previous_kind))
        .map(|(rail, role)| {
            let role = reasoning_role.unwrap_or(role);
            (rail, live_tool_rail_role(line.kind, line.color, role))
        });
    let rail = if state.block_end && alert_edge.is_none() && !code_before && !fence_line {
        rail.map(|(rail, role)| (live_block_end_rail(rail), role))
    } else {
        rail
    };
    let continuation_rail = rail.map(|(rail, role)| (live_continuation_rail(rail), role));
    state.previous_focused_tool = match line.kind {
        LiveLineKind::ToolSummary => line.marker == Some("▸ "),
        LiveLineKind::ToolDetail => focused_tool,
        _ => false,
    };
    state.previous_kind = Some(line.kind);
    let base_chrome_modifier = match line.kind {
        LiveLineKind::Answer => Modifier::BOLD,
        LiveLineKind::Reasoning => live_reasoning_chrome_modifier(width),
        LiveLineKind::ToolSummary => Modifier::BOLD,
        LiveLineKind::ToolDetail => Modifier::DIM,
        LiveLineKind::Splash => Modifier::BOLD,
    };
    let chrome_modifier = if focused {
        // A selected Reasoning/detail row sheds DIM; BOLD is the only added
        // focus signal, keeping geometry and ANSI16 colors unchanged.
        match line.kind {
            LiveLineKind::Reasoning | LiveLineKind::ToolDetail => Modifier::BOLD,
            _ => base_chrome_modifier | Modifier::BOLD,
        }
    } else if focus_context {
        base_chrome_modifier | Modifier::DIM
    } else {
        base_chrome_modifier
    };
    let base_body_modifier = match line.kind {
        // Keep the channel marker/rail bold, but let long prose breathe. The
        // Markdown renderer still applies heading/inline emphasis itself.
        LiveLineKind::Answer => Modifier::empty(),
        // Details are intentionally quiet while collapsed/unfocused, but an
        // explicitly focused Ctrl+O surface must be readable without another
        // visual toggle or a second rendering path.
        LiveLineKind::ToolDetail if focused_tool => Modifier::empty(),
        _ => chrome_modifier,
    };
    let body_modifier = if focused {
        base_body_modifier | Modifier::BOLD
    } else if focus_context {
        base_body_modifier | Modifier::DIM
    } else {
        base_body_modifier
    };
    // Per-line step/time/token metadata changed while the model was idle and
    // duplicated the authoritative top/bottom telemetry. Keep the semantic
    // channel marker stable so the live layout cache survives telemetry ticks;
    // actual reasoning text remains fully visible and inspectable.
    let marker: Option<Cow<'static, str>> = line.marker.map(Cow::Borrowed);
    let marker =
        live_phase_marker(line.kind, marker.as_deref(), previous_kind, width).map(Cow::Owned);
    let rail_cells = rail.map(|(rail, _)| str_cells(rail)).unwrap_or_default();
    let marker = marker.and_then(|marker| fit_live_marker(marker, line.kind, width, rail_cells));
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(3);
    if let Some((rail, rail_role)) = rail {
        spans.push(Span::styled(
            rail,
            Style::default()
                .fg(role_color(rail_role))
                .add_modifier(chrome_modifier),
        ));
    }
    let marker_cells = marker.as_deref().map(str_cells).unwrap_or_default();
    if let Some(marker) = marker {
        let marker_role = match line.kind {
            LiveLineKind::Answer => Role::Primary,
            LiveLineKind::Reasoning => reasoning_role.unwrap_or(Role::Reasoning),
            _ => Role::Info,
        };
        spans.push(Span::styled(
            marker,
            Style::default()
                .fg(role_color(marker_role))
                .add_modifier(chrome_modifier),
        ));
    }
    let base_prefix_cells = rail_cells + marker_cells;
    let badge = fence_label.and_then(|language| {
        let badge = format!("‹{language}› ");
        let badge_cells = str_cells(&badge);
        (width.saturating_sub(base_prefix_cells as u16) >= badge_cells.saturating_add(4) as u16)
            .then_some(badge)
    });
    let has_badge = badge.is_some();
    let badge_cells = badge.as_deref().map(str_cells).unwrap_or_default();
    if let Some(badge) = badge {
        spans.push(Span::styled(
            badge,
            Style::default()
                .fg(role_color(Role::Info))
                .add_modifier(Modifier::BOLD),
        ));
    }
    let prefix_cells = base_prefix_cells + badge_cells;
    let prefix_cells = prefix_cells.min(width as usize) as u16;
    let text_width = width.saturating_sub(prefix_cells);
    let content = if line.kind == LiveLineKind::Answer {
        let display_text = has_badge.then(|| fence_without_language(line.text));
        let text = display_text.as_deref().unwrap_or(line.text);
        // Keep syntax highlighting itself viewport-bounded, not only the final
        // physical wrap.  This prevents a long Markdown line from paying a
        // full inline-span scan when only a few tail rows can be seen. Alert
        // openers stay intact so their semantic label remains truthful.
        let highlight_budget = (text_width.max(1) as usize)
            .saturating_mul(state.max_rows.max(1))
            .saturating_mul(4);
        let bounded = if alert_edge.is_none() && text.len() > highlight_budget {
            Cow::Owned(tail_display_cells(text, text_width, state.max_rows))
        } else {
            Cow::Borrowed(text)
        };
        live_markdown_spans_with_alert_edge(
            bounded.as_ref(),
            &mut state.code_before,
            line.color,
            body_modifier,
            &mut state.alert_role,
            alert_edge,
        )
    } else {
        // The collapsed hint is optional detail affordance. At narrow widths
        // its full label can wrap into two physical rows and push the tool
        // summary out of the viewport's physical tail; keep the affordance
        // single-row so the summary remains observable.
        let display_text = if line.kind == LiveLineKind::ToolDetail
            && line.text.trim_start().starts_with("[Ctrl+O details")
            && str_cells(line.text) > text_width as usize
        {
            "[Ctrl+O]"
        } else {
            line.text
        };
        let display_text = tail_display_cells(display_text, text_width, state.max_rows);
        let body_color = if line.kind == LiveLineKind::Reasoning {
            role_color(reasoning_role.unwrap_or(Role::Reasoning))
        } else {
            line.color
        };
        vec![Span::styled(
            display_text,
            Style::default().fg(body_color).add_modifier(body_modifier),
        )]
    };
    let rows = wrap_live_spans_tail(content, text_width, state.max_rows);
    rows.into_iter()
        .enumerate()
        .map(|(row, body)| {
            let mut line_spans = if row == 0 {
                spans.clone()
            } else if prefix_cells == 0 {
                Vec::new()
            } else {
                let mut prefix = Vec::with_capacity(2);
                if let Some((rail, role)) = continuation_rail {
                    prefix.push(Span::styled(
                        rail,
                        Style::default()
                            .fg(role_color(role))
                            .add_modifier(chrome_modifier),
                    ));
                    let rail_cells = str_cells(rail) as u16;
                    let spaces = prefix_cells.saturating_sub(rail_cells);
                    if spaces > 0 {
                        prefix.push(Span::raw(" ".repeat(spaces as usize)));
                    }
                    prefix
                } else {
                    vec![Span::raw(" ".repeat(prefix_cells as usize))]
                }
            };
            line_spans.extend(body);
            Line::from(line_spans)
        })
        .collect()
}

/// Bounded presentation rail for the top chrome.  It owns only display-cell
/// accounting; semantic priority remains explicit at the call site.
struct ChromeRail {
    width: usize,
    used: usize,
    spans: Vec<Span<'static>>,
}

impl ChromeRail {
    fn new(width: u16) -> Self {
        Self {
            width: width as usize,
            used: 0,
            spans: Vec::new(),
        }
    }

    fn remaining(&self) -> usize {
        self.width.saturating_sub(self.used)
    }

    fn push_fit(&mut self, text: &'static str, style: Style) -> bool {
        self.push_fit_with_budget(text, style, self.remaining())
    }

    fn push_fit_with_budget(&mut self, text: &'static str, style: Style, budget: usize) -> bool {
        let cells = str_cells(text);
        if cells == 0 || cells > self.remaining().min(budget) {
            return false;
        }
        self.used += cells;
        self.spans.push(Span::styled(text, style));
        true
    }

    fn push_dynamic_fit(&mut self, text: String, style: Style) -> bool {
        let cells = str_cells(&text);
        if cells == 0 || cells > self.remaining() {
            return false;
        }
        self.used += cells;
        self.spans.push(Span::styled(text, style));
        true
    }

    fn push_clipped(&mut self, text: &str, style: Style) {
        let remaining = self.remaining();
        if remaining == 0 {
            return;
        }
        let clipped = clip_display_cells(text, remaining as u16);
        self.used += str_cells(&clipped);
        self.spans.push(Span::styled(clipped, style));
    }

    fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    fn finish(self) -> Line<'static> {
        Line::from(self.spans)
    }
}

fn focused_tool_chip(summary: &str, remaining: usize) -> Option<String> {
    if remaining < 10 {
        return None;
    }
    let summary = summary.lines().next().unwrap_or("").trim();
    if summary.is_empty() {
        return None;
    }
    let label_width = remaining.min(28).saturating_sub(4) as u16;
    let label = clip_display_cells(summary, label_width);
    let chip = format!(" ◈ {label} ");
    (str_cells(&chip) <= remaining).then_some(chip)
}

fn focused_block_chip(focus: LiveBlockFocus, remaining: usize) -> Option<(String, Role)> {
    if remaining < 11 {
        return None;
    }
    let (label, role) = match focus {
        LiveBlockFocus::Answer(_) => ("ANSWER", Role::Primary),
        LiveBlockFocus::Reasoning(_) => ("THINK", Role::Reasoning),
        LiveBlockFocus::Tool(_) => return None,
    };
    let chip = format!(" ◉ {label} ");
    (str_cells(&chip) <= remaining).then_some((chip, role))
}

fn reasoning_visibility_chip(expanded: bool, remaining: usize) -> Option<String> {
    if remaining < 10 {
        return None;
    }
    let full = if expanded {
        " ◇ THINKING · Ctrl+R collapse "
    } else {
        " ◇ THINKING · Ctrl+R reasoning "
    };
    if str_cells(full) <= remaining {
        return Some(full.to_owned());
    }
    let compact = if expanded {
        " ◇ THINK+ "
    } else {
        " ◇ THINK "
    };
    (str_cells(compact) <= remaining).then_some(compact.to_owned())
}

fn live_inspection_chip(remaining: usize, has_semantic_block: bool) -> Option<String> {
    if remaining < 10 {
        return None;
    }
    let full = if has_semantic_block {
        " ◇ HOLD · Alt+←/→ focus · Ctrl+Space follow · Space toggle · Alt+End "
    } else {
        " ◇ HOLD · Ctrl+Space follow · Alt+End "
    };
    if str_cells(full) <= remaining {
        return Some(full.to_owned());
    }
    let compact = " ◇ HOLD ";
    (str_cells(compact) <= remaining).then_some(compact.to_owned())
}

fn push_channel_badge(rail: &mut ChromeRail, width: usize, channel: LiveChannel) {
    let (label, role) = stream_channel_badge(channel);
    let style = Style::default()
        .fg(role_color(role))
        .add_modifier(Modifier::BOLD);
    let full = match label {
        "[ANSWER]" => " [ANSWER] ",
        "[THINK]" => " [THINK] ",
        "[TOOL]" => " [TOOL] ",
        _ => unreachable!("stream badge labels are exhaustive"),
    };
    if rail.push_fit_with_budget(full, style, width) {
        return;
    }
    let compact = match channel {
        LiveChannel::Answer => " A ",
        LiveChannel::Reasoning => " T ",
        LiveChannel::Tool => " O ",
    };
    let _ = rail.push_fit_with_budget(compact, style, width);
}

fn waiting_phase(ui: &Ui) -> String {
    format!("waiting · {}", waiting_target(ui))
}

fn waiting_target(ui: &Ui) -> &'static str {
    if ui.pending_call.is_some() {
        "tool"
    } else {
        "model"
    }
}

fn compact_waiting_anchor(target: &str, width: u16) -> String {
    let full = format!("┊ HOLD · waiting · {target}");
    if str_cells(&full) <= width as usize {
        return full;
    }
    let compact = format!("HOLD waiting:{target}");
    if str_cells(&compact) <= width as usize {
        return compact;
    }
    let minimal = format!("HOLD · {target}");
    if str_cells(&minimal) <= width as usize {
        return minimal;
    }
    clip_display_cells(&minimal, width)
}

fn compact_busy_phase(ui: &Ui) -> String {
    let phase = if ui.waiting {
        waiting_phase(ui)
    } else {
        let activity = if ui.activity.is_empty() {
            ui.phase.as_str()
        } else {
            ui.activity.as_str()
        };
        activity
            .rsplit('·')
            .next()
            .unwrap_or(activity)
            .trim()
            .to_owned()
    };
    if ui.transcript.is_inspecting() {
        // Put the safety state first: narrow chrome clips from the right, so
        // a trailing HOLD marker disappears exactly when intervention matters.
        format!("HOLD · {phase}")
    } else {
        phase
    }
}

fn compact_idle_status(ui: &Ui) -> String {
    let Some(entry) = ui.activity_history.back() else {
        return " ready".to_owned();
    };
    if entry.kind == ActivityKind::System {
        return " ready".to_owned();
    }
    let label = entry
        .text
        .rsplit('·')
        .next()
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .unwrap_or("ready");
    format!(" {} {label}", entry.kind.tag())
}

/// Keep the observed phase inside the output viewport while the user audits
/// older live rows.  The normal chrome remains below the viewport; this
/// bounded anchor prevents HOLD from looking like a detached static log.
pub(crate) fn live_phase_anchor(ui: &Ui, vitals: &Vitals, width: u16) -> Option<Line<'static>> {
    if !ui.transcript.is_inspecting() || width == 0 {
        return None;
    }
    let waiting = ui.waiting.then(|| waiting_phase(ui));
    let fallback = if ui.activity.is_empty() {
        ui.phase.as_str()
    } else {
        ui.activity.as_str()
    };
    let phase = waiting.as_deref().unwrap_or(fallback);
    let step = (vitals.step > 0).then(|| format!(" · step {}", vitals.step));
    let trace = phase_trace_text(&ui.transcript, width)
        .map(|trace| format!(" · {trace}"))
        .unwrap_or_default();
    let lifecycle = live_lifecycle_badge(ui)
        .map(|(tag, _)| format!(" · {tag}"))
        .unwrap_or_default();
    let full = format!(
        "┊ HOLD{trace}{lifecycle} · {}{}",
        phase.trim(),
        step.unwrap_or_default()
    );
    let text = if ui.waiting && str_cells(&full) > width as usize {
        compact_waiting_anchor(waiting_target(ui), width)
    } else {
        clip_display_cells(&full, width)
    };
    (!text.trim().is_empty()).then(|| {
        Line::from(Span::styled(
            text,
            Style::default()
                .fg(role_color(Role::Info))
                .add_modifier(Modifier::DIM),
        ))
    })
}

/// Keep non-channel lifecycle transitions visible beside an existing stream.
///
/// The top chrome already reports the current phase, but native scrollback and
/// a long live tail can make that one-row status easy to lose.  This anchor is
/// deliberately limited to actionable lifecycle signals; ordinary Answer,
/// Reasoning, and Tool rows keep their existing rails and are not duplicated.
fn live_lifecycle_anchor(ui: &Ui, vitals: &Vitals, width: u16) -> Option<Line<'static>> {
    if !ui.busy || ui.transcript.is_inspecting() || width == 0 {
        return None;
    }
    let entry = ui.activity_history.back()?;
    let (tag, role) = match entry.kind {
        ActivityKind::Verification => ("CHK", Role::Info),
        ActivityKind::Waiting => ("WAIT", Role::Warn),
        ActivityKind::Approval => ("ASK", Role::Warn),
        ActivityKind::Conclusion => ("SUM", Role::Success),
        ActivityKind::Error => ("ERR", Role::Error),
        _ => return None,
    };
    let detail = entry
        .text
        .split_once('·')
        .map(|(_, detail)| detail.trim())
        .filter(|detail| !detail.is_empty())
        .unwrap_or(entry.text.as_str());
    let step = (vitals.step > 0).then(|| format!(" · step {}", vitals.step));
    let hint = if width >= 28 {
        " · Ctrl+T activity"
    } else {
        ""
    };
    let text = format!(
        "┊ {tag} · {}{}{hint}",
        sanitize_display_text(detail),
        step.unwrap_or_default()
    );
    Some(Line::from(Span::styled(
        clip_display_cells(&text, width),
        Style::default()
            .fg(role_color(role))
            .add_modifier(Modifier::DIM | Modifier::BOLD),
    )))
}

fn live_surface_state(ui: &Ui) -> (Role, &'static str) {
    if ui.waiting {
        return (Role::Warn, "WAIT");
    }
    // A streamed Answer/Reasoning/Tool block is the freshest observable phase.
    // Retained lifecycle entries may describe the preceding node and must not
    // hide the model text that is currently arriving.
    if ui.busy {
        if let Some(channel) = ui.transcript.active_channel() {
            return live_channel_state(channel);
        }
    }
    if let Some(entry) = ui.activity_history.back() {
        let state = match entry.kind {
            ActivityKind::Error => Some((Role::Error, "ERR")),
            ActivityKind::Approval => Some((Role::Warn, "ASK")),
            ActivityKind::Conclusion => Some((Role::Success, "SUM")),
            ActivityKind::Completed => Some((Role::Success, "DONE")),
            ActivityKind::Takeover => Some((Role::Primary, "TAKE")),
            ActivityKind::Verification => Some((Role::Info, "CHK")),
            ActivityKind::Reasoning => Some((Role::Reasoning, "THK")),
            ActivityKind::Answer => Some((Role::Primary, "ANS")),
            ActivityKind::Tool => Some((Role::Info, "TLS")),
            _ => None,
        };
        if let Some(state) = state {
            return state;
        }
    }
    if !ui.busy {
        return (Role::Border, "READY");
    }
    match ui.transcript.active_channel() {
        Some(channel) => live_channel_state(channel),
        None => (Role::Primary, "LIVE"),
    }
}

fn live_channel_state(channel: LiveChannel) -> (Role, &'static str) {
    match channel {
        LiveChannel::Reasoning => (Role::Reasoning, "THK"),
        LiveChannel::Answer => (Role::Primary, "ANS"),
        LiveChannel::Tool => (Role::Info, "TLS"),
    }
}

/// Wide live chrome exposes every channel present in the bounded transcript,
/// not only the newest one.  This keeps a streamed Answer visibly related to
/// the preceding reasoning/tool work without duplicating any body text.
fn live_channel_tags(ui: &Ui) -> Vec<&'static str> {
    let mut channels = Vec::with_capacity(3);
    if ui.transcript.has_reasoning() {
        channels.push("THK");
    }
    if ui.transcript.has_answer() {
        channels.push("ANS");
    }
    if ui.has_live_tools() {
        channels.push("TLS");
    }
    channels
}

fn live_channel_role(tag: &str) -> Role {
    match tag {
        "THK" => Role::Reasoning,
        "ANS" => Role::Primary,
        "TLS" => Role::Info,
        _ => Role::Muted,
    }
}

/// Keep an actionable lifecycle signal beside a live channel. The current
/// channel remains the primary state; this badge prevents a preceding
/// verification/conclusion/error from disappearing when the user inspects the
/// stream without adding a row or changing execution focus.
fn live_lifecycle_badge(ui: &Ui) -> Option<(&'static str, Role)> {
    if !ui.busy || ui.transcript.active_channel().is_none() {
        return None;
    }
    match ui.activity_history.back()?.kind {
        ActivityKind::Verification => Some(("CHK", Role::Info)),
        ActivityKind::Conclusion => Some(("SUM", Role::Success)),
        ActivityKind::Approval => Some(("ASK", Role::Warn)),
        ActivityKind::Error => Some(("ERR", Role::Error)),
        ActivityKind::Waiting
        | ActivityKind::System
        | ActivityKind::Plan
        | ActivityKind::Reasoning
        | ActivityKind::Answer
        | ActivityKind::Tool
        | ActivityKind::Queue
        | ActivityKind::Takeover
        | ActivityKind::Completed => None,
    }
}

fn live_surface_title_line(ui: &Ui, width: u16) -> Line<'static> {
    let (state_role, tag) = live_surface_state(ui);
    let full = width >= 40;
    let compact = width >= 18;
    if !compact {
        return Line::default();
    }

    let mut spans = Vec::with_capacity(8);
    if width >= 32 {
        spans.push(Span::styled(
            " LIVE · ",
            Style::default().fg(role_color(Role::Label)),
        ));
    }
    spans.push(Span::styled(
        tag,
        Style::default()
            .fg(role_color(state_role))
            .add_modifier(Modifier::BOLD),
    ));
    if full {
        if let Some((badge, badge_role)) = live_lifecycle_badge(ui) {
            spans.push(Span::styled(
                " · ",
                Style::default().fg(role_color(Role::Border)),
            ));
            spans.push(Span::styled(
                badge,
                Style::default()
                    .fg(role_color(badge_role))
                    .add_modifier(Modifier::BOLD),
            ));
        }
        let channels = live_channel_tags(ui);
        if channels.len() > 1 {
            spans.push(Span::styled(
                " · ",
                Style::default().fg(role_color(Role::Border)),
            ));
            for (index, channel) in channels.into_iter().enumerate() {
                if index > 0 {
                    spans.push(Span::styled(
                        "/",
                        Style::default().fg(role_color(Role::Border)),
                    ));
                }
                spans.push(Span::styled(
                    channel,
                    Style::default()
                        .fg(role_color(live_channel_role(channel)))
                        .add_modifier(Modifier::BOLD),
                ));
            }
        }
        if ui.transcript.is_inspecting() {
            spans.push(Span::styled(
                " · HOLD",
                Style::default()
                    .fg(role_color(Role::Warn))
                    .add_modifier(Modifier::BOLD),
            ));
        }
    }
    Line::from(spans)
}

#[cfg(test)]
pub(crate) fn live_surface_title(ui: &Ui, width: u16) -> String {
    let mut text = live_surface_title_line(ui, width)
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    if width >= 32 && !text.is_empty() {
        text.push(' ');
    } else if width >= 18 && !text.is_empty() {
        text.insert(0, ' ');
        text.push(' ');
    }
    clip_display_cells(&text, width)
}

fn live_surface_block(ui: &Ui, area: Rect) -> Option<Block<'static>> {
    // Idle output already has its own semantic rails and must keep the full
    // text width for native scrollback/fenced content.  Spend frame budget on
    // the active turn, where the state signal is most valuable.
    if !ui.busy || area.width < 18 || area.height < 3 {
        return None;
    }
    let (role, _) = live_surface_state(ui);
    let full = area.width >= 40 && area.height >= 5;
    let borders = if full {
        Borders::ALL
    } else {
        Borders::LEFT | Borders::RIGHT
    };
    let mut block = Block::default()
        .borders(borders)
        .border_style(Style::default().fg(role_color(role)));
    if full {
        block = block
            .border_type(BorderType::Rounded)
            .title(live_surface_title_line(ui, area.width))
            .title_style(
                Style::default()
                    .fg(role_color(role))
                    .add_modifier(Modifier::BOLD),
            );
    }
    Some(block)
}

fn queue_front_chip(queued: usize, remaining: usize) -> Option<String> {
    if queued == 0 || remaining < 4 {
        return None;
    }
    let full = format!(" ⏭{queued} ");
    if str_cells(&full) <= remaining {
        return Some(full);
    }
    let compact = format!(" ⏭{queued}");
    (str_cells(&compact) <= remaining).then_some(compact)
}

fn activity_age_label(ui: &Ui) -> String {
    let Some(started) = ui.activity_started else {
        return String::new();
    };
    let elapsed_ms = started.elapsed().as_millis();
    if elapsed_ms < 1_000 {
        format!(" · +{elapsed_ms}ms")
    } else if elapsed_ms < 60_000 {
        format!(" · +{}s", elapsed_ms / 1_000)
    } else {
        format!(" · +{}m", elapsed_ms / 60_000)
    }
}

/// Pure status projection consumed by the top rail.  Keeping status wording
/// separate from chip placement makes the visual rail replaceable without
/// letting theme/layout code infer execution state.
struct TopStatus {
    text: String,
    role: Role,
    bold: bool,
}

fn top_status(ui: &Ui, vitals: &Vitals, width: usize) -> TopStatus {
    let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"][ui.frame % 10];
    let status_text = if ui.busy {
        let busy_phase = if ui.waiting {
            let target = waiting_target(ui);
            if vitals.step > 0 {
                format!("waiting · {target} · step {}", vitals.step)
            } else {
                format!("waiting · {target}")
            }
        } else {
            let activity = if ui.activity.is_empty() {
                ui.phase.as_str()
            } else {
                ui.activity.as_str()
            };
            fmt_busy_phase(activity, vitals.step).into_owned()
        };
        format!(
            " {spinner} {}",
            fmt_busy_signal(
                &busy_phase,
                &ui.todos,
                vitals.elapsed_s,
                vitals.rate,
                vitals.queued,
                ui.pending_call.as_ref(),
            )
        )
    } else {
        let todo = todo_progress(&ui.todos)
            .map(|(done, total)| format!(" · todo {done}/{total}"))
            .unwrap_or_default();
        let outcome = ui
            .activity_history
            .back()
            .filter(|entry| {
                matches!(
                    entry.kind,
                    ActivityKind::Takeover
                        | ActivityKind::Completed
                        | ActivityKind::Error
                        | ActivityKind::Approval
                        | ActivityKind::Verification
                        | ActivityKind::Conclusion
                )
            })
            .map(|entry| format!(" · {}", entry.text))
            .unwrap_or_default();
        format!(" ready{outcome}{todo}")
    };
    let status_text = if !ui.busy && width < 64 {
        // Keep the semantic activity tag ahead of the outcome.  Otherwise a
        // 32-cell frame shows `ready · takeover re…` while the ledger still
        // knows the complete, actionable `takeover ready` boundary.
        compact_idle_status(ui)
    } else {
        status_text
    };
    TopStatus {
        text: status_text,
        role: if ui.busy { Role::Primary } else { Role::Info },
        bold: ui.busy,
    }
}

/// 顶部 chrome 的唯一投影：品牌、越狱警示、真实流通道与 busy/ready 状态在此组装。
/// 宽度不足时优先保留安全/通道 badge，再裁剪次要 phase 文案，避免窄端只剩无意义省略号。
/// 不拥有任务状态；只消费 `Ui`/`Vitals` 的既有事实，令主布局函数只负责槽位与覆盖层。
pub(crate) fn top_chrome(ui: &Ui, vitals: &Vitals, area_width: u16) -> Line<'static> {
    let width = area_width as usize;
    let status = top_status(ui, vitals, width);
    let status_style = Style::default()
        .fg(role_color(status.role))
        .add_modifier(if status.bold {
            Modifier::BOLD
        } else {
            Modifier::empty()
        });
    let mut above = ChromeRail::new(area_width);
    // On narrow busy frames, keep the full/compact channel badge and a compact
    // live phase together when they fit. The bottom bar carries telemetry; do
    // not let long usage text turn a critical phase into an ellipsis.
    if ui.busy && width < 48 {
        if agent::allow_jailbreak() {
            let style = Style::default()
                .fg(Color::Black)
                .bg(role_color(Role::Error))
                .add_modifier(Modifier::BOLD);
            let _ = above.push_fit(" [JAIL] ", style);
        }
        let phase = compact_busy_phase(ui);
        let phase_chip = format!(" {phase}");
        let queue_chip = queue_front_chip(vitals.queued, above.remaining());
        let queue_cells = queue_chip.as_ref().map(|chip| str_cells(chip)).unwrap_or(0);
        if let Some(channel) = ui.transcript.active_channel() {
            // Reserve phase and front-queue feedback before selecting the
            // channel badge; narrow frames fall back from full to compact.
            let channel_reserve = if queue_cells > 0 && width >= 18 {
                queue_cells + str_cells(&phase_chip)
            } else {
                0
            };
            let channel_width = width.saturating_sub(channel_reserve);
            push_channel_badge(&mut above, channel_width, channel);
        }
        let phase_budget = above.remaining().saturating_sub(queue_cells);
        if phase_budget > 0 {
            let phase = clip_display_cells(&phase_chip, phase_budget as u16);
            let _ = above.push_dynamic_fit(phase, status_style);
        }
        if let Some(queue_chip) = queue_chip {
            let queue_style = Style::default()
                .fg(role_color(Role::Primary))
                .add_modifier(Modifier::BOLD);
            let _ = above.push_dynamic_fit(queue_chip, queue_style);
        }
        if above.is_empty() {
            above.push_clipped(&status.text, status_style);
        }
        return above.finish();
    }
    let wide = width >= 32;
    if wide {
        let brand_style = Style::default()
            .fg(role_color(Role::Primary))
            .add_modifier(Modifier::BOLD);
        let _ = above.push_fit(" RidgeCode ", brand_style);
    }
    if agent::allow_jailbreak() {
        let style = Style::default()
            .fg(Color::Black)
            .bg(role_color(Role::Error))
            .add_modifier(Modifier::BOLD);
        let full = " ⚠JAILBREAK ";
        if !above.push_fit(full, style) {
            let _ = above.push_fit(" [JAIL] ", style);
        }
    }
    if ui.busy {
        if let Some(channel) = ui.transcript.active_channel() {
            push_channel_badge(&mut above, width, channel);
        }
        if width >= 64 {
            if let Some(chip) = phase_trace_chip(&ui.transcript, above.remaining()) {
                let style = Style::default()
                    .fg(role_color(Role::Info))
                    .add_modifier(Modifier::DIM);
                let _ = above.push_dynamic_fit(chip, style);
            }
            if let Some(chip) = activity_breadcrumb(&ui.activity_history, above.remaining()) {
                let style = Style::default()
                    .fg(role_color(Role::Info))
                    .add_modifier(Modifier::DIM);
                let _ = above.push_dynamic_fit(chip, style);
            }
            let age = activity_age_label(ui);
            if !age.is_empty() {
                let style = Style::default()
                    .fg(role_color(Role::Info))
                    .add_modifier(Modifier::DIM);
                let _ = above.push_dynamic_fit(age, style);
            }
        }
    } else if !wide {
        let brand_style = Style::default()
            .fg(role_color(Role::Primary))
            .add_modifier(Modifier::BOLD);
        let _ = above.push_fit(" RDG ", brand_style);
    }
    if ui.busy && (48..64).contains(&width) && !ui.transcript.is_inspecting() {
        if let Some((chip, role)) = activity_signal_chip(ui, above.remaining()) {
            let style = Style::default()
                .fg(role_color(role))
                .add_modifier(Modifier::BOLD);
            let _ = above.push_dynamic_fit(chip, style);
        }
    }
    if ui.transcript.is_inspecting() {
        if let Some(chip) = live_inspection_chip(
            above.remaining(),
            ui.has_live_tools() || ui.transcript.has_reasoning(),
        ) {
            let style = Style::default()
                .fg(role_color(Role::Info))
                .add_modifier(Modifier::BOLD);
            let _ = above.push_dynamic_fit(chip, style);
        }
    }
    if width >= 48 {
        match ui.transcript.focused_block() {
            Some(LiveBlockFocus::Tool(_)) if ui.has_live_tools() => {
                if let Some(summary) = ui.transcript.focused_tool_summary() {
                    if let Some(chip) = focused_tool_chip(summary, above.remaining()) {
                        let style = Style::default()
                            .fg(role_color(Role::Info))
                            .add_modifier(Modifier::BOLD);
                        let _ = above.push_dynamic_fit(chip, style);
                    }
                }
            }
            Some(focus @ (LiveBlockFocus::Answer(_) | LiveBlockFocus::Reasoning(_))) => {
                if let Some((chip, role)) = focused_block_chip(focus, above.remaining()) {
                    let style = Style::default()
                        .fg(role_color(role))
                        .add_modifier(Modifier::BOLD);
                    let _ = above.push_dynamic_fit(chip, style);
                }
            }
            None | Some(LiveBlockFocus::Tool(_)) => {}
        }
    }
    if ui.busy
        && ui.transcript.active_channel() == Some(LiveChannel::Answer)
        && ui.transcript.has_reasoning()
        && width >= 48
    {
        if let Some(chip) =
            reasoning_visibility_chip(ui.transcript.is_reasoning_expanded(), above.remaining())
        {
            let style = Style::default()
                .fg(role_color(Role::Reasoning))
                .add_modifier(Modifier::BOLD);
            let _ = above.push_dynamic_fit(chip, style);
        }
    }
    above.push_clipped(&status.text, status_style);
    above.finish()
}

/// 主 Live 四槽的响应式垂直预算：输出与输入优先，低高时收缩 chrome/底栏。
/// 约束总高永不超过终端高，避免高输入把 Answer 槽挤成不可预测的零行。
pub(crate) fn responsive_live_layout(
    area: Rect,
    requested_input_rows: u16,
    requested_status_rows: u16,
) -> [Rect; 4] {
    let height = area.height;
    let output_floor = u16::from(height > 0);
    // At four rows, a one-row chrome plus a two-row bordered editor leaves
    // no inner row for the draft.  Keep output + editable input truthful;
    // the input title remains the compact activity affordance at this height.
    let chrome_rows = u16::from(height >= 5);
    let input_floor = match height {
        0 => 0,
        1..=2 => 1,
        _ => 2,
    };
    let status_rows = if height >= 6 {
        requested_status_rows.min(height.saturating_sub(output_floor + chrome_rows + input_floor))
    } else {
        0
    };
    let input_capacity = height.saturating_sub(output_floor + chrome_rows + status_rows);
    let input_rows = requested_input_rows
        .min(input_capacity)
        .max(input_floor.min(input_capacity));
    let slots = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(output_floor),
            Constraint::Length(chrome_rows),
            Constraint::Length(input_rows),
            Constraint::Length(status_rows),
        ])
        .split(area);
    [slots[0], slots[1], slots[2], slots[3]]
}

/// Immutable presentation plan for the live surface.
///
/// Measurement and slot allocation are kept together so the renderer consumes
/// one truthful projection of queue preview, status text, and geometry.  It
/// owns no interaction state and never changes execution facts.
pub(crate) struct LiveFramePlan {
    area: Rect,
    queue_preview: Vec<Line<'static>>,
    below_text: String,
    ctx: u16,
    outer: [Rect; 4],
}

impl LiveFramePlan {
    pub(crate) fn build(
        area: Rect,
        ui: &Ui,
        meta: &ReplMeta,
        tokens: usize,
        vitals: &Vitals,
    ) -> Self {
        // Keep queued intent available whenever the frame can expose an input
        // surface.  At the smallest heights the chrome still carries the
        // queue count; the full preview starts once the input border has
        // content rows.
        let queue_preview = if area.height >= 5 {
            pending_queue_lines(&ui.queued, area.width.saturating_sub(2))
        } else {
            Vec::new()
        };
        let input_rows = input_height(&ui.input.buffer, area.width.saturating_sub(2), 3, 8)
            .saturating_add(queue_preview.len() as u16)
            .min(12);
        let ctx = ctx_percent(vitals.ctx_used, meta.ctx_window as usize);
        // The configured bottom bar may wrap; its height is part of the same
        // geometry pass as the input and live-output slots.
        let input_tokens = if ui.input_tokens > 0 {
            ui.input_tokens.to_string()
        } else {
            format!("~{}", vitals.ctx_used)
        };
        let output_tokens = if ui.output_tokens > 0 || !ui.busy {
            ui.output_tokens.to_string()
        } else {
            // Provider/runtime telemetry may advance ahead of local chunk
            // accounting; keep the busy estimate monotonic and visible.
            format!("~{}", ui.stream_tokens.max(vitals.task_tokens))
        };
        let effort = ui.effort.as_deref().unwrap_or("default");
        let sv = StatusVars {
            provider: meta.provider_label.clone(),
            model: meta.model.clone(),
            ctx: format!("{ctx}%"),
            tokens: format!(
                "total {tokens} tok · in {input_tokens} · out {output_tokens} · effort {effort}"
            ),
            cwd: cwd_name(),
        };
        let configured_status =
            sanitize_display_text(&render_status_template(&meta.status_bar, &sv));
        // Below 80 cells, preserve the high-value telemetry in one compact
        // row when a custom status template would otherwise consume the live
        // viewport with wrapping.
        let below_text = if area.width < 80 && wrapped_rows(&configured_status, area.width) > 1 {
            compact_status_line(
                area.width,
                &meta.provider_label,
                &meta.model,
                &format!("{ctx}%"),
                tokens,
                &input_tokens,
                &output_tokens,
                effort,
            )
        } else {
            configured_status
        };
        let below_rows = wrapped_rows(&below_text, area.width).clamp(1, 3) as u16;
        let outer = responsive_live_layout(area, input_rows, below_rows);
        Self {
            area,
            queue_preview,
            below_text,
            ctx,
            outer,
        }
    }

    #[cfg(test)]
    pub(crate) fn slots(&self) -> [Rect; 4] {
        self.outer
    }

    #[cfg(test)]
    pub(crate) fn status_text(&self) -> &str {
        &self.below_text
    }
}

/// Keep the editable cursor row inside the bounded input viewport after the
/// sticky queue rail consumes some rows.  The editor buffer remains complete;
/// this only chooses which already-wrapped rows are painted.
fn input_viewport(lines: &[String], cursor_row: u16, capacity: usize) -> (Vec<String>, u16) {
    if lines.is_empty() || capacity == 0 {
        return (Vec::new(), 0);
    }
    let capacity = capacity.min(lines.len());
    let cursor = (cursor_row as usize).min(lines.len().saturating_sub(1));
    let start = if lines.len() <= capacity {
        0
    } else {
        cursor
            .saturating_sub(capacity.saturating_sub(1))
            .min(lines.len().saturating_sub(capacity))
    };
    let end = start + capacity;
    (
        lines[start..end].to_vec(),
        cursor.saturating_sub(start) as u16,
    )
}

/// Live 视口绘制(iter-26;iter-31 双状态栏):顶状态行 + 输出尾 + [忙碌粘条] + 输入框 + 自定义底栏;
/// 审批模态覆整个视口。五槽定长布局,忙碌槽空闲时高 0(索引恒定,免条件分支乱套)。
#[cfg(test)]
pub(crate) fn draw(
    frame: &mut ratatui::Frame,
    ui: &Ui,
    meta: &ReplMeta,
    tokens: usize,
    vitals: &Vitals,
    approval: Option<&ApprovalRequest>,
) {
    let mut cache = LiveOutputCache::default();
    draw_with_cache(frame, ui, meta, tokens, vitals, approval, &mut cache);
}

fn input_title_action(text: &str) -> bool {
    text.contains("Ctrl")
        || text.contains('^')
        || text.contains("Enter")
        || text.contains("Alt+")
        || text.contains("Pg")
        || text.contains("Tab")
        || text.contains("Esc")
        || text.contains("queue")
        || text.contains("takeover")
        || text.contains("details")
        || text.contains("reasoning")
        || text.contains("answers")
        || text.contains("activity")
        || text.contains("follow")
        || text.contains("front")
        || text.contains("Space")
        || text.contains("toggle")
}

/// Preserve the exact action text while giving labels, separators, and
/// executable shortcuts distinct visual weights in the input title.
pub(crate) fn input_title_line(text: &str, role: Role) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, part) in text.split(" · ").enumerate() {
        if index > 0 {
            spans.push(Span::styled(
                " · ",
                Style::default().fg(role_color(Role::Muted)),
            ));
        }
        let style = if input_title_action(part) {
            Style::default()
                .fg(role_color(Role::Metric))
                .add_modifier(Modifier::BOLD)
        } else if part.contains("Queue") || part.contains("Input") {
            Style::default().fg(role_color(role))
        } else {
            Style::default().fg(role_color(Role::Label))
        };
        spans.push(Span::styled(part.to_owned(), style));
    }
    Line::from(spans)
}

/// Paint the editor surface and its sticky pending-intent rail.
///
/// Input wrapping, queue preview allocation, and cursor placement share one
/// cell-width calculation here; the orchestration layer no longer needs to
/// know editor details.
fn draw_input_surface(
    frame: &mut ratatui::Frame,
    ui: &Ui,
    approval: Option<&ApprovalRequest>,
    area: Rect,
    queue_preview: Vec<Line<'static>>,
) {
    // A bordered block needs two rows for its frame.  Below three rows there
    // is no honest inner editor viewport, so keep the draft visible as a
    // compact plain-text surface instead of drawing an empty border.
    if area.height < 3 {
        let pending_message = if area.height >= 2 {
            ui.queued.front()
        } else {
            None
        };
        let pending_rows = u16::from(pending_message.is_some());
        // With only one editor row, keep the queue count and draft on the
        // same bounded line; with two rows, give the pending message its own
        // sticky row above the draft.
        let queue_prefix = if pending_message.is_none() && !ui.queued.is_empty() {
            format!("⏭{} ", ui.queued.len())
        } else {
            String::new()
        };
        let (input_lines, cur_row, cur_col) = wrap_input(
            &ui.input.buffer,
            ui.input.cursor,
            area.width.saturating_sub(str_cells(&queue_prefix) as u16),
        );
        let (visible_input_lines, visible_cur_row) = input_viewport(
            &input_lines,
            cur_row,
            area.height.saturating_sub(pending_rows) as usize,
        );
        let mut content = Vec::with_capacity(visible_input_lines.len() + 1);
        if let Some(message) = pending_message {
            content.push(Line::from(clip_display_cells(
                &format!("⏭ {}", message),
                area.width,
            )));
        }
        content.extend(visible_input_lines.iter().enumerate().map(|(index, line)| {
            if index == 0 && !queue_prefix.is_empty() {
                Line::from(format!("{queue_prefix}{line}"))
            } else {
                Line::from(line.as_str())
            }
        }));
        let input_role = if ui.busy {
            input_chrome(InputChromeArgs {
                busy: ui.busy,
                queued: ui.queued.len(),
                width: area.width,
                reasoning_expanded: ui.transcript.is_reasoning_expanded(),
                has_reasoning: ui.transcript.has_reasoning(),
                has_reasoning_history: !ui.reasoning_history.is_empty(),
                has_live_answer: ui.busy && ui.transcript.has_answer(),
                has_answer_history: !ui.answer_history.is_empty(),
                has_live_history: ui.transcript.has_history(),
                has_tools: ui.has_live_tools(),
                has_history: !ui.tool_history.is_empty(),
                has_scrollable_tool_details: ui.has_scrollable_live_tool(),
                has_live_output: ui.has_inspectable_live_output(),
                live_inspecting: ui.transcript.is_inspecting(),
            })
            .1
        } else {
            Role::Muted
        };
        frame.render_widget(
            Paragraph::new(Text::from(content)).style(Style::default().fg(role_color(input_role))),
            area,
        );
        if approval.is_none()
            && ui.panel.is_none()
            && !visible_input_lines.is_empty()
            && area.width > 0
            && area.height > 0
        {
            let x = (area.x + str_cells(&queue_prefix) as u16 + cur_col)
                .min(area.right().saturating_sub(1));
            let y = (area.y + pending_rows + visible_cur_row).min(area.bottom().saturating_sub(1));
            frame.set_cursor_position(Position { x, y });
        }
        return;
    }
    let (input_lines, cur_row, cur_col) = wrap_input(
        &ui.input.buffer,
        ui.input.cursor,
        area.width.saturating_sub(2),
    );
    let input_capacity = area.height.saturating_sub(2) as usize;
    // Reserve one row for the editor before showing additional queue rows:
    // pending intent must remain sticky even while the current draft wraps.
    // If the inner frame has only one row, the top chrome's queue count is the
    // truthful compact fallback; never replace the editable draft with it.
    let queue_visible = queue_preview
        .into_iter()
        .take(input_capacity.saturating_sub(1))
        .collect::<Vec<_>>();
    let queue_rows = queue_visible.len() as u16;
    let (visible_input_lines, visible_cur_row) = input_viewport(
        &input_lines,
        cur_row,
        input_capacity.saturating_sub(queue_visible.len()),
    );
    let mut input_content = queue_visible;
    input_content.extend(
        visible_input_lines
            .iter()
            .map(|line| Line::from(line.as_str())),
    );
    frame.render_widget(
        Paragraph::new(Text::from(input_content)).block({
            let (input_title, input_role) = input_chrome(InputChromeArgs {
                busy: ui.busy,
                queued: ui.queued.len(),
                width: area.width,
                reasoning_expanded: ui.transcript.is_reasoning_expanded(),
                has_reasoning: ui.transcript.has_reasoning(),
                has_reasoning_history: !ui.reasoning_history.is_empty(),
                has_live_answer: ui.busy && ui.transcript.has_answer(),
                has_answer_history: !ui.answer_history.is_empty(),
                has_live_history: ui.transcript.has_history(),
                has_tools: ui.has_live_tools(),
                has_history: !ui.tool_history.is_empty(),
                has_scrollable_tool_details: ui.has_scrollable_live_tool(),
                has_live_output: ui.has_inspectable_live_output(),
                live_inspecting: ui.transcript.is_inspecting(),
            });
            rounded_surface_block()
                .border_style(Style::default().fg(role_color(if ui.busy {
                    input_role
                } else {
                    Role::Muted
                })))
                .title_style(
                    Style::default()
                        .fg(role_color(input_role))
                        .add_modifier(Modifier::BOLD),
                )
                .title(input_title_line(&input_title, input_role))
        }),
        area,
    );
    // CJK/emoji cursor placement uses the same wrapped viewport as the
    // painted editor. Modal surfaces own the cursor while they are open.
    if approval.is_none()
        && ui.panel.is_none()
        && !visible_input_lines.is_empty()
        && area.width >= 3
        && area.height >= 3
    {
        let x = (area.x + 1 + cur_col).min(area.right().saturating_sub(2));
        let y = (area.y + 1 + queue_rows + visible_cur_row).min(area.bottom().saturating_sub(2));
        frame.set_cursor_position(Position { x, y });
    }
}

fn fit_idle_line(width: usize, candidates: &[String]) -> String {
    candidates
        .iter()
        .find(|candidate| str_cells(candidate) <= width)
        .cloned()
        .unwrap_or_else(|| {
            candidates
                .last()
                .map(|candidate| clip_display_cells(candidate, width as u16))
                .unwrap_or_default()
        })
}

fn idle_history_actions(width: usize) -> &'static str {
    if width >= 48 {
        "Ctrl+A answers · Ctrl+R reasoning · Ctrl+T activity"
    } else if width >= 31 {
        "^A answers · ^R think · ^T log"
    } else if width >= 28 {
        "^A answers · ^R think · ^T"
    } else if width >= 21 {
        "^A answers · ^R think"
    } else if width >= 11 {
        "^A ans · ^R"
    } else {
        "^A/^R"
    }
}

fn idle_answer_recovery(width: usize, partial: bool) -> &'static str {
    if partial {
        if width >= 23 {
            "PARTIAL · Ctrl+A expand"
        } else if width >= 19 {
            "PARTIAL · ^A expand"
        } else if width >= 12 {
            "PARTIAL · ^A"
        } else {
            "^A"
        }
    } else if width >= 19 {
        "ANS · Ctrl+A expand"
    } else if width >= 15 {
        "ANS · ^A expand"
    } else if width >= 8 {
        "ANS · ^A"
    } else {
        "^A"
    }
}

fn idle_answer_excerpt(width: usize, entry: &AnswerEntry) -> Option<String> {
    if width < 28 {
        return None;
    }
    let source = entry
        .text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(sanitize_display_text)?;
    let source = source.split_whitespace().collect::<Vec<_>>().join(" ");
    if source.is_empty() {
        return None;
    }

    let prefix = if entry.partial {
        "PARTIAL · "
    } else {
        "ANS · "
    };
    let available = width.saturating_sub(str_cells(prefix));
    (available > 1).then(|| format!("{prefix}{}", clip_display_cells(&source, available as u16)))
}

fn idle_reasoning_excerpt(width: usize, entry: &ReasoningEntry) -> Option<String> {
    if width < 28 {
        return None;
    }
    let source = entry
        .text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(sanitize_display_text)?;
    let source = source.split_whitespace().collect::<Vec<_>>().join(" ");
    if source.is_empty() {
        return None;
    }

    let prefix = "THK · ";
    let available = width.saturating_sub(str_cells(prefix));
    (available > 1).then(|| format!("{prefix}{}", clip_display_cells(&source, available as u16)))
}

fn idle_channel_meta(
    width: usize,
    tag: &str,
    step: usize,
    elapsed_s: u64,
    tokens: usize,
) -> Option<String> {
    if width < 28 {
        return None;
    }
    let step = if step > 0 {
        format!("step {step} · ")
    } else {
        String::new()
    };
    let text = format!("{tag} · {step}+{elapsed_s}s · {tokens} task tok");
    Some(clip_display_cells(&text, width as u16))
}

/// Completed/Conclusion 的详情卡保留语义标签的视觉层级：回答明亮、思考
/// 用独立冷色、结论用成功色，快捷键/普通提示仍保持低干扰。文本、宽度
/// 与历史入口不变，故此处只生成 presentation spans，不引入新状态。
fn idle_detail_line(detail: String, width: u16) -> Line<'static> {
    let detail = clip_display_cells(&detail, width);
    let Some((tag, body)) = detail.split_once(" · ") else {
        return Line::from(Span::styled(
            detail,
            Style::default()
                .fg(role_color(Role::Muted))
                .add_modifier(Modifier::DIM),
        ));
    };
    let (role, body_modifier) = match tag {
        "ANS" => (Role::Primary, Modifier::empty()),
        "PARTIAL" => (Role::Warn, Modifier::empty()),
        "THK" => (Role::Reasoning, live_reasoning_chrome_modifier(width)),
        "SUM" => (Role::Success, Modifier::DIM),
        _ => {
            return Line::from(Span::styled(
                detail,
                Style::default()
                    .fg(role_color(Role::Muted))
                    .add_modifier(Modifier::DIM),
            ));
        }
    };
    Line::from(vec![
        Span::styled(
            format!("{tag} · "),
            Style::default()
                .fg(role_color(role))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            body.to_owned(),
            Style::default()
                .fg(role_color(role))
                .add_modifier(body_modifier),
        ),
    ])
}

fn idle_line_cells(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|span| str_cells(span.content.as_ref()))
        .sum()
}

/// 宽屏完成态的低噪声结果卡。只包 presentation spans，不复制正文或新增
/// 交互状态；窄屏/低高视口继续使用原有线性投影，保证恢复快捷键优先。
fn idle_result_card_lines(
    summary: &(String, Role, Vec<String>),
    width: u16,
    rows: usize,
) -> Option<Vec<Line<'static>>> {
    if width < 48 || rows < summary.2.len().saturating_add(3) {
        return None;
    }
    let inner_width = width.saturating_sub(4);
    let title = clip_display_cells(&summary.0, width.saturating_sub(3));
    let title_cells = str_cells(&title);
    let top_fill = (width as usize)
        .saturating_sub(3)
        .saturating_sub(title_cells);
    let border = Style::default().fg(role_color(Role::Border));
    let title_style = Style::default()
        .fg(role_color(summary.1))
        .add_modifier(Modifier::BOLD);
    let mut lines = Vec::with_capacity(summary.2.len() + 3);
    let card_rows = summary.2.len() + 3;
    lines.extend((0..rows.saturating_sub(card_rows) / 2).map(|_| Line::default()));
    lines.push(Line::from(vec![
        Span::styled("╭─", border),
        Span::styled(title, title_style),
        Span::styled("─".repeat(top_fill), border),
        Span::styled("╮", border),
    ]));
    for detail in &summary.2 {
        let detail = idle_detail_line(detail.clone(), inner_width);
        let padding = inner_width as usize - idle_line_cells(&detail).min(inner_width as usize);
        let mut spans = vec![Span::styled("│ ", border)];
        spans.extend(detail.spans);
        spans.push(Span::raw(" ".repeat(padding)));
        spans.push(Span::styled(" │", border));
        lines.push(Line::from(spans));
    }
    lines.push(Line::from(Span::styled(
        format!("╰{}╯", "─".repeat(width.saturating_sub(2) as usize)),
        border,
    )));
    Some(lines)
}

fn idle_activity_recovery(width: usize) -> &'static str {
    if width >= 15 {
        "Ctrl+T activity"
    } else if width >= 11 {
        "^T activity"
    } else if width >= 6 {
        "^T log"
    } else {
        "^T"
    }
}

fn idle_result_headline(width: usize, tag: &str, headline: &str) -> String {
    let mut candidates = vec![format!("◇ {tag} // {headline}")];
    match (tag, headline) {
        ("DONE", "partial answer retained") => {
            candidates.extend([
                "◇ DONE // partial retained".to_owned(),
                "◇ DONE · partial".to_owned(),
            ]);
        }
        ("DONE", "answer archived") => {
            candidates.extend(["◇ DONE · answer".to_owned(), "DONE · answer".to_owned()]);
        }
        ("SUM", "result ready") => {
            candidates.extend(["◇ SUM · ready".to_owned(), "SUM · result".to_owned()]);
        }
        ("ERR", "inspect failure") => {
            candidates.extend(["◇ ERR · failure".to_owned(), "ERR · failure".to_owned()]);
        }
        _ => {}
    }
    candidates.push(tag.to_owned());
    fit_idle_line(width, &candidates)
}

fn empty_state_headline(width: usize, mode: &str, detail: &str) -> String {
    let (full, compact, short, tag) = match mode {
        "LIVE" => (
            format!("⟳ LIVE // {detail}"),
            format!("⟳ LIVE · {detail}"),
            "⟳ LIVE".to_owned(),
            "LIVE".to_owned(),
        ),
        "WAIT" => (
            format!("◌ WAIT // {detail}"),
            format!("◌ WAIT · {detail}"),
            "◌ WAIT".to_owned(),
            "WAIT".to_owned(),
        ),
        "QUEUE" => (
            format!("◌ QUEUE HOLD // {detail}"),
            format!("◌ QUEUE · {detail}"),
            "◌ QUEUE".to_owned(),
            "QUEUE".to_owned(),
        ),
        _ => (
            "◇ READY // output channel clear".to_owned(),
            "◇ READY · clear".to_owned(),
            "◇ READY".to_owned(),
            "READY".to_owned(),
        ),
    };
    fit_idle_line(width, &[full, compact, short, tag])
}

fn empty_state_actions(width: usize, mode: &str) -> String {
    let candidates = match mode {
        "LIVE" => vec![
            "observing stream · Ctrl+Space hold · Ctrl+I inspect · Esc takeover".to_owned(),
            "stream · Ctrl+Space hold · Ctrl+I · Esc takeover".to_owned(),
            "stream · ^Space hold · ^I · Esc".to_owned(),
            "stream · ^Space · ^I · Esc".to_owned(),
            "^Space · ^I · Esc".to_owned(),
            "Esc".to_owned(),
        ],
        "WAIT" => vec![
            "waiting · no stream · Esc/Ctrl+C takeover · Ctrl+I inspect".to_owned(),
            "waiting · Esc/Ctrl+C takeover".to_owned(),
            "WAIT · Esc/^C takeover".to_owned(),
            "WAIT · ^C".to_owned(),
            "WAIT".to_owned(),
        ],
        "QUEUE" => vec![
            "Enter queue · Ctrl+Enter front · Ctrl+C takeover".to_owned(),
            "Enter queue · Ctrl+Enter front".to_owned(),
            "Enter · ^Enter front".to_owned(),
            "^Enter front".to_owned(),
            "Enter".to_owned(),
        ],
        _ => vec![
            "Enter send · Ctrl+T activity · /help".to_owned(),
            "Enter send · ^T activity".to_owned(),
            "Enter · ^T".to_owned(),
            "Enter".to_owned(),
        ],
    };
    fit_idle_line(width, &candidates)
}

fn idle_conclusion_detail(width: usize, text: &str) -> String {
    let text = sanitize_display_text(text).trim().to_owned();
    let full = if text.is_empty() {
        "SUM · result".to_owned()
    } else {
        format!("SUM · {text}")
    };
    fit_idle_line(width, &[full, "SUM · result".to_owned(), "SUM".to_owned()])
}

fn idle_result_summary(ui: &Ui, width: usize, rows: usize) -> Option<(String, Role, Vec<String>)> {
    let signal = ui.activity_history.back()?;
    let has_answer = !ui.answer_history.is_empty();
    let has_reasoning = !ui.reasoning_history.is_empty();
    let partial_answer = ui.answer_history.back().is_some_and(|entry| entry.partial);
    let answer_tag = if partial_answer { "PARTIAL" } else { "ANS" };
    let (tag, role, headline, detail) = match signal.kind {
        ActivityKind::Completed => (
            "DONE",
            Role::Success,
            if partial_answer {
                "partial answer retained"
            } else {
                "answer archived"
            },
            if has_answer {
                idle_answer_recovery(width, partial_answer)
            } else {
                idle_activity_recovery(width)
            },
        ),
        ActivityKind::Conclusion => (
            "SUM",
            Role::Success,
            "result ready",
            if has_answer {
                idle_answer_recovery(width, partial_answer)
            } else {
                idle_activity_recovery(width)
            },
        ),
        ActivityKind::Error => (
            "ERR",
            Role::Error,
            "inspect failure",
            if has_answer {
                idle_answer_recovery(width, partial_answer)
            } else {
                idle_activity_recovery(width)
            },
        ),
        _ => return None,
    };
    let mut details = Vec::with_capacity(4);
    if matches!(
        signal.kind,
        ActivityKind::Completed | ActivityKind::Conclusion
    ) {
        if let Some(conclusion) = ui
            .activity_history
            .iter()
            .rev()
            .find(|entry| entry.kind == ActivityKind::Conclusion)
        {
            details.push(idle_conclusion_detail(width, &conclusion.text));
        }
    }
    if matches!(
        signal.kind,
        ActivityKind::Completed | ActivityKind::Conclusion | ActivityKind::Error
    ) && rows >= 5
        && has_answer
    {
        if let Some(answer) = ui.answer_history.back() {
            if let Some(excerpt) = idle_answer_excerpt(width, answer) {
                details.push(excerpt);
            }
        }
    }
    if matches!(
        signal.kind,
        ActivityKind::Completed | ActivityKind::Conclusion | ActivityKind::Error
    ) && width >= 48
        && rows >= 10
    {
        if let Some(answer) = ui.answer_history.back() {
            if let Some(meta) = idle_channel_meta(
                width,
                answer_tag,
                answer.step,
                answer.elapsed_s,
                answer.tokens,
            ) {
                details.push(meta);
            }
        }
        if let Some(reasoning) = ui.reasoning_history.back() {
            if let Some(meta) = idle_channel_meta(
                width,
                "THK",
                reasoning.step,
                reasoning.elapsed_s,
                reasoning.tokens,
            ) {
                details.push(meta);
            }
        }
    }
    if matches!(
        signal.kind,
        ActivityKind::Completed | ActivityKind::Conclusion | ActivityKind::Error
    ) && rows >= 6
        && has_reasoning
    {
        if let Some(reasoning) = ui.reasoning_history.back() {
            if let Some(excerpt) = idle_reasoning_excerpt(width, reasoning) {
                details.push(excerpt);
            }
        }
    }
    details.push(detail.to_owned());
    details.push(idle_history_actions(width).to_owned());
    Some((idle_result_headline(width, tag, headline), role, details))
}

fn live_empty_state(ui: &Ui, width: u16, rows: usize) -> Vec<Line<'static>> {
    if width == 0 || rows == 0 {
        return Vec::new();
    }

    let (headline, headline_role, details) = if ui.busy && ui.waiting {
        let phase = if ui.activity.trim().is_empty() {
            "no stream"
        } else {
            ui.activity.as_str()
        };
        (
            empty_state_headline(
                width as usize,
                "WAIT",
                sanitize_display_text(phase.trim()).trim(),
            ),
            Role::Warn,
            vec![empty_state_actions(width as usize, "WAIT")],
        )
    } else if ui.busy {
        let phase = if ui.activity.trim().is_empty() {
            ui.phase.as_str()
        } else {
            ui.activity.as_str()
        };
        (
            empty_state_headline(
                width as usize,
                "LIVE",
                sanitize_display_text(phase.trim()).trim(),
            ),
            Role::Primary,
            vec![empty_state_actions(width as usize, "LIVE")],
        )
    } else if !ui.queued.is_empty() {
        (
            empty_state_headline(
                width as usize,
                "QUEUE",
                &format!("{} pending", ui.queued.len()),
            ),
            Role::Warn,
            vec![empty_state_actions(width as usize, "QUEUE")],
        )
    } else if let Some(summary) = idle_result_summary(ui, width as usize, rows) {
        if let Some(card) = idle_result_card_lines(&summary, width, rows) {
            return card;
        }
        summary
    } else {
        (
            empty_state_headline(width as usize, "READY", ""),
            Role::Primary,
            vec![empty_state_actions(width as usize, "READY")],
        )
    };
    let width = width as usize;
    let headline = clip_display_cells(&headline, width as u16);
    let details = details
        .into_iter()
        .map(|detail| idle_detail_line(detail, width as u16))
        .collect::<Vec<_>>();
    let content_rows = 1 + details.len();
    let mut lines = Vec::with_capacity(rows.min(content_rows));
    let pad = rows.saturating_sub(content_rows) / 2;
    lines.extend((0..pad).map(|_| Line::default()));
    lines.push(Line::from(Span::styled(
        headline,
        Style::default()
            .fg(role_color(headline_role))
            .add_modifier(Modifier::BOLD),
    )));
    for detail in details {
        if lines.len() >= rows {
            break;
        }
        lines.push(detail);
    }
    lines
}

#[cfg(test)]
pub(crate) fn live_empty_state_for_test(ui: &Ui, width: u16, rows: usize) -> Vec<Line<'static>> {
    live_empty_state(ui, width, rows)
}

fn live_line_cells(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|span| str_cells(span.content.as_ref()))
        .sum()
}

fn render_live_cursor(
    frame: &mut ratatui::Frame,
    area: Rect,
    line_count: usize,
    last_line_cells: usize,
    frame_index: usize,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let row = line_count
        .saturating_sub(1)
        .min(area.height.saturating_sub(1) as usize) as u16;
    let col = last_line_cells.min(area.width.saturating_sub(1) as usize) as u16;
    let cursor = Line::from(Span::styled(
        "█",
        Style::default().fg(role_color(Role::Primary)).add_modifier(
            if frame_index.is_multiple_of(2) {
                Modifier::BOLD
            } else {
                Modifier::DIM
            },
        ),
    ));
    frame.render_widget(
        Paragraph::new(cursor),
        Rect {
            x: area.x + col,
            y: area.y + row,
            width: 1,
            height: 1,
        },
    );
}

/// Paint only the bounded live tail. The cache owns wrapping; this boundary
/// adds the transient phase anchor and breathing cursor after cache reuse.
fn draw_live_output(
    frame: &mut ratatui::Frame,
    ui: &Ui,
    vitals: &Vitals,
    area: Rect,
    live_cache: &mut LiveOutputCache,
) {
    let surface = live_surface_block(ui, area);
    let content_area = surface.as_ref().map_or(area, |block| block.inner(area));
    let rows = content_area.height as usize;
    let anchor = (rows >= 2).then(|| {
        live_phase_anchor(ui, vitals, content_area.width)
            .or_else(|| live_lifecycle_anchor(ui, vitals, content_area.width))
    });
    let anchor_line = anchor.flatten();
    let anchor_rows = usize::from(anchor_line.is_some());
    let output_rows = rows.saturating_sub(anchor_rows).max(1);
    live_cache.prepare(
        &ui.transcript,
        content_area.width,
        output_rows,
        ui.busy,
        vitals,
    );

    let mut output_area = content_area;
    if let Some(anchor) = anchor_line {
        if output_area.height > 0 {
            frame.render_widget(
                Paragraph::new(anchor),
                Rect {
                    x: output_area.x,
                    y: output_area.y,
                    width: output_area.width,
                    height: 1,
                },
            );
            output_area.y = output_area.y.saturating_add(1);
            output_area.height = output_area.height.saturating_sub(1);
        }
    }

    if live_cache.is_empty() {
        let empty = live_empty_state(ui, output_area.width, output_area.height as usize);
        let line_count = empty.len();
        let last_line_cells = empty.last().map(live_line_cells).unwrap_or_default();
        frame.render_widget(Paragraph::new(Text::from(empty)), output_area);
        if ui.busy {
            render_live_cursor(frame, output_area, line_count, last_line_cells, ui.frame);
        }
    } else {
        live_cache.render(frame, output_area);
        if ui.busy {
            render_live_cursor(
                frame,
                output_area,
                live_cache.line_count,
                live_cache.cursor_cells(),
                ui.frame,
            );
        }
    }
    if let Some(surface) = surface {
        frame.render_widget(surface, area);
    }
}

/// Paint non-blocking audit overlays. These surfaces observe the live state;
/// they do not own panel selection, device authentication, or execution.
fn draw_audit_overlays(
    frame: &mut ratatui::Frame,
    area: Rect,
    ui: &Ui,
    panel_cache: &mut PanelLayoutCache,
) {
    if let Some(panel) = &ui.panel {
        draw_panel_with_cache(frame, area, panel, panel_cache);
    }
    if let Some(status) = ui.device_auth_status.as_deref() {
        draw_device_auth(frame, area, status);
    }
}

/// Production draw entry point.  The cache lives outside `Ui`: execution and
/// interaction state stay independent from render-only memoization.
pub(crate) fn draw_with_cache(
    frame: &mut ratatui::Frame,
    ui: &Ui,
    meta: &ReplMeta,
    tokens: usize,
    vitals: &Vitals,
    approval: Option<&ApprovalRequest>,
    live_cache: &mut LiveOutputCache,
) {
    let draw_started = std::time::Instant::now();
    let LiveFramePlan {
        area,
        queue_preview,
        below_text,
        ctx,
        outer,
    } = LiveFramePlan::build(frame.area(), ui, meta, tokens, vitals);
    // Four stable slots: live output / top activity chrome / editor / wrapped
    // telemetry.  The plan owns measurement; this function only paints.
    // Live tail uses the cached bounded projection, then adds only transient
    // anchor/cursor decoration for this frame.
    draw_live_output(frame, ui, vitals, outer[0], live_cache);
    // [1] 输入框上状态条(常驻):badge + 越狱标 + (busy → 实时忙碌条 | idle → ready+todo)。
    // **不含 provider/model/ctx/tokens** —— 那些在下方状态条,避免旧顶栏那种重复。
    frame.render_widget(
        Paragraph::new(top_chrome(ui, vitals, outer[1].width)).style(telemetry_surface()),
        outer[1],
    );
    // 输入框:**自己字符折行**(与光标同口径),不再用 ratatui 词折行 —— 光标才能跟着折行走。
    draw_input_surface(frame, ui, approval, outer[2], queue_preview);
    // 输入框下状态条(config `status_bar` 模板)—— **可换行**:高已按折行算好,`.wrap` 落多行。
    frame.render_widget(
        Paragraph::new(status_line_projection(&below_text))
            .wrap(Wrap { trim: false })
            .style(telemetry_surface().fg(role_color(context_pressure_role(ctx)))),
        outer[3],
    );
    // 补全浮窗(iter-27):输入框上方,有界尺寸,高亮选中项。
    if let Some(p) = &ui.popup {
        let h = (p.items.len().min(6) as u16) + 2;
        let w = p
            .items
            .iter()
            .map(|s| str_cells(s))
            .max()
            .unwrap_or(10)
            .min(48) as u16
            + 4;
        let x = outer[2].x + 1;
        let y = outer[2].y.saturating_sub(h);
        let rect = Rect {
            x,
            y,
            width: w.min(area.width.saturating_sub(x)),
            height: h.min(area.height),
        };
        frame.render_widget(Clear, rect);
        let items: Vec<ListItem> = p.items.iter().map(|s| ListItem::new(s.as_str())).collect();
        let mut state = ListState::default();
        state.select(Some(p.selected));
        frame.render_stateful_widget(
            List::new(items)
                .block(
                    rounded_surface_block()
                        .title(" ↑↓ select · Tab complete · Enter send · Esc close "),
                )
                .highlight_style(selection_style()),
            rect,
            &mut state,
        );
    }
    // 交互页模态(iter-35):居中覆视口,搜索框 + 过滤列表 + 提示。审批优先级更高,故在其前画。
    draw_audit_overlays(frame, area, ui, &mut live_cache.panel);
    if let Some(req) = approval {
        // 审批模态覆整个 Live 视口;↑↓ 滚动看长 diff;diff 行按 +/- 语义着色(iter-28)。
        frame.render_widget(Clear, area);
        let mut lines: Vec<Line> = vec![
            Line::from(Span::styled(
                format!("⚠ Allow {} ?", req.action),
                Style::default()
                    .fg(role_color(Role::Warn))
                    .add_modifier(Modifier::BOLD),
            )),
            Line::default(),
        ];
        for l in req.detail.lines() {
            let role = if l.starts_with('+') {
                Role::DiffAdd
            } else if l.starts_with('-') {
                Role::DiffDel
            } else {
                Role::Warn
            };
            lines.push(Line::from(Span::styled(
                l.to_owned(),
                Style::default().fg(role_color(role)),
            )));
        }
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "y/Enter: approve    n/Esc: reject    ↑↓/PgUp/PgDn: scroll details",
            Style::default().fg(role_color(Role::Muted)),
        )));
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .block(
                    rounded_surface_block()
                        .title(" Permission required ")
                        .border_style(Style::default().fg(role_color(Role::Warn))),
                )
                .wrap(Wrap { trim: false })
                .scroll((ui.scroll, 0)),
            area,
        );
    }
    dump_frame_snapshot(frame, draw_started, ui, vitals);
}

/// Live 行的语义侧轨：只表达可见行类别/邻接/焦点，不伪造块因果或工具结果。
pub(crate) fn live_rail(
    kind: LiveLineKind,
    focused_tool: bool,
    previous_kind: Option<LiveLineKind>,
) -> Option<(&'static str, Role)> {
    match kind {
        LiveLineKind::Answer if previous_kind == Some(LiveLineKind::Reasoning) => {
            Some(("╰", Role::Primary))
        }
        LiveLineKind::Answer
            if matches!(
                previous_kind,
                Some(LiveLineKind::ToolSummary | LiveLineKind::ToolDetail)
            ) =>
        {
            Some(("╰", Role::Primary))
        }
        LiveLineKind::Answer => Some(("┃", Role::Primary)),
        LiveLineKind::Reasoning if previous_kind != Some(LiveLineKind::Reasoning) => {
            Some(("┌", Role::Reasoning))
        }
        LiveLineKind::Reasoning => Some(("│", Role::Reasoning)),
        LiveLineKind::ToolSummary if previous_kind == Some(LiveLineKind::Reasoning) => Some((
            "├",
            if focused_tool {
                Role::Primary
            } else {
                Role::Info
            },
        )),
        LiveLineKind::ToolSummary if focused_tool => Some(("▌", Role::Primary)),
        LiveLineKind::ToolSummary => Some(("│", Role::Info)),
        LiveLineKind::ToolDetail if focused_tool => Some(("┆", Role::Primary)),
        LiveLineKind::ToolDetail => Some(("┆", Role::Muted)),
        LiveLineKind::Splash => None,
    }
}

/// 活跃 reasoning 只依据当前忙碌态与**已渲染尾行**判定，不把 step 归因伪造到 LiveBlock。
pub(crate) fn active_reasoning_tail_role(
    kind: LiveLineKind,
    busy: bool,
    is_tail: bool,
) -> Option<Role> {
    (kind == LiveLineKind::Reasoning).then_some(if busy && is_tail {
        Role::Primary
    } else {
        Role::Reasoning
    })
}

pub(crate) fn live_code_rail(in_code: bool, fence_line: bool) -> Option<(&'static str, Role)> {
    if fence_line {
        Some(("\u{251c}", Role::Border))
    } else if in_code {
        Some(("\u{250a}", Role::Muted))
    } else {
        None
    }
}

pub(crate) fn live_tool_rail_role(kind: LiveLineKind, color: Color, role: Role) -> Role {
    if matches!(kind, LiveLineKind::ToolSummary | LiveLineKind::ToolDetail)
        && color == role_color(Role::Error)
    {
        Role::Error
    } else {
        role
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;

    #[test]
    fn snapshot_preserves_rows_and_metadata() {
        let mut buffer = Buffer::with_lines(["RidgeCode", "输出"]);
        buffer[(0, 0)].set_style(
            Style::default()
                .fg(role_color(Role::Primary))
                .add_modifier(Modifier::BOLD),
        );
        let mut ui = Ui::default();
        ui.set_activity("node · reason");
        // Snapshot metadata test must not depend on wall-clock scheduling
        // between set_activity and serialization.
        ui.activity_started = None;
        let vitals = Vitals {
            step: 2,
            elapsed_s: 3,
            task_tokens: 5,
            rate: 7,
            ctx_used: 11,
            queued: 1,
        };
        let snapshot = snapshot_payload(&buffer, 37, &ui, &vitals);
        let value: serde_json::Value = serde_json::from_str(&snapshot).expect("snapshot json");

        assert_eq!(value["format"], "ridgecode-tui-frame");
        assert_eq!(value["version"], 2);
        assert_eq!(value["rect"]["width"], 9);
        assert_eq!(value["render_us"], 37);
        assert_eq!(value["state"]["busy"], false);
        assert_eq!(value["state"]["activity_kind"], "THK");
        assert_eq!(value["state"]["activity_sequence"], 1);
        assert_eq!(
            value["state"]["activity_history"][0]["text"],
            "node · reason"
        );
        assert_eq!(value["state"]["reasoning_expanded"], false);
        assert_eq!(value["state"]["queued"], 1);
        assert_eq!(value["state"]["rate"], 7);
        assert!(value["state"]["presentation"].is_array());
        assert!(value["state"]["presentation"]
            .as_array()
            .is_some_and(|rows| { rows.len() <= MAX_PRESENTATION_RECORDS }));
        assert_eq!(value["telemetry"]["last_render_us"], 37);
        assert_eq!(value["telemetry"]["token_velocity"], 7);
        assert_eq!(value["telemetry"]["phase_duration_ms"], 0);
        let rows = value["rows"]
            .as_array()
            .expect("snapshot rows")
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>();
        assert!(rows.iter().any(|row| row.contains("RidgeCode")));
        assert!(rows
            .iter()
            .any(|row| row.contains('输') && row.contains('出')));
        assert!(value["styled_rows"]
            .as_array()
            .expect("styled rows")
            .iter()
            .flat_map(serde_json::Value::as_array)
            .flat_map(|runs| runs.iter())
            .any(|run| {
                run["style"]["roles"]
                    .as_array()
                    .is_some_and(|roles| roles.iter().any(|role| role == "primary"))
                    && run["style"]["modifiers"]
                        .as_array()
                        .is_some_and(|modifiers| {
                            modifiers.iter().any(|modifier| modifier == "bold")
                        })
            }));
    }

    #[test]
    fn snapshot_surfaces_open_panel_selection() {
        let buffer = Buffer::with_lines(["Queue"]);
        let mut ui = Ui::default();
        ui.queued.push_back("first pending request".into());
        ui.open_queue_panel();
        let vitals = Vitals {
            step: 0,
            elapsed_s: 0,
            task_tokens: 0,
            rate: 0,
            ctx_used: 0,
            queued: 1,
        };
        let value: serde_json::Value =
            serde_json::from_str(&snapshot_payload(&buffer, 11, &ui, &vitals)).expect("snapshot");
        assert_eq!(value["panel"]["kind"], "Queue");
        assert_eq!(value["panel"]["selected"], "⏭ next");
        assert_eq!(value["panel"]["visible_rows"], 1);
        assert_eq!(value["state"]["live_view"], "follow");
    }

    #[test]
    fn snapshot_surfaces_live_hold_mode() {
        let buffer = Buffer::with_lines(["hold"]);
        let mut ui = Ui::default();
        ui.push_chunk(provider::StreamChunk::Answer("answer".into()));
        assert!(ui.hold_live());
        let vitals = Vitals {
            step: 1,
            elapsed_s: 1,
            task_tokens: 1,
            rate: 1,
            ctx_used: 1,
            queued: 0,
        };
        let value: serde_json::Value =
            serde_json::from_str(&snapshot_payload(&buffer, 13, &ui, &vitals)).expect("snapshot");
        assert_eq!(value["state"]["live_view"], "hold");
    }

    #[test]
    fn snapshot_surfaces_reasoning_history_panel() {
        let buffer = Buffer::with_lines(["reasoning"]);
        let mut ui = Ui::default();
        ui.push_chunk(provider::StreamChunk::Reasoning("inspect state".into()));
        ui.commit_live_reasoning(3, 2);
        assert!(ui.open_reasoning_history());
        let vitals = Vitals {
            step: 0,
            elapsed_s: 0,
            task_tokens: 4,
            rate: 0,
            ctx_used: 2,
            queued: 0,
        };
        let value: serde_json::Value =
            serde_json::from_str(&snapshot_payload(&buffer, 17, &ui, &vitals)).expect("snapshot");
        assert_eq!(value["state"]["reasoning_history"], 1);
        assert_eq!(value["panel"]["kind"], "Reasoning");
        assert!(value["panel"]["selected"]
            .as_str()
            .is_some_and(|selected| selected.contains("step 3")));
    }

    #[test]
    fn snapshot_surfaces_live_block_inspector_selection_and_detail() {
        let buffer = Buffer::with_lines(["live"]);
        let mut ui = Ui::default();
        ui.push_chunk(provider::StreamChunk::Reasoning("plan".into()));
        ui.push_tool(
            ToolBlock::from_lines(vec![
                ("read_file".into(), Color::Cyan),
                ("contents".into(), Color::Gray),
            ])
            .expect("tool"),
        );
        assert!(ui.open_live_history());
        assert!(ui.panel.as_mut().expect("live panel").toggle_detail());
        let vitals = Vitals {
            step: 2,
            elapsed_s: 1,
            task_tokens: 5,
            rate: 5,
            ctx_used: 4,
            queued: 0,
        };
        let value: serde_json::Value =
            serde_json::from_str(&snapshot_payload(&buffer, 19, &ui, &vitals)).expect("snapshot");
        assert_eq!(value["state"]["live_blocks"], 2);
        assert_eq!(value["panel"]["kind"], "Audit");
        assert_eq!(value["panel"]["detail_open"], true);
        assert!(value["panel"]["selected"]
            .as_str()
            .is_some_and(|selected| selected.contains("read_file")));
    }

    #[test]
    fn snapshot_and_chrome_surface_answer_reasoning_focus() {
        let buffer = Buffer::with_lines(["live"]);
        let mut ui = Ui {
            busy: true,
            ..Ui::default()
        };
        ui.push_chunk(provider::StreamChunk::Reasoning("plan".into()));
        ui.push_chunk(provider::StreamChunk::Answer("answer".into()));
        assert!(ui.open_live_history());

        let vitals = Vitals {
            step: 2,
            elapsed_s: 1,
            task_tokens: 5,
            rate: 5,
            ctx_used: 4,
            queued: 0,
        };
        let value: serde_json::Value =
            serde_json::from_str(&snapshot_payload(&buffer, 19, &ui, &vitals)).expect("snapshot");
        assert_eq!(value["state"]["live_focus"], "answer:1");
        assert_eq!(value["state"]["presentation"][0]["channel"], "reasoning");
        assert_eq!(value["state"]["presentation"][1]["channel"], "answer");
        assert_eq!(value["state"]["presentation"][1]["status"], "live");
        assert_eq!(value["state"]["live_trace"], "THK›ANS");
        let chrome = top_chrome(&ui, &vitals, 96)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(chrome.contains("ANSWER"), "{chrome}");

        ui.panel.as_mut().expect("live panel").move_down();
        ui.sync_live_panel_focus();
        assert!(matches!(
            ui.transcript.focused_block(),
            Some(LiveBlockFocus::Reasoning(_))
        ));
        let chrome = top_chrome(&ui, &vitals, 96)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(chrome.contains("THINK"), "{chrome}");
    }

    #[test]
    fn top_chrome_surfaces_live_phase_trace() {
        let mut ui = Ui {
            busy: true,
            phase: "answering".into(),
            ..Ui::default()
        };
        ui.push_chunk(provider::StreamChunk::Reasoning("plan".into()));
        ui.push_tool(ToolBlock::from_lines(vec![("search".into(), Color::Cyan)]).expect("tool"));
        ui.push_chunk(provider::StreamChunk::Answer("result".into()));
        let vitals = Vitals {
            step: 3,
            elapsed_s: 4,
            task_tokens: 5,
            rate: 6,
            ctx_used: 7,
            queued: 0,
        };

        let wide = top_chrome(&ui, &vitals, 120)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(wide.contains("THK›TLS›ANS"), "{wide}");

        let narrow = top_chrome(&ui, &vitals, 40)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(!narrow.contains("THK›TLS›ANS"), "{narrow}");
        assert!(str_cells(&narrow) <= 40);
    }

    #[test]
    fn live_lifecycle_anchor_keeps_waiting_visible_beside_existing_stream() {
        let mut ui = Ui {
            busy: true,
            ..Ui::default()
        };
        ui.push_chunk(provider::StreamChunk::Answer(
            "stream tail remains visible".into(),
        ));
        ui.set_activity("waiting · no stream for 8s");
        ui.waiting = true;
        let vitals = Vitals {
            step: 3,
            elapsed_s: 9,
            task_tokens: 12,
            rate: 0,
            ctx_used: 0,
            queued: 0,
        };

        let anchor = live_lifecycle_anchor(&ui, &vitals, 40).expect("waiting anchor");
        let anchor_text = anchor
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(anchor_text.contains("WAIT"), "{anchor_text}");
        assert!(anchor_text.contains("no stream"), "{anchor_text}");
        assert!(str_cells(&anchor_text) <= 40);

        let mut terminal =
            Terminal::new(ratatui::backend::TestBackend::new(40, 6)).expect("lifecycle terminal");
        let mut cache = LiveOutputCache::default();
        terminal
            .draw(|frame| draw_live_output(frame, &ui, &vitals, frame.area(), &mut cache))
            .expect("lifecycle draw");
        let symbols = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            symbols.contains("WAIT"),
            "waiting status missing: {symbols}"
        );
        assert!(
            symbols.contains("stream tail"),
            "existing live stream disappeared: {symbols}"
        );
    }

    #[test]
    fn live_lifecycle_anchor_is_bounded_and_omits_channel_chatter() {
        let mut ui = Ui {
            busy: true,
            ..Ui::default()
        };
        ui.set_activity("verifying result");
        let vitals = Vitals {
            step: 1,
            elapsed_s: 0,
            task_tokens: 0,
            rate: 0,
            ctx_used: 0,
            queued: 0,
        };
        for width in [12, 18, 32, 80] {
            let anchor = live_lifecycle_anchor(&ui, &vitals, width).expect("verification anchor");
            let text = anchor
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>();
            assert!(text.contains("CHK"), "{text}");
            assert!(str_cells(&text) <= width as usize, "{width}: {text}");
        }

        ui.set_activity("answering result");
        assert!(
            live_lifecycle_anchor(&ui, &vitals, 80).is_none(),
            "ordinary channel activity must keep its existing rail only"
        );
    }

    #[test]
    fn live_surface_title_tracks_observed_phase() {
        let mut ui = Ui::default();
        assert!(live_surface_title(&ui, 48).contains("READY"));

        ui.busy = true;
        ui.push_chunk(provider::StreamChunk::Reasoning("plan".into()));
        assert!(live_surface_title(&ui, 48).contains("THK"));

        ui.waiting = true;
        assert!(live_surface_title(&ui, 48).contains("WAIT"));
        assert!(live_surface_title(&ui, 24).contains("WAIT"));
        assert!(live_surface_title(&ui, 12).is_empty());
    }

    #[test]
    fn live_surface_title_keeps_channel_context_and_hold_visible() {
        let mut ui = Ui {
            busy: true,
            ..Ui::default()
        };
        ui.push_chunk(provider::StreamChunk::Reasoning("plan".into()));
        ui.push_chunk(provider::StreamChunk::Answer("result".into()));

        let title = live_surface_title(&ui, 48);
        assert!(title.contains("THK/ANS"), "{title}");
        assert!(!title.contains("HOLD"), "{title}");

        assert!(ui.hold_live());
        let held = live_surface_title(&ui, 48);
        assert!(held.contains("THK/ANS"), "{held}");
        assert!(held.contains("HOLD"), "{held}");

        let styled = live_surface_title_line(&ui, 48);
        assert!(styled.spans.iter().any(|span| {
            span.content == "THK" && span.style.fg == Some(role_color(Role::Reasoning))
        }));
        assert!(styled.spans.iter().any(|span| {
            span.content == "ANS" && span.style.fg == Some(role_color(Role::Primary))
        }));
        assert!(styled.spans.iter().any(|span| {
            span.content == " · HOLD"
                && span.style.fg == Some(role_color(Role::Warn))
                && span.style.add_modifier.contains(Modifier::BOLD)
        }));
    }

    #[test]
    fn live_surface_border_preserves_waiting_and_stream_content() {
        let mut ui = Ui {
            busy: true,
            ..Ui::default()
        };
        ui.push_chunk(provider::StreamChunk::Answer(
            "stream tail remains visible".into(),
        ));
        ui.set_activity("waiting 路 no stream");
        ui.waiting = true;
        let vitals = Vitals {
            step: 3,
            elapsed_s: 9,
            task_tokens: 12,
            rate: 0,
            ctx_used: 0,
            queued: 0,
        };
        let mut terminal =
            Terminal::new(ratatui::backend::TestBackend::new(40, 6)).expect("surface terminal");
        let mut cache = LiveOutputCache::default();
        terminal
            .draw(|frame| draw_live_output(frame, &ui, &vitals, frame.area(), &mut cache))
            .expect("surface draw");
        let buffer = terminal.backend().buffer();
        let symbols = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            symbols.contains("WAIT"),
            "waiting status missing: {symbols}"
        );
        assert!(
            symbols.contains("stream tail"),
            "existing live stream disappeared: {symbols}"
        );
        assert_eq!(buffer[(0, 0)].symbol(), "╭");
        assert_eq!(buffer[(0, 0)].fg, role_color(Role::Warn));
    }

    #[test]
    fn input_title_line_preserves_text_width_and_emphasizes_actions() {
        let raw = " Queue [2] · ↵ queue · ^Enter · ^C takeover · ^O";
        let line = input_title_line(raw, Role::Primary);
        let rendered = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(rendered, raw);
        assert_eq!(str_cells(&rendered), str_cells(raw));
        assert!(line.spans.iter().any(|span| {
            span.content.contains("^C takeover")
                && span.style.fg == Some(role_color(Role::Metric))
                && span.style.add_modifier.contains(Modifier::BOLD)
        }));
        assert!(line
            .spans
            .iter()
            .any(|span| span.content == " · " && span.style.fg == Some(role_color(Role::Muted))));
    }

    #[test]
    fn live_output_cache_reuses_rows_until_transcript_or_geometry_changes() {
        let mut transcript = LiveTranscript::default();
        transcript.push_answer("cached answer");
        let vitals = Vitals {
            step: 1,
            elapsed_s: 2,
            task_tokens: 3,
            rate: 4,
            ctx_used: 5,
            queued: 0,
        };
        let mut cache = LiveOutputCache::default();

        let first = cache.lines(&transcript, 32, 4, false, &vitals);
        assert!(!first.is_empty());
        assert_eq!(cache.rebuilds(), 1);

        let second = cache.lines(&transcript, 32, 4, false, &vitals);
        assert_eq!(second.len(), first.len());
        assert_eq!(cache.rebuilds(), 1);

        let later_vitals = Vitals {
            step: 9,
            elapsed_s: 17,
            task_tokens: 42,
            ..vitals
        };
        let _ = cache.lines(&transcript, 32, 4, false, &later_vitals);
        assert_eq!(
            cache.rebuilds(),
            1,
            "telemetry-only changes keep layout cache hot"
        );

        transcript.push_answer(" + new stream");
        let _ = cache.lines(&transcript, 32, 4, false, &vitals);
        assert_eq!(cache.rebuilds(), 2);

        let _ = cache.lines(&transcript, 31, 4, false, &vitals);
        assert_eq!(cache.rebuilds(), 3);
    }

    #[test]
    fn wrapped_live_continuations_keep_the_semantic_rail() {
        let mut transcript = LiveTranscript::default();
        transcript.push_reasoning("a long reasoning conclusion that must wrap");
        let line = transcript
            .visible_lines(4)
            .into_iter()
            .next()
            .expect("reasoning line");
        let mut state = LiveRowState {
            max_rows: 4,
            ..LiveRowState::default()
        };
        let rows = render_live_line(line, 0, 0, 16, false, &mut state);

        assert!(rows.len() > 1, "fixture must wrap: {rows:?}");
        assert_eq!(rows[0].spans[0].content, "┌");
        for row in rows.iter().skip(1) {
            assert_eq!(
                row.spans[0].content, "│",
                "row lost reasoning rail: {row:?}"
            );
            assert_eq!(row.spans[0].style.fg, Some(role_color(Role::Reasoning)));
        }
    }

    #[test]
    fn held_live_view_preserves_semantic_line_when_width_reflows() {
        let mut transcript = LiveTranscript::default();
        transcript.push_reasoning(
            &(0..12)
                .map(|index| format!("line-{index}: {}", "x".repeat(28)))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        assert!(transcript.hold_live());
        let vitals = Vitals {
            step: 1,
            elapsed_s: 0,
            task_tokens: 0,
            rate: 0,
            ctx_used: 0,
            queued: 0,
        };
        let mut cache = LiveOutputCache::default();
        let _ = cache.lines(&transcript, 96, 5, false, &vitals);
        let reflowed = cache.lines(&transcript, 40, 5, false, &vitals);
        let rendered = reflowed
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(rendered.contains("line-3"), "{rendered}");
        assert!(reflowed.len() <= 5);
    }

    #[test]
    fn live_tail_wrap_bounds_unbroken_output() {
        let rows = wrap_live_spans_tail(
            vec![Span::raw(
                (0..100)
                    .map(|i| (b'0' + (i % 10) as u8) as char)
                    .collect::<String>(),
            )],
            10,
            3,
        );
        let rendered = rows
            .iter()
            .flat_map(|row| row.iter().map(|span| span.content.as_ref()))
            .collect::<String>();

        assert_eq!(rows.len(), 3);
        assert_eq!(rendered.chars().count(), 30);
        assert!(rendered.ends_with("789"));
    }

    #[test]
    fn live_code_highlighting_is_bounded_to_visible_tail() {
        let source = format!("let prefix = {};", "x".repeat(512));
        let line = LiveLine {
            text: &source,
            color: Color::White,
            kind: LiveLineKind::Answer,
            marker: None,
            anchor: None,
            answer_plain: false,
            fence_before: true,
            continuation_before: false,
        };
        let mut state = LiveRowState {
            max_rows: 2,
            ..LiveRowState::default()
        };
        let rows = render_live_line(line, 0, 0, 20, false, &mut state);
        assert!(!rows.is_empty() && rows.len() <= 2);
        assert!(rows.iter().all(|row| {
            row.spans
                .iter()
                .map(|span| str_cells(span.content.as_ref()))
                .sum::<usize>()
                <= 20
        }));
        let rendered = rows
            .iter()
            .flat_map(|row| row.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(rendered.contains('…'));
        assert!(!rendered.contains("prefix"));
        assert!(rendered.ends_with("xxx;"));
    }

    #[test]
    fn live_projection_caps_physical_tail_after_many_wrapped_lines() {
        let mut transcript = LiveTranscript::default();
        transcript.push_answer(
            &(0..16)
                .map(|index| format!("line-{index}: {}", "x".repeat(28)))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let rows = render_live_tail_lines(&transcript, 12, 4, false);
        assert_eq!(rows.len(), 4);
        let rendered = rows
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(rendered.contains("line-15"), "{rendered}");
        assert!(rows.iter().all(|line| {
            line.spans
                .iter()
                .map(|span| str_cells(span.content.as_ref()))
                .sum::<usize>()
                <= 12
        }));
    }

    #[test]
    fn live_alert_projection_closes_visible_alert_edge() {
        let mut transcript = LiveTranscript::default();
        transcript.push_answer("> [!WARNING] Protect the boundary\n> Continue this conclusion");
        let rows = render_live_tail_lines(&transcript, 64, 4, false);
        let rendered = rows
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(rendered.contains("┌ WARNING"), "{rendered}");
        assert!(rendered.contains("└ Continue"), "{rendered}");
        assert!(rows
            .iter()
            .any(|line| { line.spans.iter().any(|span| span.content.as_ref() == "└") }));
    }

    #[test]
    fn live_markdown_highlighting_tails_long_prose_before_span_scan() {
        let source = format!("{} **visible tail**", "prefix ".repeat(512));
        let line = LiveLine {
            text: &source,
            color: Color::White,
            kind: LiveLineKind::Answer,
            marker: None,
            anchor: None,
            answer_plain: false,
            fence_before: false,
            continuation_before: false,
        };
        let mut state = LiveRowState {
            max_rows: 2,
            ..LiveRowState::default()
        };
        let rows = render_live_line(line, 0, 0, 20, false, &mut state);
        let rendered = rows
            .iter()
            .flat_map(|row| row.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(rows.len() <= 2);
        assert!(rendered.contains('…'));
        assert!(rendered.contains("visible tail"));
        assert!(!rendered.contains("prefix prefix"));
    }

    #[test]
    fn live_empty_state_keeps_idle_queue_and_busy_intent_visible() {
        let mut ui = Ui::default();
        let text = |lines: &[Line<'static>]| {
            lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .map(|span| span.content.as_ref())
                .collect::<String>()
        };

        let ready = live_empty_state(&ui, 48, 6);
        assert!(text(&ready).contains("READY"));

        ui.queued.push_back("/queued".to_owned());
        let queued = live_empty_state(&ui, 48, 6);
        assert!(text(&queued).contains("QUEUE HOLD"));
        assert!(text(&queued).contains("1 pending"));

        ui.busy = true;
        ui.activity = "model · thinking".to_owned();
        let busy = live_empty_state(&ui, 48, 6);
        assert!(text(&busy).contains("LIVE // model · thinking"));
        assert!(busy.iter().all(|line| {
            line.spans
                .iter()
                .map(|span| str_cells(span.content.as_ref()))
                .sum::<usize>()
                <= 48
        }));

        ui.activity = "waiting · no stream for 8s".to_owned();
        ui.waiting = true;
        let waiting = live_empty_state(&ui, 48, 6);
        assert!(text(&waiting).contains("WAIT"));
        assert!(text(&waiting).contains("no stream"));
        assert!(text(&waiting).contains("takeover"));
        assert!(waiting.iter().all(|line| {
            line.spans
                .iter()
                .map(|span| str_cells(span.content.as_ref()))
                .sum::<usize>()
                <= 48
        }));
    }

    #[test]
    fn active_reasoning_body_uses_focus_role_while_idle_stays_reasoning() {
        let make_line = || LiveLine {
            text: "live thought",
            color: role_color(Role::Reasoning),
            kind: LiveLineKind::Reasoning,
            marker: None,
            anchor: None,
            answer_plain: false,
            fence_before: false,
            continuation_before: false,
        };
        let body_color = |busy: bool| {
            let mut state = LiveRowState {
                max_rows: 2,
                ..LiveRowState::default()
            };
            render_live_line(make_line(), 0, 0, 40, busy, &mut state)
                .into_iter()
                .next()
                .and_then(|line| line.spans.last().cloned())
                .and_then(|span| span.style.fg)
                .expect("reasoning body style")
        };

        assert_eq!(body_color(true), role_color(Role::Primary));
        assert_eq!(body_color(false), role_color(Role::Reasoning));
    }

    #[test]
    fn answer_body_is_regular_while_channel_rail_stays_bold() {
        let line = LiveLine {
            text: "answer prose",
            color: role_color(Role::Answer),
            kind: LiveLineKind::Answer,
            marker: Some("🤖 "),
            anchor: None,
            answer_plain: true,
            fence_before: false,
            continuation_before: false,
        };
        let mut state = LiveRowState {
            max_rows: 2,
            ..LiveRowState::default()
        };
        let row = render_live_line(line, 0, 0, 64, false, &mut state)
            .into_iter()
            .next()
            .expect("answer row");
        let body = row
            .spans
            .iter()
            .find(|span| span.content.as_ref() == "a")
            .expect("answer body span");
        assert!(!body.style.add_modifier.contains(Modifier::BOLD));
        assert!(row
            .spans
            .iter()
            .any(|span| span.style.add_modifier.contains(Modifier::BOLD)));
    }

    #[test]
    fn focused_tool_detail_body_lifts_dim_only_when_explicitly_focused() {
        let make_line = || LiveLine {
            text: "tool output detail",
            color: role_color(Role::Info),
            kind: LiveLineKind::ToolDetail,
            marker: None,
            anchor: None,
            answer_plain: false,
            fence_before: false,
            continuation_before: false,
        };
        let body_modifier = |focused: bool| {
            let mut state = LiveRowState {
                max_rows: 2,
                previous_focused_tool: focused,
                ..LiveRowState::default()
            };
            render_live_line(make_line(), 0, 0, 64, false, &mut state)
                .into_iter()
                .next()
                .and_then(|line| line.spans.last().cloned())
                .map(|span| span.style.add_modifier)
                .expect("tool detail body style")
        };

        assert!(!body_modifier(true).contains(Modifier::DIM));
        assert!(body_modifier(false).contains(Modifier::DIM));
    }

    #[test]
    fn audit_focus_emphasizes_selected_tool_without_restyling_follow_view() {
        let mut transcript = LiveTranscript::default();
        transcript.push_answer("answer context");
        transcript.push_tool(
            ToolBlock::from_lines(vec![
                ("tool summary".into(), Color::Cyan),
                ("tool detail".into(), Color::Gray),
            ])
            .expect("tool"),
        );

        let line_style = |rows: &[Line<'static>], needle: &str| {
            rows.iter()
                .find(|line| {
                    line.spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect::<String>()
                        .contains(needle)
                })
                .and_then(|line| line.spans.last())
                .map(|span| span.style.add_modifier)
                .unwrap_or_else(|| panic!("missing rendered line: {needle}"))
        };

        let follow = render_live_tail_projection(&transcript, 64, 8, false, None).lines;
        assert!(!line_style(&follow, "answer context").contains(Modifier::DIM));

        let tool_focus = transcript
            .inspector_rows()
            .into_iter()
            .find_map(|entry| match entry.focus {
                LiveBlockFocus::Tool(_) => Some(entry.focus),
                _ => None,
            })
            .expect("tool focus");
        assert!(transcript.hold_live());
        assert!(transcript.focus_live_block(tool_focus));
        assert_eq!(transcript.focused_block(), Some(tool_focus));
        assert!(transcript.is_inspecting());
        let audit = render_live_tail_projection(&transcript, 64, 8, false, None).lines;

        assert!(line_style(&audit, "tool summary").contains(Modifier::BOLD));
        let answer_modifier = line_style(&audit, "answer context");
        assert!(
            answer_modifier.contains(Modifier::DIM),
            "answer modifier={answer_modifier:?}; audit={audit:?}"
        );
    }

    #[test]
    fn semantic_boundary_closes_visible_reasoning_before_answer() {
        let mut transcript = LiveTranscript::default();
        transcript.push_reasoning("think first\nthink second");
        transcript.push_answer("answer next");

        let rows = render_live_tail_projection(&transcript, 64, 8, false, None).lines;
        assert_eq!(rows.len(), 3, "boundary cue must not add a physical row");
        let reasoning = rows
            .iter()
            .find(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
                    .contains("think second")
            })
            .expect("reasoning row");
        assert_eq!(
            reasoning.spans.first().map(|span| span.content.as_ref()),
            Some("└")
        );
    }

    #[test]
    fn narrow_live_reasoning_keeps_readability_floor() {
        let mut transcript = LiveTranscript::default();
        transcript.push_reasoning("think clearly");

        let modifier_at = |width| {
            let rendered = render_live_tail_projection(&transcript, width, 8, false, None);
            rendered
                .lines
                .into_iter()
                .find(|line| {
                    line.spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect::<String>()
                        .contains("think clearly")
                })
                .expect("reasoning body")
                .spans
                .last()
                .expect("reasoning body span")
                .style
                .add_modifier
        };

        for width in [18, 24, 32, 40] {
            let narrow = modifier_at(width);
            assert!(narrow.contains(Modifier::ITALIC), "width={width}");
            assert!(!narrow.contains(Modifier::DIM), "width={width}");
        }
        for width in [80, 96] {
            let wide = modifier_at(width);
            assert!(wide.contains(Modifier::ITALIC), "width={width}");
            assert!(wide.contains(Modifier::DIM), "width={width}");
        }
    }

    #[test]
    fn idle_reasoning_detail_keeps_the_same_readability_floor() {
        let modifier_at = |width| {
            idle_detail_line("THK · inspect the reasoning".to_owned(), width)
                .spans
                .last()
                .expect("reasoning detail body")
                .style
                .add_modifier
        };

        for width in [32, 40] {
            let narrow = modifier_at(width);
            assert!(narrow.contains(Modifier::ITALIC), "width={width}");
            assert!(!narrow.contains(Modifier::DIM), "width={width}");
        }
        for width in [80, 96] {
            let wide = modifier_at(width);
            assert!(wide.contains(Modifier::ITALIC), "width={width}");
            assert!(wide.contains(Modifier::DIM), "width={width}");
        }
    }
}
