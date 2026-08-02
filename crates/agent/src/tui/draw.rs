use std::borrow::Cow;

use super::*;

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

fn panel_kind_label(kind: PanelKind) -> &'static str {
    match kind {
        PanelKind::Config => "Config",
        PanelKind::Provider => "Provider",
        PanelKind::Tools => "Tools",
        PanelKind::ToolHistory => "History",
        PanelKind::Models => "Models",
        PanelKind::Agent => "Agents",
        PanelKind::Login => "Login",
        PanelKind::Mcp => "MCP",
        PanelKind::Skills => "Skills",
    }
}

fn panel_title(panel: &Panel, width: u16) -> String {
    let label = panel_kind_label(panel.kind);
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

fn panel_hint(panel: &Panel, width: u16) -> String {
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
            PanelKind::ToolHistory => {
                "↑↓/PgUp/PgDn select · Home/End jump · Enter expand · Esc close"
            }
            PanelKind::Tools | PanelKind::Agent | PanelKind::Mcp | PanelKind::Skills => {
                "↑↓ scroll · type to filter · Esc close"
            }
        }
    };
    let compact = if panel.editing.is_some() {
        "Enter · Esc"
    } else {
        "↑↓ · Enter · Esc"
    };
    let text = if width >= 32 {
        full
    } else if width >= 14 {
        compact
    } else {
        "Esc"
    };
    clip_display_cells(text, width)
}

fn panel_query(query: &str, width: u16) -> String {
    let prefix = if width >= 12 { "🔍 " } else { "> " };
    clip_display_cells(&format!("{prefix}{query}"), width)
}

fn panel_item(text: String, width: u16) -> ListItem<'static> {
    ListItem::new(clip_display_cells(&text, width))
}

