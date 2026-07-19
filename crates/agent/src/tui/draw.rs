use super::*;

/// 交互页模态绘制(iter-35):居中框(≤80 宽)= 搜索/编辑行 + 过滤列表(选中高亮)+ 提示行。
pub(crate) fn draw_panel(frame: &mut ratatui::Frame, area: Rect, panel: &Panel) {
    let w = area.width.saturating_sub(4).clamp(20, 80);
    let h = area.height.saturating_sub(2).max(6);
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    let rect = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", panel.title))
        .border_style(Style::default().fg(role_color(Role::Primary)));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // 搜索/编辑行
            Constraint::Min(1),    // 列表
            Constraint::Length(1), // 提示
        ])
        .split(inner);
    // 搜索行(编辑态显编辑缓冲;登录页的 key 输入掩码,防肩窥)。
    let (head, head_color) = match &panel.editing {
        Some(buf) if panel.kind == PanelKind::Login => (
            format!("✎ API key: {}", "•".repeat(buf.chars().count())),
            role_color(Role::Warn),
        ),
        Some(buf) => (format!("✎ new value: {buf}"), role_color(Role::Warn)),
        None => (format!("🔍 {}", panel.query), role_color(Role::Muted)),
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            head,
            Style::default().fg(head_color),
        ))),
        rows[0],
    );
    // 过滤列表:key 左对齐 + 右列值。
    let items: Vec<ListItem> = panel
        .view
        .iter()
        .map(|&i| {
            let r = &panel.rows[i];
            let line = if r.value.is_empty() {
                r.key.clone()
            } else {
                format!("{:<18} {}", r.key, r.value)
            };
            ListItem::new(line)
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
        rows[1],
        &mut state,
    );
    let hint = if panel.editing.is_some() {
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
            PanelKind::Tools | PanelKind::Agent => "↑↓ scroll · type to filter · Esc close",
        }
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(role_color(Role::Muted)),
        ))),
        rows[2],
    );
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
    let below_text = render_status_template(&meta.status_bar, &sv);
    let below_rows = wrapped_rows(&below_text, area.width).clamp(1, 3) as u16;
    // 四区(用户需求:只留输入框上下两条状态条,删掉与下方重复的顶部状态行):
    // [0] 输出 / [1] 输入框上状态条(常驻 live 状态)/ [2] 输入框 / [3] 输入框下状态条(可换行)。
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),             // [0] 输出尾
            Constraint::Length(1),          // [1] 输入框上状态条(常驻)
            Constraint::Length(input_rows), // [2] 输入框
            Constraint::Length(below_rows), // [3] 输入框下状态条(可换行)
        ])
        .split(area);
    // [0] 流式尾巴:只画最后 K 行(已完段落随 Superstep 静态提交进历史)。
    // 分道显示:**回答**(白·粗)恒显且优先占行;其上用剩余行显**思考**(灰·暗),
    // 免把回答当思考、也不让长思考挤掉回答。回答空(仍在思考)时全用来显思考。
    let k = outer[0].height as usize;
    let answer_lines: Vec<Line> = stream_tail(&ui.stream, k)
        .into_iter()
        .map(|s| {
            Line::from(Span::styled(
                s.to_owned(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ))
        })
        .collect();
    let mut tail: Vec<Line> = Vec::with_capacity(k);
    let remaining = k.saturating_sub(answer_lines.len());
    if remaining > 0 && !ui.reasoning.is_empty() {
        for s in stream_tail(&ui.reasoning, remaining) {
            tail.push(Line::from(Span::styled(
                s.to_owned(),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM | Modifier::ITALIC),
            )));
        }
    }
    tail.extend(answer_lines);
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
    let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"][ui.frame % 10];
    let badge_bg = if ui.busy { Color::Yellow } else { Color::Cyan };
    let mut above: Vec<Span> = vec![Span::styled(
        " RidgeCode ",
        Style::default()
            .fg(Color::Black)
            .bg(badge_bg)
            .add_modifier(Modifier::BOLD),
    )];
    if agent::allow_jailbreak() {
        above.push(Span::styled(
            " ⚠JAILBREAK ",
            Style::default()
                .fg(Color::Black)
                .bg(role_color(Role::Error))
                .add_modifier(Modifier::BOLD),
        ));
    }
    if ui.busy {
        above.push(Span::styled(
            format!(
                " {spinner} {}",
                fmt_busy_bar(
                    &ui.phase,
                    &ui.todos,
                    vitals.elapsed_s,
                    vitals.task_tokens,
                    vitals.rate,
                    vitals.queued,
                )
            ),
            Style::default()
                .fg(role_color(Role::Warn))
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        let todo = todo_progress(&ui.todos)
            .map(|(d, n)| format!(" · todo {d}/{n}"))
            .unwrap_or_default();
        above.push(Span::styled(
            format!(" ready{todo}"),
            Style::default().fg(role_color(Role::Muted)),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(above)).style(Style::default().bg(Color::DarkGray)),
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
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(role_color(Role::Border)))
                .title(" Input (Enter send · Shift/Alt+Enter newline · Tab complete) "),
        ),
        outer[2],
    );
    // 输入框下状态条(config `status_bar` 模板)—— **可换行**:高已按折行算好,`.wrap` 落多行。
    frame.render_widget(
        Paragraph::new(Text::from(below_text))
            .wrap(Wrap { trim: false })
            .style(
                Style::default()
                    .fg(role_color(Role::Muted))
                    .bg(Color::DarkGray),
            ),
        outer[3],
    );
    // 真光标(iter-27;iter-30 改按显示单元格列):CJK/emoji 宽字符落点精确,不再偏左。
    // 审批 / 交互页模态开时不落输入光标(iter-35)。
    if approval.is_none() && ui.panel.is_none() {
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