fn panel_item_styled(text: String, width: u16, style: Style) -> ListItem<'static> {
    ListItem::new(Line::from(Span::styled(
        clip_display_cells(&text, width),
        style,
    )))
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
pub(crate) fn draw_panel(frame: &mut ratatui::Frame, area: Rect, panel: &Panel) {
    if panel.kind == PanelKind::ToolHistory {
        draw_tool_history_panel(frame, area, panel);
        return;
    }
    let rect = modal_rect(area);
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(panel_title(panel, rect.width.saturating_sub(2)))
        .border_style(Style::default().fg(role_color(Role::Primary)));
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
    let items: Vec<ListItem> = if panel.kind == PanelKind::Models {
        let mut items: Vec<ListItem> = Vec::new();
        let mut last_group: Option<&str> = None;
        for &i in &panel.view {
            let r = &panel.rows[i];
            let group = r.key.split_once(" · ").map(|(g, _)| g);
            if group != last_group {
                let hdr = format!(" ── {} ──", group.unwrap_or(""));
                items.push(panel_item_styled(
                    hdr,
                    rows[list_index].width,
                    Style::default()
                        .fg(role_color(Role::Muted))
                        .add_modifier(Modifier::BOLD),
                ));
                last_group = group;
            }
            // 行内剥 "provider · " 前缀:provider 已由分栏标题承载,行只显模型名。
            let name = r.key.split_once(" · ").map(|(_, m)| m).unwrap_or(&r.key);
            let line = if r.value.is_empty() {
                format!("  {name}")
            } else {
                format!("  {name:<18} {}", r.value)
            };
            items.push(panel_item(line, rows[list_index].width));
        }
        items
    } else {
        panel
            .view
            .iter()
            .map(|&i| {
                let r = &panel.rows[i];
                let line = if r.value.is_empty() {
                    r.key.clone()
                } else {
                    format!("{:<18} {}", r.key, r.value)
                };
                panel_item(line, rows[list_index].width)
            })
            .collect()
    };
    let mut state = ListState::default();
    // Models 页:sel 需跨过分栏标题行
    let sel_in_items = if panel.kind == PanelKind::Models {
        let mut idx = 0;
        let mut last_group: Option<&str> = None;
        for (vi, &i) in panel.view.iter().enumerate() {
            let r = &panel.rows[i];
            let group = r.key.split_once(" · ").map(|(g, _)| g);
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
    state.select(sel_in_items);
    frame.render_stateful_widget(
        List::new(items).highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(role_color(Role::Primary))
                .add_modifier(Modifier::BOLD),
        ),
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

/// 工具历史检视器:原生 scrollback 保持静态,当前摘要列表与有界详情在 live modal 中交互。
fn draw_tool_history_panel(frame: &mut ratatui::Frame, area: Rect, panel: &Panel) {
    let rect = modal_rect(area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(panel_title(panel, rect.width.saturating_sub(2)))
        .border_style(Style::default().fg(role_color(Role::Primary)));
    let inner = block.inner(rect);
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);

    let selected_detail = panel
        .selected()
        .filter(|row| panel.detail_open && !row.value.is_empty())
        .map(|row| row.value.clone());
    let show_query = inner.height >= 3;
    let show_hint = inner.height >= 2;
    let max_detail = inner.height.saturating_sub(3).max(1) as usize;
    let detail_height = selected_detail
        .as_deref()
        .filter(|_| show_query && show_hint && inner.height >= 4)
        .map(|text| {
            let minimum = if max_detail >= 2 { 2 } else { 1 };
            (wrapped_rows(text, inner.width).saturating_add(1))
                .min(max_detail)
                .max(minimum) as u16
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

    let items: Vec<ListItem> = panel
        .view
        .iter()
        .map(|&index| {
            let row = &panel.rows[index];
            let marker = if row.value.is_empty() {
                "· "
            } else if panel.detail_open && panel.view.get(panel.sel).copied() == Some(index) {
                "▾ "
            } else {
                "▸ "
            };
            panel_item_styled(
                format!("{marker}{}", row.key),
                rows[list_index].width,
                Style::default().fg(role_color(Role::Info)),
            )
        })
        .collect();
    let mut state = ListState::default();
    state.select((!panel.view.is_empty()).then_some(panel.sel));
    frame.render_stateful_widget(
        List::new(items).highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(role_color(Role::Primary))
                .add_modifier(Modifier::BOLD),
        ),
        rows[list_index],
        &mut state,
    );

    if let Some(detail) = selected_detail {
        let detail_index = list_index + 1;
        let visible_rows = rows[detail_index].height.saturating_sub(1).max(1);
        let detail_scroll = detail_match_scroll(
            &detail,
            &panel.query,
            rows[detail_index].width,
            visible_rows,
        );
        frame.render_widget(
            Paragraph::new(detail)
                .style(Style::default().fg(role_color(Role::Muted)))
                .block(Block::default().borders(Borders::TOP).title(" Detail "))
                .wrap(Wrap { trim: false })
                .scroll((detail_scroll, 0)),
            rows[detail_index],
        );
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

#[derive(Default)]
struct LiveRowState {
    code_before: bool,
    previous_kind: Option<LiveLineKind>,
    previous_focused_tool: bool,
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

/// 单行 Live 投影：集中处理 Answer/Reasoning/Tool 的 rail、marker、badge 与宽度预算。
/// 只消费真实 `LiveLine` 与既有体征，不拥有任务状态或输入语义。
fn render_live_line<'a>(
    line: LiveLine<'a>,
    index: usize,
    last_visible_line: usize,
    width: u16,
    busy: bool,
    vitals: &Vitals,
    state: &mut LiveRowState,
) -> Line<'a> {
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
    let reasoning_role = active_reasoning_tail_role(line.kind, busy, index == last_visible_line);
    let code_before = line.kind == LiveLineKind::Answer && state.code_before;
    let fence_line = line.kind == LiveLineKind::Answer && line.text.trim_start().starts_with("```");
    let fence_label = (!code_before && fence_line)
        .then(|| fence_language(line.text))
        .flatten();
    let continuation_rail = (line.kind == LiveLineKind::Reasoning && line.continuation_before)
        .then_some(("┊", reasoning_role.unwrap_or(Role::Muted)));
    let rail = continuation_rail
        .or_else(|| live_code_rail(code_before, fence_line))
        .or_else(|| live_rail(line.kind, focused_tool, state.previous_kind))
        .map(|(rail, role)| {
            let role = reasoning_role.unwrap_or(role);
            (rail, live_tool_rail_role(line.kind, line.color, role))
        });
    state.previous_focused_tool = match line.kind {
        LiveLineKind::ToolSummary => line.marker == Some("▸ "),
        LiveLineKind::ToolDetail => focused_tool,
        _ => false,
    };
    state.previous_kind = Some(line.kind);
    let modifier = match line.kind {
        LiveLineKind::Answer => Modifier::BOLD,
        LiveLineKind::Reasoning => Modifier::DIM | Modifier::ITALIC,
        LiveLineKind::ToolSummary => Modifier::BOLD,
        LiveLineKind::ToolDetail => Modifier::DIM,
        LiveLineKind::Splash => Modifier::BOLD,
    };
    let marker: Option<Cow<'static, str>> =
        if line.kind == LiveLineKind::Reasoning && line.marker.is_some() {
            Some(Cow::Owned(fmt_reasoning_meta(
                vitals.step,
                vitals.elapsed_s,
                vitals.task_tokens,
            )))
        } else {
            line.marker.map(Cow::Borrowed)
        };
    let rail_cells = rail.map(|(rail, _)| str_cells(rail)).unwrap_or_default();
    let marker = marker.and_then(|marker| fit_live_marker(marker, line.kind, width, rail_cells));
    let mut spans: Vec<Span<'a>> = Vec::with_capacity(3);
    if let Some((rail, rail_role)) = rail {
        spans.push(Span::styled(
            rail,
            Style::default()
                .fg(role_color(rail_role))
                .add_modifier(modifier),
        ));
    }
    let marker_cells = marker.as_deref().map(str_cells).unwrap_or_default();
    if let Some(marker) = marker {
        let marker_role = match line.kind {
            LiveLineKind::Answer => Role::Primary,
            LiveLineKind::Reasoning => reasoning_role.unwrap_or(Role::Muted),
            _ => Role::Info,
        };
        spans.push(Span::styled(
            marker,
            Style::default()
                .fg(role_color(marker_role))
                .add_modifier(modifier),
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
    if line.kind == LiveLineKind::Answer {
        let display_text = has_badge.then(|| fence_without_language(line.text));
        spans.extend(live_markdown_line(
            display_text.as_deref().unwrap_or(line.text),
            text_width,
            &mut state.code_before,
            line.color,
            modifier,
        ));
    } else {
        let text = if str_cells(line.text) <= text_width as usize {
            Cow::Borrowed(line.text)
        } else {
            Cow::Owned(clip_display_cells(line.text, text_width))
        };
        spans.push(Span::styled(
            text,
            Style::default().fg(line.color).add_modifier(modifier),
        ));
    }
    Line::from(spans)
}

fn push_chrome_fit(
    spans: &mut Vec<Span<'static>>,
    used: &mut usize,
    width: usize,
    text: &'static str,
    style: Style,
) -> bool {
    let cells = str_cells(text);
    if cells == 0 || cells > width.saturating_sub(*used) {
        return false;
    }
    *used += cells;
    spans.push(Span::styled(text, style));
    true
}

fn push_chrome_clipped(
    spans: &mut Vec<Span<'static>>,
    used: &mut usize,
    width: usize,
    text: &str,
    style: Style,
) {
    let remaining = width.saturating_sub(*used);
    if remaining == 0 {
        return;
    }
    let clipped = clip_display_cells(text, remaining as u16);
    *used += str_cells(&clipped);
    spans.push(Span::styled(clipped, style));
}

fn push_chrome_dynamic_fit(
    spans: &mut Vec<Span<'static>>,
    used: &mut usize,
    width: usize,
    text: String,
    style: Style,
) -> bool {
    let cells = str_cells(&text);
    if cells == 0 || cells > width.saturating_sub(*used) {
        return false;
    }
    *used += cells;
    spans.push(Span::styled(text, style));
    true
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

fn push_channel_badge(
    spans: &mut Vec<Span<'static>>,
    used: &mut usize,
    width: usize,
    channel: LiveChannel,
) {
    let (label, role) = stream_channel_badge(channel);
    let style = Style::default()
        .fg(Color::Black)
        .bg(role_color(role))
        .add_modifier(Modifier::BOLD);
    let full = match label {
        "[ANSWER]" => " [ANSWER] ",
        "[THINK]" => " [THINK] ",
        "[TOOL]" => " [TOOL] ",
        _ => unreachable!("stream badge labels are exhaustive"),
    };
    if push_chrome_fit(spans, used, width, full, style) {
        return;
    }
    let compact = match channel {
        LiveChannel::Answer => " A ",
        LiveChannel::Reasoning => " T ",
        LiveChannel::Tool => " O ",
    };
    let _ = push_chrome_fit(spans, used, width, compact, style);
}

/// 顶部 chrome 的唯一投影：品牌、越狱警示、真实流通道与 busy/ready 状态在此组装。
/// 宽度不足时优先保留安全/通道 badge，再裁剪次要 phase 文案，避免窄端只剩无意义省略号。
/// 不拥有任务状态；只消费 `Ui`/`Vitals` 的既有事实，令主布局函数只负责槽位与覆盖层。
pub(crate) fn top_chrome(ui: &Ui, vitals: &Vitals, area_width: u16) -> Line<'static> {
    let width = area_width as usize;
    let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"][ui.frame % 10];
    let status_text = if ui.busy {
        let busy_phase = fmt_busy_phase(&ui.phase, vitals.step);
        format!(
            " {spinner} {}",
            fmt_busy_bar(
                busy_phase.as_ref(),
                &ui.todos,
                vitals.elapsed_s,
                vitals.task_tokens,
                vitals.rate,
                vitals.queued,
                ui.pending_call.as_ref(),
            )
        )
    } else {
        let todo = todo_progress(&ui.todos)
            .map(|(done, total)| format!(" · todo {done}/{total}"))
            .unwrap_or_default();
        format!(" ready{todo}")
    };
    let status_style = Style::default()
        .fg(role_color(if ui.busy { Role::Warn } else { Role::Muted }))
        .add_modifier(if ui.busy {
            Modifier::BOLD
        } else {
            Modifier::empty()
        });
    let mut above = Vec::new();
    let mut used = 0usize;
    let wide = width >= 32;
    if wide {
        let brand_style = Style::default()
            .fg(Color::Black)
            .bg(if ui.busy { Color::Yellow } else { Color::Cyan })
            .add_modifier(Modifier::BOLD);
        let _ = push_chrome_fit(&mut above, &mut used, width, " RidgeCode ", brand_style);
    }
    if agent::allow_jailbreak() {
        let style = Style::default()
            .fg(Color::Black)
            .bg(role_color(Role::Error))
            .add_modifier(Modifier::BOLD);
        let full = " ⚠JAILBREAK ";
        if !push_chrome_fit(&mut above, &mut used, width, full, style) {
            let _ = push_chrome_fit(&mut above, &mut used, width, " [JAIL] ", style);
        }
    }
    if ui.busy {
        if let Some(channel) = ui.transcript.active_channel() {
            push_channel_badge(&mut above, &mut used, width, channel);
        }
    } else if !wide {
        let brand_style = Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        let _ = push_chrome_fit(&mut above, &mut used, width, " RDG ", brand_style);
    }
    if ui.has_live_tools() && width >= 48 {
        if let Some(summary) = ui.transcript.focused_tool_summary() {
            if let Some(chip) = focused_tool_chip(summary, width.saturating_sub(used)) {
                let style = Style::default()
                    .fg(Color::Black)
                    .bg(role_color(Role::Info))
                    .add_modifier(Modifier::BOLD);
                let _ = push_chrome_dynamic_fit(&mut above, &mut used, width, chip, style);
            }
        }
    }
    if ui.busy
        && !ui.has_live_tools()
        && ui.transcript.active_channel() == Some(LiveChannel::Answer)
        && ui.transcript.has_reasoning()
        && width >= 48
    {
        if let Some(chip) = reasoning_visibility_chip(
            ui.transcript.is_reasoning_expanded(),
            width.saturating_sub(used),
        ) {
            let style = Style::default()
                .fg(Color::Black)
                .bg(role_color(Role::Muted))
                .add_modifier(Modifier::BOLD);
            let _ = push_chrome_dynamic_fit(&mut above, &mut used, width, chip, style);
        }
    }
    push_chrome_clipped(&mut above, &mut used, width, &status_text, status_style);
    Line::from(above)
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
    let chrome_rows = u16::from(height >= 4);
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

/// Live 视口绘制(iter-26;iter-31 双状态栏):顶状态行 + 输出尾 + [忙碌粘条] + 输入框 + 自定义底栏;
/// 审批模态覆整个视口。五槽定长布局,忙碌槽空闲时高 0(索引恒定,免条件分支乱套)。
pub(crate) fn draw(
    frame: &mut ratatui::Frame,
    ui: &Ui,
    meta: &ReplMeta,
    tokens: usize,
    vitals: &Vitals,
    approval: Option<&ApprovalRequest>,
) {
    let area = frame.area();
    let input_rows = input_height(&ui.input.buffer, area.width.saturating_sub(2), 3, 8);
    let ctx = ctx_percent(vitals.ctx_used, meta.ctx_window as usize);
    // 下方状态条(config 模板)——**可换行**(用户需求):按内容折行算高,clamp [1,3]。
    let sv = StatusVars {
        provider: meta.provider.clone(),
        model: meta.model.clone(),
        ctx: format!("{ctx}%"),
        tokens: tokens.to_string(),
        cwd: cwd_name(),
    };
    let below_text = sanitize_display_text(&render_status_template(&meta.status_bar, &sv));
    let below_rows = wrapped_rows(&below_text, area.width).clamp(1, 3) as u16;
    // 四区(用户需求:只留输入框上下两条状态条,删掉与下方重复的顶部状态行):
    // [0] 输出 / [1] 输入框上状态条(常驻 live 状态)/ [2] 输入框 / [3] 输入框下状态条(可换行)。
    let outer = responsive_live_layout(area, input_rows, below_rows);
    // [0] 流式尾巴:只画最后 K 行(已完段落随 Superstep 静态提交进历史)。
    // 分道显示:**回答**(白·粗)恒显且优先占行;其上用剩余行显**思考**(灰·暗),
    // 免把回答当思考、也不让长思考挤掉回答。回答空(仍在思考)时全用来显思考。
    let k = outer[0].height as usize;
    let visible_lines = ui.transcript.visible_lines(k);
    let last_visible_line = visible_lines.len().saturating_sub(1);
    let mut row_state = LiveRowState::default();
    let mut tail = visible_lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let line_width = if ui.busy && index == last_visible_line {
                outer[0].width.saturating_sub(1)
            } else {
                outer[0].width
            };
            render_live_line(
                line,
                index,
                last_visible_line,
                line_width,
                ui.busy,
                vitals,
                &mut row_state,
            )
        })
        .collect::<Vec<_>>();
    // 流式呼吸游标(iter-28):busy 时尾缀青色 █,按帧奇偶 BOLD/DIM 交替。
    if ui.busy {
        let cursor = Span::styled(
            "█",
            Style::default().fg(role_color(Role::Primary)).add_modifier(
                if ui.frame.is_multiple_of(2) {
                    Modifier::BOLD
                } else {
                    Modifier::DIM
                },
            ),
        );
        match tail.last_mut() {
            Some(last) => last.spans.push(cursor),
            None => tail.push(Line::from(cursor)),
        }
    }
    frame.render_widget(Paragraph::new(Text::from(tail)), outer[0]);
    // [1] 输入框上状态条(常驻):badge + 越狱标 + (busy → 实时忙碌条 | idle → ready+todo)。
    // **不含 provider/model/ctx/tokens** —— 那些在下方状态条,避免旧顶栏那种重复。
    frame.render_widget(
        Paragraph::new(top_chrome(ui, vitals, outer[1].width))
            .style(Style::default().bg(Color::DarkGray)),
        outer[1],
    );
    // 输入框:**自己字符折行**(与光标同口径),不再用 ratatui 词折行 —— 光标才能跟着折行走。
    let (input_lines, cur_row, cur_col) = wrap_input(
        &ui.input.buffer,
        ui.input.cursor,
        outer[2].width.saturating_sub(2),
    );
    frame.render_widget(
        Paragraph::new(Text::from(
            input_lines
                .iter()
                .map(|l| Line::from(l.as_str()))
                .collect::<Vec<_>>(),
        ))
        .block({
            let (input_title, input_role) = input_chrome(InputChromeArgs {
                busy: ui.busy,
                queued: ui.queued.len(),
                width: outer[2].width,
                reasoning_expanded: ui.transcript.is_reasoning_expanded(),
                has_reasoning: ui.transcript.has_reasoning(),
                has_tools: ui.has_live_tools(),
                has_history: !ui.tool_history.is_empty(),
                has_scrollable_tool_details: ui.has_scrollable_live_tool(),
            });
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(role_color(input_role)))
                .title_style(
                    Style::default()
                        .fg(role_color(input_role))
                        .add_modifier(Modifier::BOLD),
                )
                .title(input_title)
        }),
        outer[2],
    );
    // 输入框下状态条(config `status_bar` 模板)—— **可换行**:高已按折行算好,`.wrap` 落多行。
    frame.render_widget(
        Paragraph::new(Text::from(below_text))
            .wrap(Wrap { trim: false })
            .style(
                Style::default()
                    .fg(role_color(context_pressure_role(ctx)))
                    .bg(Color::DarkGray),
            ),
        outer[3],
    );
    // 真光标(iter-27;iter-30 改按显示单元格列):CJK/emoji 宽字符落点精确,不再偏左。
    // 审批 / 交互页模态开时不落输入光标(iter-35)。
    if approval.is_none() && ui.panel.is_none() && outer[2].width >= 3 && outer[2].height >= 3 {
        let inner = outer[2];
        let x = (inner.x + 1 + cur_col).min(inner.right().saturating_sub(2));
        let y = (inner.y + 1 + cur_row).min(inner.bottom().saturating_sub(2));
        frame.set_cursor_position(Position { x, y });
    }
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
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Tab/↑↓ select · Enter use · Esc close "),
                )
                .highlight_style(
                    Style::default()
                        .fg(Color::Black)
                        .bg(role_color(Role::Primary))
                        .add_modifier(Modifier::BOLD),
                ),
            rect,
            &mut state,
        );
    }
    // 交互页模态(iter-35):居中覆视口,搜索框 + 过滤列表 + 提示。审批优先级更高,故在其前画。
    if let Some(panel) = &ui.panel {
        draw_panel(frame, area, panel);
    }
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
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Permission required ")
                        .border_style(Style::default().fg(role_color(Role::Warn))),
                )
                .wrap(Wrap { trim: false })
                .scroll((ui.scroll, 0)),
            area,
        );
    }
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
            Some(("┌", Role::Muted))
        }
        LiveLineKind::Reasoning => Some(("│", Role::Muted)),
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
        Role::Muted
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
