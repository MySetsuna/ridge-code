use super::*;
#[test]
fn event_colours_are_semantic() {
    assert_eq!(event_color("verify: PASS"), Color::Green);
    assert_eq!(event_color("act: run_shell"), Color::Yellow);
}

#[test]
fn live_rail_uses_semantic_kind_and_focus_only() {
    assert_eq!(
        live_rail(LiveLineKind::Answer, false, None),
        Some(("┃", Role::Primary))
    );
    assert_eq!(
        live_rail(LiveLineKind::Reasoning, false, None),
        Some(("┌", Role::Muted))
    );
    assert_eq!(
        live_rail(LiveLineKind::ToolSummary, true, None),
        Some(("▌", Role::Primary))
    );
    assert_eq!(
        live_rail(LiveLineKind::ToolDetail, false, None),
        Some(("┆", Role::Muted))
    );
    assert_eq!(
        live_rail(
            LiveLineKind::ToolDetail,
            true,
            Some(LiveLineKind::ToolSummary)
        ),
        Some(("┆", Role::Primary))
    );
    assert_eq!(live_rail(LiveLineKind::Splash, false, None), None);
}

#[test]
fn stream_channel_badges_keep_actual_output_semantics() {
    assert_eq!(
        stream_channel_badge(LiveChannel::Reasoning),
        ("[THINK]", Role::Muted)
    );
    assert_eq!(
        stream_channel_badge(LiveChannel::Answer),
        ("[ANSWER]", Role::Primary)
    );
    assert_eq!(
        stream_channel_badge(LiveChannel::Tool),
        ("[TOOL]", Role::Info)
    );
}

#[test]
fn top_chrome_identifies_focused_tool_without_overrunning_width() {
    let mut ui = Ui {
        busy: true,
        phase: "acting".into(),
        ..Ui::default()
    };
    ui.push_tool(
        ToolBlock::from_lines(vec![
            ("read_file src/main.rs".into(), Color::Cyan),
            ("old detail".into(), Color::Gray),
        ])
        .expect("old tool"),
    );
    ui.push_tool(
        ToolBlock::from_lines(vec![("write_file src/lib.rs".into(), Color::Cyan)])
            .expect("current tool"),
    );
    assert!(ui.transcript.move_tool_focus(-1));
    let vitals = Vitals {
        step: 2,
        elapsed_s: 3,
        task_tokens: 40,
        rate: 12,
        ctx_used: 0,
        queued: 0,
    };

    for width in [64, 48, 32] {
        let line = top_chrome(&ui, &vitals, width);
        let text = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(str_cells(&text) <= width as usize, "width={width}: {text}");
        if width >= 48 {
            assert!(
                text.contains("read_file src/main.rs"),
                "width={width}: {text}"
            );
        } else {
            assert!(!text.contains("read_file"), "width={width}: {text}");
        }
    }
}

#[test]
fn top_chrome_surfaces_reasoning_visibility_without_tools() {
    let mut ui = Ui {
        busy: true,
        phase: "answering".into(),
        ..Ui::default()
    };
    ui.push_chunk(provider::StreamChunk::Reasoning("r0\nr1".into()));
    ui.push_chunk(provider::StreamChunk::Answer("answer".into()));
    let vitals = Vitals {
        step: 2,
        elapsed_s: 3,
        task_tokens: 40,
        rate: 12,
        ctx_used: 0,
        queued: 0,
    };

    for width in [96, 64, 48, 32] {
        let line = top_chrome(&ui, &vitals, width);
        let text = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(str_cells(&text) <= width as usize, "width={width}: {text}");
        if width >= 48 {
            assert!(text.contains("THINK"), "width={width}: {text}");
        } else {
            assert!(!text.contains("THINK"), "width={width}: {text}");
        }
    }

    assert!(ui.toggle_reasoning());
    let expanded = top_chrome(&ui, &vitals, 96)
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(expanded.contains("Ctrl+R collapse"), "{expanded}");
}

#[test]
fn reasoning_answer_transition_rail_is_bounded() {
    assert_eq!(
        live_rail(
            LiveLineKind::Reasoning,
            false,
            Some(LiveLineKind::ToolSummary)
        ),
        Some(("┌", Role::Muted))
    );
    assert_eq!(
        live_rail(
            LiveLineKind::Reasoning,
            false,
            Some(LiveLineKind::Reasoning)
        ),
        Some(("│", Role::Muted))
    );
    assert_eq!(
        live_rail(LiveLineKind::Answer, false, Some(LiveLineKind::Reasoning)),
        Some(("╰", Role::Primary))
    );
    assert_eq!(
        live_rail(LiveLineKind::Answer, false, Some(LiveLineKind::ToolSummary)),
        Some(("╰", Role::Primary))
    );
    assert_eq!(
        live_rail(LiveLineKind::Answer, false, Some(LiveLineKind::ToolDetail)),
        Some(("╰", Role::Primary))
    );
    assert_eq!(
        live_rail(
            LiveLineKind::ToolSummary,
            true,
            Some(LiveLineKind::Reasoning)
        ),
        Some(("├", Role::Primary))
    );
    for rail in ["┌", "│", "╰", "┃", "├", "▌", "┆"] {
        assert_eq!(
            str_cells(rail),
            1,
            "rail must cost one display cell: {rail}"
        );
    }
}

#[test]
fn reasoning_tool_answer_connector_rail_is_bounded() {
    assert_eq!(
        live_rail(
            LiveLineKind::ToolSummary,
            true,
            Some(LiveLineKind::Reasoning)
        ),
        Some(("├", Role::Primary))
    );
    assert_eq!(
        live_rail(LiveLineKind::Answer, false, Some(LiveLineKind::ToolDetail)),
        Some(("╰", Role::Primary))
    );
    for rail in ["├", "╰"] {
        assert_eq!(
            str_cells(rail),
            1,
            "connector rail must cost one display cell"
        );
    }
}

#[test]
fn tool_failure_rail_uses_existing_error_role() {
    assert_eq!(
        live_tool_rail_role(
            LiveLineKind::ToolSummary,
            role_color(Role::Error),
            Role::Primary
        ),
        Role::Error
    );
    assert_eq!(
        live_tool_rail_role(
            LiveLineKind::ToolDetail,
            role_color(Role::Error),
            Role::Muted
        ),
        Role::Error
    );
    assert_eq!(
        live_tool_rail_role(
            LiveLineKind::ToolSummary,
            role_color(Role::Info),
            Role::Info
        ),
        Role::Info
    );
    assert_eq!(
        live_tool_rail_role(LiveLineKind::Answer, role_color(Role::Error), Role::Primary),
        Role::Primary
    );
}

#[test]
fn answer_header_anchor_preserves_budget_and_fence_boundary() {
    let mut transcript = LiveTranscript::default();
    transcript.push_answer("answer header\nline 1\nline 2\nline 3\nline 4\nline 5");
    let lines = transcript.visible_lines(5);
    assert_eq!(lines.len(), 5);
    assert_eq!(lines[0].text, "answer header");
    assert_eq!(lines[1].text, "  … answer continues");
    assert_eq!(lines.last().map(|line| line.text), Some("line 5"));
    assert_eq!(lines[0].marker, Some("🤖 "));

    let mut fenced = LiveTranscript::default();
    fenced.push_answer("```rust\nline 1\nline 2\nline 3\nline 4\nline 5");
    let fenced_lines = fenced.visible_lines(5);
    assert_eq!(fenced_lines[0].text, "```rust");
    assert_eq!(fenced_lines[1].kind, LiveLineKind::Answer);
    assert_eq!(fenced_lines.len(), 5);
}

#[test]
fn reasoning_tail_marks_hidden_prefix_without_extra_row() {
    let mut transcript = LiveTranscript::default();
    transcript.push_reasoning("r0\nr1\nr2");
    let lines = transcript.visible_lines(2);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].text, "r1");
    assert!(lines[0].continuation_before);
    assert!(!lines[1].continuation_before);

    let mut complete = LiveTranscript::default();
    complete.push_reasoning("r0\nr1");
    assert!(complete
        .visible_lines(2)
        .iter()
        .all(|line| !line.continuation_before));
}

#[test]
fn active_reasoning_tail_focus_is_render_only() {
    assert_eq!(
        active_reasoning_tail_role(LiveLineKind::Reasoning, true, true),
        Some(Role::Primary)
    );
    assert_eq!(
        active_reasoning_tail_role(LiveLineKind::Reasoning, true, false),
        Some(Role::Muted)
    );
    assert_eq!(
        active_reasoning_tail_role(LiveLineKind::Reasoning, false, true),
        Some(Role::Muted)
    );
    assert_eq!(
        active_reasoning_tail_role(LiveLineKind::Answer, true, true),
        None
    );
}

#[test]
fn live_code_fence_rail_is_bounded() {
    assert_eq!(
        live_code_rail(false, true),
        Some(("\u{251c}", Role::Border))
    );
    assert_eq!(live_code_rail(true, false), Some(("\u{250a}", Role::Muted)));
    assert_eq!(live_code_rail(true, true), Some(("\u{251c}", Role::Border)));
    assert_eq!(live_code_rail(false, false), None);
    for rail in ["\u{251c}", "\u{250a}"] {
        assert_eq!(str_cells(rail), 1, "code rail must cost one display cell");
    }
}
#[test]
fn final_answer_gets_assistant_marker() {
    assert_eq!(format_event_plain("(final) hello"), "🤖 hello");
    assert_eq!(
        format_event_plain("reason#2: (final) **hello**"),
        "🤖 **hello**"
    );
    assert!(is_final_event("reason#2: (final) hello"));
    assert!(!is_final_event("reason#2: tool_call search {}"));
}

/// iter-50:输出流总览化 —— 读只显路径、读回执丢内容、改显 ± diff、写显预览。
#[test]
fn display_text_strips_terminal_escape_sequences() {
    let text = "\x1b[31mred\x1b[0m\x1b]8;;https://example.invalid\x07link\x1b]8;;\x07";
    let clean = sanitize_display_text(text);
    assert_eq!(clean, "redlink");
    assert!(!clean.contains('\x1b'));
}

#[test]
fn malformed_escape_sequences_recover_at_line_boundaries() {
    for text in [
        "prefix\x1b[31\nsuffix",
        "prefix\x1b]8;;url\nsuffix",
        "prefix\u{9b}31\nsuffix",
        "prefix\u{9d}url\nsuffix",
    ] {
        assert_eq!(sanitize_display_text(text), "prefix\nsuffix", "{text:?}");
    }
}

#[test]
fn summarize_event_overviews_tools() {
    // 读:只显路径,不倒内容。
    let r = summarize_event(r#"reason#1: tool_call read_file {"path":"src/x.rs"}"#);
    assert_eq!(r.len(), 1);
    assert!(r[0].0.contains("读 src/x.rs"), "{}", r[0].0);
    // 读回执:丢内容,只回执字数。
    let a = summarize_event("act: read_file -> 一二三四五");
    assert!(a[0].0.contains("读完"), "{}", a[0].0);
    assert!(!a[0].0.contains("一二三"), "内容不应回显");
    // 改:git-diff 式 ± 行,红减绿增。
    let e = summarize_event(
        r#"reason#2: tool_call edit_file {"path":"a.rs","old_string":"let n=1;","new_string":"let n=2;"}"#,
    );
    assert!(e[0].0.contains("改 a.rs"), "{}", e[0].0);
    assert!(e
        .iter()
        .any(|(l, c)| l.starts_with("  - ") && l.contains("n=1") && *c == role_color(Role::Error)));
    assert!(e.iter().any(|(l, c)| l.starts_with("  + ")
        && l.contains("n=2")
        && *c == role_color(Role::Success)));
    // 写:路径 + 内容预览行。
    let w = summarize_event(
        r#"reason#3: tool_call write_file {"path":"b.rs","contents":"line1\nline2"}"#,
    );
    assert!(w[0].0.contains("写 b.rs"), "{}", w[0].0);
    assert!(w.iter().any(|(l, _)| l.contains("line1")));
    // 失败观察:显红 ✗(非绿 ✓)+ 多行错误正文(非只首行),别把报错藏掉。
    let f = summarize_event(
        "act: run_shell -> exit 1: compiling\nerror: cannot find `foo`\n  --> src/x.rs:3",
    );
    assert!(f[0].0.starts_with("  ✗ run_shell"), "失败应显 ✗:{}", f[0].0);
    assert_eq!(f[0].1, role_color(Role::Error), "失败头行应红");
    assert!(
        f.iter().any(|(l, _)| l.contains("cannot find `foo`")),
        "报错正文续行须显示,不能只留首行:{f:?}"
    );
    // 被拦截 / 拒绝也算失败,显红 ✗。
    let b = summarize_event("act: run_shell -> BLOCKED (dangerous: rm -rf /) —— 拒绝执行");
    assert!(
        b[0].0.starts_with("  ✗ run_shell"),
        "BLOCKED 应显 ✗:{}",
        b[0].0
    );

    // 批量编辑:折叠摘要显有界文件清单,详情沿用既有 ± 语义色,不读磁盘。
    let batch = summarize_event(
        r#"reason#4: tool_call apply_edits {"edits":[{"path":"src/a.rs","old_string":"a","new_string":"A"},{"path":"src/b.rs","old_string":"b","new_string":"B"},{"path":"src/c.rs","old_string":"c","new_string":"C"},{"path":"src/d.rs","old_string":"d","new_string":"D"}]}"#,
    );
    assert!(batch[0].0.contains("4 文件 / 4 处"), "{batch:?}");
    assert!(batch[0].0.contains("src/a.rs"), "{batch:?}");
    assert!(batch[0].0.contains("src/c.rs"), "{batch:?}");
    assert!(batch[0].0.contains("… +1 个"), "摘要路径须有界:{batch:?}");
    assert!(!batch[0].0.contains("src/d.rs"), "摘要不应溢出:{batch:?}");
    assert!(batch
        .iter()
        .any(|(line, color)| { line.starts_with("  - ") && *color == role_color(Role::Error) }));
    assert!(batch
        .iter()
        .any(|(line, color)| { line.starts_with("  + ") && *color == role_color(Role::Success) }));
}

/// iter-29:上下文窗口人读化。
#[test]
fn ctx_size_is_human_readable() {
    assert_eq!(fmt_ctx(128_000), "128K");
    assert_eq!(fmt_ctx(200_000), "200K");
    assert_eq!(fmt_ctx(1_048_576), "1.0M");
    assert_eq!(fmt_ctx(512), "512");
}

/// iter-31:状态双栏纯函数(零 wall-clock/PTY,计时/计量全由入参给定)。
#[test]
fn token_rate_guards_div_zero_and_scales() {
    assert_eq!(token_rate(0, 0), 0); // 未起步:防除零
    assert_eq!(token_rate(100, 1000), 100); // 100 tok / 1s = 100 tok/s
    assert_eq!(token_rate(50, 2000), 25); // 50 tok / 2s = 25 tok/s
}

#[test]
fn reasoning_meta_omits_unobserved_step_and_keeps_real_measurements() {
    assert_eq!(fmt_reasoning_meta(0, 2, 8), "💭 [t+2s · 8 task tok] ");
    assert_eq!(
        fmt_reasoning_meta(3, 12, 34),
        "💭 [step 3 · t+12s · 34 task tok] "
    );
}

#[test]
fn ctx_percent_clamps_and_guards() {
    assert_eq!(ctx_percent(0, 200_000), 0);
    assert_eq!(ctx_percent(6_000, 200_000), 3);
    assert_eq!(ctx_percent(999_999, 100), 100); // 超窗封顶
    assert_eq!(ctx_percent(500, 0), 0); // 窗口未知:防除零
}

#[test]
fn context_pressure_role_has_deterministic_boundaries() {
    assert_eq!(context_pressure_role(79), Role::Muted);
    assert_eq!(context_pressure_role(80), Role::Warn);
    assert_eq!(context_pressure_role(94), Role::Warn);
    assert_eq!(context_pressure_role(95), Role::Error);
    assert_eq!(context_pressure_role(100), Role::Error);
}

#[test]
fn busy_bar_omits_todo_when_empty_and_shows_when_present() {
    let none = fmt_busy_bar("reasoning", &[], 12, 340, 28, 0, None);
    assert_eq!(none, "⚡ reasoning · ⏱ 12s · 340 tok · 28 tok/s");
    let todos = vec![
        Todo {
            content: "a".into(),
            status: "completed".into(),
        },
        Todo {
            content: "b".into(),
            status: "in_progress".into(),
        },
    ];
    let with = fmt_busy_bar("acting", &todos, 3, 10, 3, 0, None);
    assert_eq!(with, "⚡ acting · ⏱ 3s · 10 tok · 3 tok/s · todo 1/2");
}

/// iter-33:忙碌粘条显待跑队列深度(纯函数)。
#[test]
fn busy_bar_shows_queue_depth() {
    assert_eq!(
        fmt_busy_bar("reasoning", &[], 5, 100, 20, 0, None),
        "⚡ reasoning · ⏱ 5s · 100 tok · 20 tok/s"
    );
    assert_eq!(
        fmt_busy_bar("reasoning", &[], 5, 100, 20, 2, None),
        "⚡ reasoning · ⏱ 5s · 100 tok · 20 tok/s · ⏳2"
    );
}

#[test]
fn busy_bar_shows_observed_step_only_when_available() {
    assert_eq!(fmt_busy_phase("reasoning", 0).as_ref(), "reasoning");

    assert_eq!(
        fmt_busy_phase("reasoning", 4).as_ref(),
        "reasoning · step 4"
    );
}

#[test]
fn busy_bar_projects_bounded_safe_tool_intent() {
    let call = provider::ToolCall {
        id: "1".into(),
        name: "read_file".into(),
        arguments: serde_json::json!({
            "path": "C:\\very\\long\\project\\private\\settings.toml\nnext",
            "contents": "api_key=should-not-render"
        }),
    };
    let text = fmt_busy_bar("acting", &[], 3, 10, 3, 0, Some(&call));
    assert!(text.contains("◈ read_file"), "{text}");
    assert!(text.contains("path=C:"), "{text}");
    assert!(text.contains('…'), "path should be clipped: {text}");
    assert!(
        !text.contains("api_key"),
        "content must stay hidden: {text}"
    );
    assert!(!text.contains('\n'));

    let sensitive = provider::ToolCall {
        id: "2".into(),
        name: "read_file".into(),
        arguments: serde_json::json!({"path": "C:\\secrets\\api_key.txt"}),
    };
    let sensitive_text = fmt_busy_bar("acting", &[], 3, 10, 3, 0, Some(&sensitive));
    assert!(
        sensitive_text.contains("path=[redacted]"),
        "{sensitive_text}"
    );
    assert!(!sensitive_text.contains("api_key"), "{sensitive_text}");
}

fn chrome(
    busy: bool,
    queued: usize,
    width: u16,
    reasoning_expanded: bool,
    has_reasoning: bool,
    has_tools: bool,
    has_history: bool,
) -> (String, Role) {
    input_chrome(InputChromeArgs {
        busy,
        queued,
        width,
        reasoning_expanded,
        has_reasoning,
        has_tools,
        has_history,
        has_scrollable_tool_details: false,
    })
}

#[test]
fn input_chrome_exposes_submit_or_queue_mode() {
    let (idle, idle_role) = chrome(false, 0, 80, false, true, false, false);
    assert!(idle.contains("Input"));
    assert!(idle.contains("Ctrl+R reasoning"));
    assert!(!idle.contains("Ctrl+O"));
    assert!(!idle.contains("Alt+↑/↓ focus"));
    assert_eq!(idle_role, Role::Primary);

    let (queued, queued_role) = chrome(true, 2, 80, false, true, false, false);
    assert!(queued.contains("Queue [2]"));
    assert!(queued.contains("Ctrl+R reasoning"));
    assert!(!queued.contains("Ctrl+O"));
    assert_eq!(queued_role, Role::Warn);

    let (idle_history, _) = chrome(false, 0, 80, false, true, false, true);
    assert!(idle_history.contains("Ctrl+O history"));
    assert!(!idle_history.contains("Ctrl+O details"));

    let (busy_tools, _) = chrome(true, 2, 80, false, true, true, false);
    assert!(busy_tools.contains("Queue [2]"));
    assert!(busy_tools.contains("Alt+↑/↓ focus"));
    assert!(busy_tools.contains("Ctrl+O details"));

    let (busy_tools_expanded, _) = chrome(true, 2, 80, true, true, true, false);
    assert!(busy_tools_expanded.contains("Ctrl+R collapse"));
    assert!(!busy_tools_expanded.contains("Ctrl+R reasoning"));
    let (busy_tools_scrolled, _) = input_chrome(InputChromeArgs {
        busy: true,
        queued: 2,
        width: 160,
        reasoning_expanded: true,
        has_reasoning: true,
        has_tools: true,
        has_history: false,
        has_scrollable_tool_details: true,
    });
    assert!(busy_tools_scrolled.contains("Alt+PgUp/PgDn scroll"));

    let (wide_busy_tools, _) = chrome(true, 2, 96, false, true, true, false);
    assert!(wide_busy_tools.contains("Ctrl+R reasoning"));
    assert!(wide_busy_tools.contains("Alt+↑/↓ focus"));
    assert!(wide_busy_tools.contains("Ctrl+O details"));

    let (expanded, _) = chrome(false, 0, 80, true, true, true, false);
    assert!(expanded.contains("Ctrl+R collapse"));
    assert!(!expanded.contains("Ctrl+R reasoning"));

    let (wide_idle, _) = chrome(false, 0, 96, false, true, true, false);
    assert!(wide_idle.contains("Alt+↑/↓ focus"));

    let (medium_without_tools, _) = chrome(true, 10, 64, false, true, false, false);
    assert!(!medium_without_tools.contains("Alt+↑/↓ focus"));

    let (medium_with_tools, _) = chrome(true, 10, 64, false, true, true, false);
    assert!(medium_with_tools.contains("Alt+↑/↓ focus"));
    assert!(medium_with_tools.contains("Ctrl+O details"));
    assert!(medium_with_tools.contains("Ctrl+R"));

    let (narrow_medium_with_tools, _) = chrome(true, 10, 56, false, true, true, false);
    assert!(narrow_medium_with_tools.contains("Ctrl+O details"));

    let (medium_with_tools_expanded, _) = chrome(true, 10, 64, true, true, true, false);
    assert!(medium_with_tools_expanded.contains("Ctrl+R collapse"));

    let (narrow, narrow_role) = chrome(true, 10, 15, false, false, true, false);
    assert_eq!(narrow, " Q:[10] ^O ");
    assert_eq!(narrow_role, Role::Warn);
    assert!(str_cells(&narrow) <= 13);

    let (narrow_idle_tools, _) = chrome(false, 0, 15, false, true, true, false);
    assert!(narrow_idle_tools.contains("^O"), "{narrow_idle_tools}");
    assert!(str_cells(&narrow_idle_tools) <= 13);

    let (compact_tools_and_reasoning, _) = chrome(false, 0, 18, false, true, true, false);
    assert!(
        compact_tools_and_reasoning.contains("^O"),
        "{compact_tools_and_reasoning}"
    );
    assert!(
        compact_tools_and_reasoning.contains("^R"),
        "{compact_tools_and_reasoning}"
    );
    assert!(str_cells(&compact_tools_and_reasoning) <= 16);

    let (narrow_history, _) = chrome(false, 0, 15, false, false, false, true);
    assert!(narrow_history.contains("^O"), "{narrow_history}");
    assert!(str_cells(&narrow_history) <= 13);
}

#[test]
fn reasoning_hint_tracks_actual_content_at_narrow_widths() {
    let (none, _) = chrome(false, 0, 80, false, false, false, false);
    assert!(!none.contains("Ctrl+R"));
    assert!(!none.contains("^R"));

    let (wide, _) = chrome(false, 0, 80, false, true, false, false);
    assert!(wide.contains("Ctrl+R reasoning"));

    let (compact, _) = chrome(false, 0, 18, false, true, false, false);
    assert!(compact.contains("Ctrl+R"), "{compact}");

    let (tiny, _) = chrome(true, 2, 15, false, true, false, false);
    assert!(tiny.contains("^R"), "{tiny}");

    let (expanded, _) = chrome(false, 0, 80, true, true, false, false);
    assert!(expanded.contains("Ctrl+R collapse"));
}

#[test]
fn status_template_substitutes_known_and_keeps_unknown() {
    let v = StatusVars {
        provider: "anthropic".into(),
        model: "opus".into(),
        ctx: "12%".into(),
        tokens: "500".into(),
        cwd: "ridge-code".into(),
    };
    assert_eq!(
        render_status_template(" {provider} · {model} · ctx {ctx} · {tokens} tok ", &v),
        " anthropic · opus · ctx 12% · 500 tok "
    );
    // 未知占位原样保留,不吞字符。
    assert_eq!(
        render_status_template("{branch}/{cwd}", &v),
        "{branch}/ridge-code"
    );
    // 无占位原样。
    assert_eq!(render_status_template("plain", &v), "plain");
}

/// est_tokens 跨 crate 可见(ctx% 分子复用同一估算口径)。
#[test]
fn est_tokens_is_public() {
    assert!(est_tokens("你好abcd") >= 1);
}

/// iter-35:交互 Panel 纯函数。
fn mi(id: &str, ctx: Option<u64>) -> provider::models::ModelInfo {
    provider::models::ModelInfo {
        id: id.into(),
        context: ctx,
    }
}
fn prow(key: &str, value: &str) -> PanelRow {
    PanelRow {
        key: key.into(),
        value: value.into(),
        ctx: None,
    }
}

#[test]
fn panel_filter_substring_case_insensitive() {
    let rows = vec![
        prow("model", "opus"),
        prow("provider", "anthropic"),
        prow("base_url", "x"),
    ];
    assert_eq!(panel_filter(&rows, "").len(), 3); // 空 query 全含
    assert_eq!(panel_filter(&rows, "MOD"), vec![0]); // 命中 key,大小写无关
    assert_eq!(panel_filter(&rows, "anthropic"), vec![1]); // 命中 value
    assert!(panel_filter(&rows, "zzz").is_empty()); // 无命中
}

#[test]
fn panel_nav_and_retype_clamp() {
    let rows = vec![prow("a", ""), prow("ab", ""), prow("abc", "")];
    let mut p = Panel::new(PanelKind::Tools, "t".into(), rows);
    p.sel = 2;
    p.move_down(); // 已在末,不越界
    assert_eq!(p.sel, 2);
    p.query = "abc".into();
    p.retype(); // view 缩到 1 项,sel 钳回
    assert_eq!(p.view.len(), 1);
    assert_eq!(p.sel, 0);
    p.move_up();
    assert_eq!(p.sel, 0); // 已在首,不越界
    p.page_down();
    assert_eq!(p.sel, 0); // 过滤后只有一项,分页不越界
    p.query.clear();
    p.retype();
    p.page_down();
    assert_eq!(p.sel, 2);
    p.first();
    assert_eq!(p.sel, 0);
    p.last();
    assert_eq!(p.sel, 2);
}

#[test]
fn tool_history_search_opens_and_positions_detail() {
    let mut history = VecDeque::new();
    history.push_back(
        ToolBlock::from_lines(
            (0..8)
                .map(|index| {
                    if index == 7 {
                        ("needle at the end".into(), Color::Gray)
                    } else {
                        (format!("detail {index}"), Color::Gray)
                    }
                })
                .collect(),
        )
        .expect("tool history"),
    );
    let mut panel = tool_history_panel(&history);
    panel.query = "needle".into();
    panel.retype();
    assert!(panel.detail_open);
    assert!(panel
        .selected()
        .is_some_and(|row| row.value.contains("needle")));
    assert_eq!(
        detail_match_scroll("zero\none\nneedle\nlast", "needle", 40, 2),
        1
    );

    let mut terminal =
        Terminal::new(ratatui::backend::TestBackend::new(40, 12)).expect("history search terminal");
    terminal
        .draw(|frame| draw_panel(frame, frame.area(), &panel))
        .expect("history search draw");
    let symbols = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        symbols.contains("needle"),
        "matched detail not visible: {symbols}"
    );
}

#[test]
fn narrow_frame_retains_context_and_token_status() {
    let ui = Ui::default();
    let meta = ReplMeta {
        tools: Vec::new(),
        provider: "test".into(),
        model: "model".into(),
        base_url: String::new(),
        status_bar: "ctx {ctx} · {tokens} tok".into(),
        ctx_window: 200_000,
    };
    let vitals = Vitals {
        step: 0,
        elapsed_s: 0,
        task_tokens: 0,
        rate: 0,
        ctx_used: 160_000,
        queued: 0,
    };
    for width in [40, 32, 18] {
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(width, 10))
            .expect("narrow telemetry terminal");
        terminal
            .draw(|frame| draw(frame, &ui, &meta, 12_345, &vitals, None))
            .expect("narrow telemetry draw");
        let symbols = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            symbols.contains("ctx"),
            "context telemetry hidden at {width}: {symbols}"
        );
        assert!(
            symbols.contains("12_345") || symbols.contains("12,345") || symbols.contains("12345"),
            "token telemetry hidden at {width}: {symbols}"
        );
        assert!(
            symbols.contains("tok"),
            "token unit hidden at {width}: {symbols}"
        );
    }
}

#[test]
fn config_panel_lists_all_config_keys() {
    let p = config_panel();
    let keys: Vec<&str> = p.rows.iter().map(|r| r.key.as_str()).collect();
    for k in agent::CONFIG_KEYS {
        assert!(keys.contains(k), "配置页缺键 {k}");
    }
    assert_eq!(p.rows.len(), agent::CONFIG_KEYS.len());
}

/// 登录页(iter-38):列 Claude OAuth 入口 + 全部内置 preset,kind 为 Login。
#[test]
fn login_panel_lists_all_presets() {
    let p = login_panel();
    assert_eq!(p.kind, PanelKind::Login);
    assert_eq!(p.rows.len(), PROVIDER_PRESETS.len() + 2);
    let keys: Vec<&str> = p.rows.iter().map(|r| r.key.as_str()).collect();
    assert!(keys.contains(&CLAUDE_OAUTH_ROW));
    assert!(keys.contains(&CODEX_OAUTH_ROW));
    assert!(keys.contains(&"openai"));
    for r in p
        .rows
        .iter()
        .filter(|r| r.key != CLAUDE_OAUTH_ROW && r.key != CODEX_OAUTH_ROW)
    {
        assert!(
            preset_by_id(&r.key).is_some(),
            "登录页行 key 非 preset id: {}",
            r.key
        );
    }
}

#[test]
fn models_panel_selects_current() {
    let grouped: Vec<(String, Vec<provider::models::ModelInfo>)> = vec![(
        "test".into(),
        vec![
            mi("a", Some(128_000)),
            mi("b", Some(200_000)),
            mi("c", None),
        ],
    )];
    let p = models_panel(&grouped, "test", "b");
    assert_eq!(p.kind, PanelKind::Models);
    // key 格式: "provider · model_id"
    assert_eq!(p.selected().map(|r| r.key.as_str()), Some("test · b"));
    assert_eq!(p.rows[0].ctx, Some(128_000)); // 携真实窗口供选中缓存
    assert!(p.rows[2].value.contains('?')); // 缺 ctx 显 ?
}

/// iter-35:斜杠即弹 —— 打 `/` 现全表、`/mo` 滤到 `/model`(iter-37 合并后 `/models` 退出补全表)。
#[test]
fn slash_popup_lists_all_and_filters() {
    let mut all = InputState::default();
    all.insert_str("/");
    let p = build_popup(&all).expect("打 / 应现全部命令");
    assert_eq!(p.items.len(), SLASH_COMMANDS.len());
    let mut mo = InputState::default();
    mo.insert_str("/mo");
    let f = build_popup(&mo).expect("应有候选");
    assert_eq!(f.items, vec!["/model".to_string()]);
}

#[test]
fn panel_action_routes_keys() {
    let press = |code| KeyEvent::new(code, KeyModifiers::NONE);
    assert_eq!(panel_action(&press(KeyCode::Up)), PanelAction::Up);
    assert_eq!(panel_action(&press(KeyCode::Down)), PanelAction::Down);
    assert_eq!(panel_action(&press(KeyCode::PageUp)), PanelAction::PageUp);
    assert_eq!(
        panel_action(&press(KeyCode::PageDown)),
        PanelAction::PageDown
    );
    assert_eq!(panel_action(&press(KeyCode::Home)), PanelAction::First);
    assert_eq!(panel_action(&press(KeyCode::End)), PanelAction::Last);
    assert_eq!(panel_action(&press(KeyCode::Enter)), PanelAction::Enter);
    assert_eq!(panel_action(&press(KeyCode::Esc)), PanelAction::Esc);
    assert_eq!(
        panel_action(&press(KeyCode::Char('x'))),
        PanelAction::Char('x')
    );
    assert_eq!(
        panel_action(&press(KeyCode::Backspace)),
        PanelAction::Backspace
    );
}

#[tokio::test]
async fn history_command_opens_bounded_tool_history() {
    let mut ui = Ui::default();
    ui.push_tool(
        ToolBlock::from_lines(vec![("  tool: search".into(), role_color(Role::Info))])
            .expect("tool"),
    );
    ui.commit_live_tools();
    let mut history = Vec::new();
    let mut meta = ReplMeta {
        tools: Vec::new(),
        provider: "test".into(),
        model: "model".into(),
        base_url: String::new(),
        status_bar: String::new(),
        ctx_window: 200_000,
    };
    let swap = Arc::new(provider::SwapProvider::new(Arc::new(
        provider::ScriptedProvider::new(Vec::new()),
    )));
    let agents = agent::Agents::default();

    let should_exit = run_command(
        "/history",
        &mut ui,
        &mut history,
        &mut meta,
        &swap,
        &agents,
        &[],
        &[],
        0,
        0,
    )
    .await
    .expect("history command");

    assert!(!should_exit);
    assert!(matches!(
        ui.panel.as_ref().map(|p| p.kind),
        Some(PanelKind::ToolHistory)
    ));
    assert!(ui.panel.as_ref().is_some_and(|p| p.rows.len() == 1));
    assert!(SLASH_COMMANDS.contains(&"/history"));
}

/// 根因回归(输入法吞空格):去重 Windows 双触发 + 兜住输入法「仅 Release」的字符注入 +
/// no-break(U+00A0)/全角(U+3000)空格归一。实测某输入法把空格键作为 `Char('\u{a0}')` 只发 Release,
/// 旧「只收 Press」把它整个丢弃 → 打不出空格。
#[test]
fn decide_key_dedups_and_recovers_ime_space() {
    use std::collections::HashSet;
    let mut p: HashSet<KeyCode> = HashSet::new();
    let press = |c| KeyEvent::new_with_kind(c, KeyModifiers::NONE, KeyEventKind::Press);
    let release = |c| KeyEvent::new_with_kind(c, KeyModifiers::NONE, KeyEventKind::Release);

    // 正常键:Press 处理;其后的 Release 丢弃(免 Windows 双触发)。
    assert_eq!(
        decide_key(&mut p, &press(KeyCode::Char('a'))).map(|k| k.code),
        Some(KeyCode::Char('a'))
    );
    assert!(decide_key(&mut p, &release(KeyCode::Char('a'))).is_none());

    // 输入法空格:Char('\u{a0}') 仅 Release(悬空)→ 收下,归一为普通空格、以 Press 呈现给下游。
    let k = decide_key(&mut p, &release(KeyCode::Char('\u{a0}'))).expect("悬空字符 Release 应收下");
    assert_eq!(
        k.code,
        KeyCode::Char(' '),
        "no-break space 应归一为普通空格"
    );
    assert_eq!(k.kind, KeyEventKind::Press, "应以 Press 呈现给下游");
    assert_eq!(
        decide_key(&mut p, &release(KeyCode::Char('\u{3000}')))
            .unwrap()
            .code,
        KeyCode::Char(' '),
        "全角空格同样归一"
    );

    // 悬空的**非字符** Release(如启动残留的 Enter 松键)→ 忽略,不误触发 Submit。
    assert!(decide_key(&mut p, &release(KeyCode::Enter)).is_none());

    // Unix 口径:只有 Press、无 Release,普通空格照常处理。
    assert_eq!(
        decide_key(&mut p, &press(KeyCode::Char(' '))).map(|k| k.code),
        Some(KeyCode::Char(' '))
    );
}

/// 根因回归:审批态下滚动键**不再误拒**,而是滚动;仅 y/Enter 批准、n/Esc 拒绝,余键忽略。
#[test]
fn terminal_event_router_separates_paste_and_resize() {
    let paste = terminal_event_action(Event::Paste("a\r\nb".into()));
    let TerminalEventAction::Paste(text) = paste else {
        panic!("paste must stay outside key routing");
    };
    let mut ui = Ui {
        popup: Some(Popup {
            items: vec!["/help".into()],
            selected: 0,
            anchor: 0,
        }),
        ..Ui::default()
    };
    apply_paste(&mut ui, &text);
    assert_eq!(ui.input.buffer, "a\nb");
    assert!(ui.popup.is_none());
    assert!(matches!(
        terminal_event_action(Event::Resize(80, 24)),
        TerminalEventAction::Redraw
    ));
    assert!(matches!(
        terminal_event_action(Event::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE
        ))),
        TerminalEventAction::Key(_)
    ));
}

#[test]
fn approval_scroll_keys_do_not_reject() {
    assert_eq!(approval_action(KeyCode::Up), ApprovalAction::Scroll(1));
    assert_eq!(approval_action(KeyCode::Down), ApprovalAction::Scroll(-1));
    assert_eq!(approval_action(KeyCode::PageUp), ApprovalAction::Scroll(8));
    assert_eq!(
        approval_action(KeyCode::PageDown),
        ApprovalAction::Scroll(-8)
    );
    assert_eq!(approval_action(KeyCode::Char('y')), ApprovalAction::Approve);
    assert_eq!(approval_action(KeyCode::Enter), ApprovalAction::Approve);
    assert_eq!(approval_action(KeyCode::Char('n')), ApprovalAction::Reject);
    assert_eq!(approval_action(KeyCode::Esc), ApprovalAction::Reject);
    // 关键:随手一个字符键不再落「拒绝」,而是被忽略(等用户明确 y/n)。
    assert_eq!(approval_action(KeyCode::Char('x')), ApprovalAction::Ignore);
    assert_eq!(approval_action(KeyCode::Backspace), ApprovalAction::Ignore);
}

/// 滚动增量应用:上/下界饱和,不 panic。
#[test]
fn apply_scroll_saturates() {
    assert_eq!(apply_scroll(5, 1), 6);
    assert_eq!(apply_scroll(5, -1), 4);
    assert_eq!(apply_scroll(0, -8), 0);
    assert_eq!(apply_scroll(u16::MAX, 8), u16::MAX);
}

/// iter-27:主输入键位路由矩阵 —— Shift/Alt+Enter/Ctrl+J 换行,Up/Down 归光标/历史枢纽,
/// busy 时 Enter 不提交,浮窗态 ↑↓/Tab/Enter/Esc 归浮窗、字符穿透,松键忽略。
#[test]
fn input_action_routes_keys() {
    let press = |code| KeyEvent::new(code, KeyModifiers::NONE);
    // 基本编辑
    assert_eq!(
        input_action(&press(KeyCode::Char('a')), false, false),
        InputAction::Insert('a')
    );
    assert_eq!(
        input_action(&press(KeyCode::Backspace), false, false),
        InputAction::Backspace
    );
    assert_eq!(
        input_action(&press(KeyCode::Left), false, false),
        InputAction::Left
    );
    assert_eq!(
        input_action(&press(KeyCode::End), false, false),
        InputAction::End
    );
    // 多行换行三键
    assert_eq!(
        input_action(
            &KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
            false,
            false
        ),
        InputAction::NewLine
    );
    assert_eq!(
        input_action(
            &KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT),
            false,
            false
        ),
        InputAction::NewLine
    );
    assert_eq!(
        input_action(
            &KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
            false,
            false
        ),
        InputAction::NewLine
    );
    // 提交/忽略
    assert_eq!(
        input_action(&press(KeyCode::Enter), false, false),
        InputAction::Submit
    );
    // busy 时 Enter 不再忽略 → 入队(iter-33)
    assert_eq!(
        input_action(&press(KeyCode::Enter), true, false),
        InputAction::Queue
    );
    let active_frontier = vec!["verify".to_owned()];
    assert!(superstep_is_busy(&active_frontier));
    assert_eq!(
        input_action(
            &press(KeyCode::Enter),
            superstep_is_busy(&active_frontier),
            false
        ),
        InputAction::Queue
    );
    assert!(!superstep_is_busy(&[]));
    assert_eq!(
        input_action(&press(KeyCode::Enter), superstep_is_busy(&[]), false),
        InputAction::Submit
    );
    assert!(can_start_task(false, false));
    assert!(!can_start_task(false, true));
    assert!(!can_start_task(true, false));
    let mut approval_ui = Ui::default();
    approval_ui.resume_after_approval();
    assert_eq!(
        input_action(&press(KeyCode::Enter), approval_ui.busy, false),
        InputAction::Queue
    );
    // 光标/历史枢纽 + 浮窗触发
    assert_eq!(
        input_action(&press(KeyCode::Up), false, false),
        InputAction::CursorUpOrHistory
    );
    assert_eq!(
        input_action(&press(KeyCode::Down), false, false),
        InputAction::CursorDownOrHistory
    );
    assert_eq!(
        input_action(&press(KeyCode::Tab), false, false),
        InputAction::PopupOpen
    );
    assert_eq!(
        input_action(
            &KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
            false,
            false
        ),
        InputAction::ToggleDetails
    );
    assert_eq!(
        input_action(
            &KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
            true,
            false
        ),
        InputAction::ToggleReasoning
    );
    assert_eq!(
        tool_focus_action(&KeyEvent::new(KeyCode::Up, KeyModifiers::ALT), false, true),
        Some(-1)
    );
    assert_eq!(
        tool_focus_action(
            &KeyEvent::new(KeyCode::Down, KeyModifiers::ALT),
            false,
            true
        ),
        Some(1)
    );
    assert_eq!(
        tool_focus_action(&KeyEvent::new(KeyCode::Up, KeyModifiers::ALT), true, true),
        None
    );
    assert_eq!(
        tool_detail_scroll_action(
            &KeyEvent::new(KeyCode::PageUp, KeyModifiers::ALT),
            false,
            true
        ),
        Some(1)
    );
    assert_eq!(
        tool_detail_scroll_action(
            &KeyEvent::new(KeyCode::PageDown, KeyModifiers::ALT),
            false,
            true
        ),
        Some(-1)
    );
    assert_eq!(
        tool_detail_scroll_action(
            &KeyEvent::new(KeyCode::PageUp, KeyModifiers::ALT),
            true,
            true
        ),
        None
    );
    // 浮窗态
    assert_eq!(
        input_action(&press(KeyCode::Tab), false, true),
        InputAction::PopupNext
    );
    assert_eq!(
        input_action(&press(KeyCode::Down), false, true),
        InputAction::PopupNext
    );
    assert_eq!(
        input_action(&press(KeyCode::Up), false, true),
        InputAction::PopupPrev
    );
    assert_eq!(
        input_action(&press(KeyCode::Enter), false, true),
        InputAction::PopupApply
    );
    assert_eq!(
        input_action(&press(KeyCode::Esc), false, true),
        InputAction::PopupClose
    );
    assert_eq!(
        input_action(&press(KeyCode::Char('x')), false, true),
        InputAction::Insert('x') // 字符穿透继续编辑
    );
    // 中断与松键
    assert_eq!(
        input_action(
            &KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
            false,
            true
        ),
        InputAction::Ignore
    );
    assert_eq!(
        input_action(
            &KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            true,
            true
        ),
        InputAction::Interrupt
    );
    assert_eq!(
        input_action(
            &KeyEvent::new_with_kind(
                KeyCode::Char('a'),
                KeyModifiers::NONE,
                KeyEventKind::Release
            ),
            false,
            false
        ),
        InputAction::Ignore
    );
}

/// iter-30:wcwidth 显示宽度 —— CJK/emoji 占 2 格,光标显示列按实占累加,折行按实占计。
#[test]
fn wcwidth_display_columns() {
    // 单字符 / 字符串单元格宽。
    assert_eq!(char_cells('a'), 1);
    assert_eq!(char_cells('你'), 2);
    assert_eq!(str_cells("ab你好"), 6); // 1+1+2+2
                                        // 折行:CJK 按实占,不再低估行数(3 个全角 = 6 格,宽 4 → 2 行,旧口径误判 1 行)。
    assert_eq!(wrapped_rows("你你你", 4), 2);
    assert_eq!(wrapped_rows("abcd", 4), 1);
    assert_eq!(clip_display_cells("abcdef", 4), "abc…");
    assert_eq!(clip_display_cells("你好a", 3), "你…");
    assert_eq!(clip_display_cells("你好", 1), "…");
}

/// iter-49:输入折行 + 光标同口径(修「文字换到第二行时光标卡首行末」根因)。
#[test]
fn wrap_input_cursor_follows_soft_wrap() {
    // 光标显示列:CJK 前缀按 2 格累加(宽足够不折行)。
    let (_, r, c) = wrap_input("你好a", 3, 80);
    assert_eq!((r, c), (0, 5)); // 2+2+1
    let (_, r, c) = wrap_input("你好a", 2, 80); // 光标在 'a' 前
    assert_eq!((r, c), (0, 4));
    // 显式换行:光标落第二逻辑行、列从 0 起。
    let (lines, r, c) = wrap_input("你\nb", 3, 80);
    assert_eq!((lines.len(), r, c), (2, 1, 1));
    // **软折行**(bug 修复):宽 2,"abcd" → ["ab","cd"];光标在末尾应落**第二可视行**列 2(此前卡首行)。
    let (lines, r, c) = wrap_input("abcd", 4, 2);
    assert_eq!(lines, vec!["ab", "cd"]);
    assert_eq!((r, c), (1, 2));
    // 空缓冲:一行、光标 (0,0)。
    let (lines, r, c) = wrap_input("", 0, 10);
    assert_eq!((lines.len(), r, c), (1, 0, 0));
}

/// iter-27:InputState 光标编辑 —— 插删/移动/多行上下列钳位/CJK 多字节安全。
#[test]
fn input_state_cursor_editing() {
    let mut s = InputState::default();
    for c in "hello".chars() {
        s.insert(c);
    }
    assert_eq!((s.buffer.as_str(), s.cursor), ("hello", 5));
    s.left();
    s.left();
    s.insert('X');
    assert_eq!(s.buffer, "helXlo");
    s.backspace();
    assert_eq!((s.buffer.as_str(), s.cursor), ("hello", 3));
    s.home();
    assert_eq!(s.cursor, 0);
    s.end();
    assert_eq!(s.cursor, 5);
    // 多行:上下移动 + 长短行列钳位
    s.insert('\n');
    s.insert_str("ab");
    assert_eq!(s.row_col(), (1, 2));
    assert!(s.move_up()); // 回 "hello" 行,列 2 保留
    assert_eq!(s.row_col(), (0, 2));
    s.end();
    assert!(s.move_down()); // "hello"(列 5) → "ab" 行,列钳到 2
    assert_eq!(s.row_col(), (1, 2));
    assert!(!s.move_down()); // 已是末行
                             // CJK 多字节安全
    let mut z = InputState::default();
    z.insert('中');
    z.insert('文');
    z.left();
    z.insert('间');
    assert_eq!(z.buffer, "中间文");
    z.home();
    z.end();
    assert_eq!(z.cursor, 3);
    z.insert('\n');
    z.insert('尾');
    z.cursor = 0;
    z.end();
    assert_eq!(z.cursor, 3, "End stops before newline using char offsets");
}

/// iter-27:历史召回 —— 首行 Up 进历史,draft 存取,Down 走出还原草稿。
#[test]
fn input_state_history_recall_preserves_draft() {
    let mut s = InputState::default();
    s.insert_str("first task");
    assert_eq!(s.take(), "first task");
    s.insert_str("second");
    assert_eq!(s.take(), "second");
    s.insert_str("dra"); // 打了一半的草稿
    assert!(!s.move_up()); // 单行首行 → 该走召回
    s.recall_prev();
    assert_eq!(s.buffer, "second");
    s.recall_prev();
    assert_eq!(s.buffer, "first task");
    s.recall_prev(); // 到顶不越界
    assert_eq!(s.buffer, "first task");
    s.recall_next();
    assert_eq!(s.buffer, "second");
    s.recall_next(); // 走出历史 → 还原草稿
    assert_eq!(s.buffer, "dra");
    assert_eq!(s.hist_idx, None);
}

/// iter-27:词提取 + 前缀过滤 + 应用替换 + build_popup 触发条件。
#[test]
fn completion_word_filter_and_apply() {
    assert_eq!(current_word("/mo", 3), (0, "/mo".to_string()));
    assert_eq!(current_word("fix @src/ma", 11), (4, "@src/ma".to_string()));
    assert_eq!(
        filter_prefix(SLASH_COMMANDS.iter().copied(), "/co"),
        vec![
            "/commands".to_string(),
            "/compact".to_string(),
            "/config".to_string(),
            "/cost".to_string()
        ]
    );
    assert!(filter_prefix(SLASH_COMMANDS.iter().copied(), "/zzz").is_empty());
    // 应用:整词替换,保留前后文,光标落补全尾
    let mut s = InputState::default();
    s.insert_str("run /mo now");
    s.cursor = 7; // "/mo" 之后
    let p = Popup {
        items: vec!["/model".to_string()],
        selected: 0,
        anchor: 4,
    };
    apply_completion(&mut s, &p);
    assert_eq!(s.buffer, "run /model now");
    assert_eq!(s.cursor, 10);
    // build_popup:行首 / 词才补命令;非行首不补
    let mut q = InputState::default();
    q.insert_str("/re");
    let pop = build_popup(&q).expect("应有候选");
    assert_eq!(pop.items, vec!["/reset".to_string()]);
    assert_eq!(pop.anchor, 0);
    let mut r = InputState::default();
    r.insert_str("say /re");
    assert!(build_popup(&r).is_none());
}

/// iter-23:重绘判定 —— 脏或显式动画帧需求才画,业务 busy 不直接触发重绘。
#[test]
fn draw_only_when_dirty_or_animation() {
    assert!(should_draw(true, false));
    assert!(should_draw(false, true));
    assert!(!should_draw(false, false));
}

#[test]
fn inline_viewport_height_uses_stable_cap() {
    assert_eq!(inline_height_cap(), 14);
}

#[test]
fn inline_viewport_tracks_terminal_resize() {
    let mut terminal = Terminal::with_options(
        ratatui::backend::TestBackend::new(40, 4),
        TerminalOptions {
            viewport: Viewport::Inline(inline_height_cap()),
        },
    )
    .expect("inline terminal");
    let mut areas = Vec::new();
    terminal
        .draw(|frame| {
            areas.push(frame.area());
            frame.render_widget(Paragraph::new("initial"), frame.area());
        })
        .expect("initial frame");

    terminal.backend_mut().resize(18, 20);
    terminal
        .draw(|frame| {
            areas.push(frame.area());
            frame.render_widget(Paragraph::new("resized"), frame.area());
        })
        .expect("resized frame");

    assert_eq!((areas[0].width, areas[0].height), (40, 4));
    assert_eq!((areas[1].width, areas[1].height), (18, 14));
}

#[test]
fn responsive_live_layout_preserves_output_and_input_under_vertical_pressure() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 14,
    };
    for height in 1..=14 {
        let area = Rect { height, ..area };
        let slots = responsive_live_layout(area, 8, 3);
        let mut next_y = area.y;
        for slot in slots {
            assert_eq!(slot.y, next_y, "slots must remain contiguous at {height}");
            next_y = next_y.saturating_add(slot.height);
        }
        assert_eq!(next_y, area.bottom(), "slots must fill {height} rows");
        if height > 0 {
            assert!(slots[0].height >= 1, "output floor disappeared at {height}");
        }
        if height >= 4 {
            assert_eq!(slots[1].height, 1, "chrome must stay visible at {height}");
        } else {
            assert_eq!(slots[1].height, 0, "chrome should collapse at {height}");
        }
        if height >= 6 {
            assert!(
                slots[3].height >= 1,
                "status should remain visible at {height}"
            );
        } else {
            assert_eq!(slots[3].height, 0, "status should yield at {height}");
        }
    }

    let meta = ReplMeta {
        tools: Vec::new(),
        provider: "test".into(),
        model: "model".into(),
        base_url: String::new(),
        status_bar: "status".into(),
        ctx_window: 200_000,
    };
    let vitals = Vitals {
        step: 0,
        elapsed_s: 0,
        task_tokens: 0,
        rate: 0,
        ctx_used: 0,
        queued: 0,
    };
    for height in [4, 5, 7] {
        let mut ui = Ui::default();
        ui.push_chunk(provider::StreamChunk::Answer("answer survives".into()));
        ui.input.insert_str("draft");
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(24, height))
            .expect("responsive live terminal");
        terminal
            .draw(|frame| draw(frame, &ui, &meta, 0, &vitals, None))
            .expect("responsive live draw");
        let symbols = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            symbols.contains("answer survives"),
            "answer lost at {height}: {symbols}"
        );
        assert!(
            symbols.contains("Input"),
            "input chrome lost at {height}: {symbols}"
        );
        if height >= 6 {
            assert!(
                symbols.contains("status"),
                "status lost at {height}: {symbols}"
            );
        }
    }
}

#[test]
fn tiny_frames_keep_input_slot_visible() {
    let meta = ReplMeta {
        tools: Vec::new(),
        provider: "test".into(),
        model: "model".into(),
        base_url: String::new(),
        status_bar: "test".into(),
        ctx_window: 200_000,
    };
    let vitals = Vitals {
        step: 1,
        elapsed_s: 1,
        task_tokens: 1,
        rate: 1,
        ctx_used: 1,
        queued: 0,
    };
    for (width, height) in [(12, 6), (18, 8)] {
        let mut ui = Ui::default();
        ui.input.insert_str(&"x\n".repeat(10));
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(width, height))
            .expect("tiny terminal");
        terminal
            .draw(|frame| draw(frame, &ui, &meta, 1, &vitals, None))
            .expect("tiny draw");
        let symbols = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            symbols.contains("Input"),
            "input slot disappeared: {symbols}"
        );
        assert!(
            symbols.contains("test"),
            "status slot disappeared: {symbols}"
        );
    }
}

/// iter-26:折行行数(input_height/commit_height 共用)。
#[test]
fn wrapped_rows_counts_folding() {
    assert_eq!(wrapped_rows("", 80), 1);
    assert_eq!(wrapped_rows("hi", 80), 1);
    assert_eq!(wrapped_rows(&"x".repeat(85), 80), 2);
    assert_eq!(wrapped_rows("a\nb\nc", 80), 3);
    assert_eq!(wrapped_rows("abc", 0), 3); // width=0 → 每字符一行,不 panic
}

/// iter-26:静态提交高度 ≥1,折行入账。
#[test]
fn commit_height_at_least_one_row() {
    assert_eq!(commit_height("", 80), 1);
    assert_eq!(commit_height("short", 80), 1);
    assert_eq!(commit_height(&"x".repeat(85), 80), 2);
    assert_eq!(commit_height("a\nb", 80), 2);
}

/// iter-24:粘贴净化 —— CRLF/CR 归一 LF,控制字符滤除,\t 保留。
#[test]
fn sanitize_paste_normalizes_and_strips() {
    assert_eq!(sanitize_paste("a\r\nb"), "a\nb");
    assert_eq!(sanitize_paste("a\rb"), "a\nb");
    assert_eq!(sanitize_paste("a\x1b[31mb"), "a[31mb"); // ESC 滤除,可见字符保留
    assert_eq!(sanitize_paste("a\tb\nc"), "a\tb\nc");
}

/// iter-24:动态输入高度 —— 空=min、折行、多行、封顶 max、width=0 不 panic。
/// iter-48 G5(修「光标卡首行」):首逻辑行折多视觉行且非行首 → Up 先跳行首,不召回;
/// 行首 / 短行 → 照常召回历史。
#[test]
fn up_fallback_home_only_when_wrapped_and_not_at_start() {
    // 短行(单视觉行):任意位置 Up 都召回。
    assert!(!up_fallback_is_home("hi", 1, 80));
    assert!(!up_fallback_is_home("hi", 0, 80));
    // 长行折行:非行首 → 先跳行首;行首 → 召回。
    let long = "x".repeat(200);
    assert!(up_fallback_is_home(&long, 100, 80));
    assert!(!up_fallback_is_home(&long, 0, 80));
    // 多逻辑行不达此路径(move_up 会成功),但首行折行判定仍只看首行。
    let multi = format!("{}\nshort", "y".repeat(200));
    assert!(up_fallback_is_home(&multi, 5, 80));
    // 宽度 0 防御:max(1) 不崩。
    assert!(up_fallback_is_home(&long, 5, 0));
}

#[test]
fn input_height_grows_and_clamps() {
    assert_eq!(input_height("", 80, 3, 8), 3);
    assert_eq!(input_height("hi", 80, 3, 8), 3);
    assert_eq!(input_height(&"x".repeat(85), 80, 3, 8), 4);
    assert_eq!(input_height("a\nb\nc", 80, 3, 8), 5);
    assert_eq!(input_height(&"a\n".repeat(30), 80, 3, 8), 8);
    assert_eq!(input_height("abc", 0, 3, 8), 5);
}

/// iter-26:流式尾巴 —— 少于 K 全量,多于 K 取尾。
#[test]
fn stream_tail_takes_last_k_lines() {
    assert_eq!(stream_tail("a\nb\nc", 5), vec!["a", "b", "c"]);
    assert_eq!(stream_tail("a\nb\nc\nd\ne\nf", 3), vec!["d", "e", "f"]);
    assert!(stream_tail("", 3).is_empty());
}

/// iter-26:静态提交队列 —— note 入队有序,drain 取尽且清空(有界性 = 提交即出队)。
#[test]
fn commit_queue_drains_in_order() {
    let mut ui = Ui::default();
    ui.note("one", Color::White);
    ui.note("two", Color::Green);
    let drained = ui.drain_commits();
    assert_eq!(
        drained.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>(),
        vec!["one", "two"]
    );
    assert!(ui.commits.is_empty());
}

/// iter-28:色角色映射 —— ANSI 16 具名色,语义正确。
#[test]
fn role_colors_are_ansi16() {
    assert_eq!(role_color(Role::Success), Color::Green);
    assert_eq!(role_color(Role::Error), Color::Red);
    assert_eq!(role_color(Role::DiffAdd), Color::Green);
    assert_eq!(role_color(Role::DiffDel), Color::Red);
    assert_eq!(role_color(Role::Primary), Color::Cyan);
    assert_eq!(role_color(Role::Border), Color::DarkGray);
    assert_eq!(role_color(Role::Command), Color::LightGreen);
}

/// iter-28:md 轻渲染 —— 围栏切态、块内 Muted、标题粗、行内 code、未闭合按字面。
#[test]
fn md_line_rendering() {
    let (spans, state) = md_line_spans("```rust", false);
    assert!(state);
    assert_eq!(spans.len(), 1);
    let (_, state2) = md_line_spans("```", true);
    assert!(!state2);
    let (s, st) = md_line_spans("let x = 1;", true);
    assert!(st);
    assert_eq!(s[0].style.fg, Some(role_color(Role::Muted)));
    let (h, _) = md_line_spans("# Title", false);
    assert!(h[0].style.add_modifier.contains(Modifier::BOLD));
    let (i, _) = md_line_spans("use `foo` now", false);
    assert_eq!(i[1].content.as_ref(), "foo");
    assert_eq!(i[1].style.fg, Some(role_color(Role::Warn)));
    let (b, _) = md_line_spans("a **big** b", false);
    assert!(b
        .iter()
        .any(|sp| sp.content.as_ref() == "big" && sp.style.add_modifier.contains(Modifier::BOLD)));
    // 未闭合记号按字面,内容零丢失
    let (u, _) = md_line_spans("lone `tick", false);
    assert_eq!(
        u.iter().map(|sp| sp.content.as_ref()).collect::<String>(),
        "lone `tick"
    );
}

#[test]
fn markdown_structure_preserves_quote_and_nested_list_hierarchy() {
    let (quote, _) = md_line_spans("  > > cited **fact**", false);
    assert_eq!(
        quote
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>(),
        "  │ │ cited fact"
    );
    assert_eq!(quote[0].style.fg, Some(role_color(Role::Info)));
    assert!(quote.iter().any(|span| {
        span.content.as_ref() == "fact" && span.style.add_modifier.contains(Modifier::BOLD)
    }));

    let (list, _) = md_line_spans("    12. **nested** item", false);
    assert_eq!(list[0].content.as_ref(), "    12. ");
    assert_eq!(list[0].style.fg, Some(role_color(Role::Info)));
    assert!(list.iter().any(|span| {
        span.content.as_ref() == "nested" && span.style.add_modifier.contains(Modifier::BOLD)
    }));

    let (plain, _) = md_line_spans("a > b", false);
    assert_eq!(
        plain
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>(),
        "a > b"
    );
}

#[test]
fn live_markdown_structure_stays_within_narrow_cell_bound() {
    let mut in_code = false;
    let spans = live_markdown_line("  > > 你好", 8, &mut in_code, Color::White, Modifier::BOLD);
    let visible = spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(str_cells(&visible) <= 8);
    assert_eq!(spans[0].style.fg, Some(role_color(Role::Info)));
}

/// iter-52:回答块走显式 Markdown 提交路径，徽标/代码围栏跨行保留语义色。
#[test]
fn markdown_answer_block_preserves_semantic_spans() {
    let lines = markdown_lines("🤖 # Title\n```rust\nlet x = 1;\n```\nplain");
    assert_eq!(lines[0].spans[0].content.as_ref(), "🤖 ");
    assert_eq!(lines[0].spans[0].style.fg, Some(role_color(Role::Primary)));
    assert!(lines[0].spans[1]
        .style
        .add_modifier
        .contains(Modifier::BOLD));
    assert_eq!(lines[2].spans[0].style.fg, Some(role_color(Role::Muted)));
    assert_eq!(lines[3].spans[0].style.fg, Some(role_color(Role::Border)));
    let visible = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(!visible.contains('\x1b'));
}

#[test]
fn live_answer_uses_bounded_markdown_roles_and_fence_state() {
    let mut in_code = false;
    let spans = live_markdown_line(
        "a `code` **bold**",
        64,
        &mut in_code,
        Color::White,
        Modifier::BOLD,
    );
    assert_eq!(
        spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>(),
        "a code bold"
    );
    assert_eq!(
        spans
            .iter()
            .find(|span| span.content.as_ref() == "code")
            .and_then(|span| span.style.fg),
        Some(role_color(Role::Warn))
    );
    assert!(spans.iter().any(|span| {
        span.content.as_ref() == "bold" && span.style.add_modifier.contains(Modifier::BOLD)
    }));

    let fence = live_markdown_line("```rust", 64, &mut in_code, Color::White, Modifier::BOLD);
    assert_eq!(fence[0].style.fg, Some(role_color(Role::Border)));
    assert!(in_code);
    let body = live_markdown_line("let x = 1;", 64, &mut in_code, Color::White, Modifier::BOLD);
    assert_eq!(body[0].content.as_ref(), "let");
    assert_eq!(body[0].style.fg, Some(role_color(Role::Primary)));
    assert!(body.iter().any(|span| {
        span.content.as_ref() == "1" && span.style.fg == Some(role_color(Role::Warn))
    }));
    assert!(in_code);
    let close = live_markdown_line("```", 64, &mut in_code, Color::White, Modifier::BOLD);
    assert_eq!(close[0].style.fg, Some(role_color(Role::Border)));
    assert!(!in_code);
}

#[test]
fn live_code_tokens_are_semantic_and_visible_only() {
    let mut in_code = true;
    let spans = live_markdown_line(
        "fn main() { let count: usize = 42; println!(\"ok\"); } // note",
        128,
        &mut in_code,
        Color::White,
        Modifier::empty(),
    );
    assert!(spans.iter().any(|span| {
        span.content.as_ref() == "fn" && span.style.fg == Some(role_color(Role::Primary))
    }));
    assert!(spans.iter().any(|span| {
        span.content.as_ref() == "usize" && span.style.fg == Some(role_color(Role::Info))
    }));
    assert!(spans.iter().any(|span| {
        span.content.as_ref() == "42" && span.style.fg == Some(role_color(Role::Warn))
    }));
    assert!(spans.iter().any(|span| {
        span.content.as_ref() == "\"ok\"" && span.style.fg == Some(role_color(Role::Success))
    }));

    let clipped = live_markdown_line(
        "let visible = 1; // secret_keyword",
        18,
        &mut in_code,
        Color::White,
        Modifier::empty(),
    );
    let visible = clipped
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(visible.contains("visible"));
    assert!(!visible.contains("secret_keyword"));
}

#[test]
fn clipped_live_answer_uses_actual_fence_context() {
    let mut transcript = LiveTranscript::default();
    transcript.push_answer("```rust\nlet hidden = true;\nlet visible = true;");
    let line = transcript
        .visible_lines(1)
        .into_iter()
        .next()
        .expect("visible answer");
    assert!(line.fence_before);

    let mut in_code = line.fence_before;
    let spans = live_markdown_line(line.text, 64, &mut in_code, Color::White, Modifier::BOLD);
    assert_eq!(spans[0].content.as_ref(), "let");
    assert_eq!(spans[0].style.fg, Some(role_color(Role::Primary)));
    assert!(in_code);
}

#[test]
fn fence_language_badge_is_bounded_and_display_only() {
    assert_eq!(fence_language("```rust"), Some("rust"));
    assert_eq!(fence_language("  ```python extra"), Some("python"));
    assert_eq!(fence_language("```bad/lang"), None);
    assert_eq!(fence_language("```"), None);
    assert_eq!(fence_without_language("  ```rust"), "  ```");
}

/// iter-52:回答块由事件类型标记 Markdown，不再靠渲染层猜测文本前缀。
#[test]
fn markdown_commit_is_typed() {
    let mut ui = Ui::default();
    ui.note_markdown("🤖 **answer**");
    let blocks = ui.drain_commit_blocks();
    assert!(matches!(blocks.as_slice(), [CommitBlock::Markdown { .. }]));
}

#[test]
fn prefixed_final_event_uses_markdown_answer_path() {
    let event = "reason#2: (final) **answer**";
    let mut ui = Ui::default();
    for (line, color) in summarize_event(event) {
        if is_final_event(event) {
            ui.note_markdown(line);
        } else {
            ui.note(line, color);
        }
    }
    let blocks = ui.drain_commit_blocks();
    assert!(matches!(
        blocks.as_slice(),
        [CommitBlock::Markdown { text }] if text == "🤖 **answer**"
    ));
}

/// iter-52:TestBackend 复现窄终端渲染，证明宽字符折行不注入 ANSI 残留且不 panic。
#[test]
fn markdown_render_survives_narrow_test_backend() {
    let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(12, 8)).expect("terminal");
    let lines = markdown_lines("🤖 # 标题\n```rust\n你你\n```\nplain");
    terminal
        .draw(|frame| {
            Paragraph::new(Text::from(lines.clone()))
                .wrap(Wrap { trim: false })
                .render(frame.area(), frame.buffer_mut());
        })
        .expect("render");
    let symbols = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(!symbols.contains('\x1b'));
}

#[test]
fn full_tui_frame_survives_narrow_cjk_and_escape_text() {
    let mut ui = Ui::default();
    ui.input.insert_str("你好 🚀");
    ui.busy = true;
    ui.phase = "reasoning".into();
    ui.push_chunk(provider::StreamChunk::Reasoning("思考\x1b[2K".into()));
    ui.push_tool(
        ToolBlock::from_lines(vec![
            ("  tool: search".into(), Color::Cyan),
            ("  detail 你好".into(), Color::Gray),
        ])
        .expect("tool"),
    );
    ui.push_chunk(provider::StreamChunk::Answer("回答\x1b]8;;url\x07".into()));
    let meta = ReplMeta {
        tools: Vec::new(),
        provider: "test".into(),
        model: "model".into(),
        base_url: String::new(),
        status_bar: "\x1b[31m{provider}\x1b[0m · {model}".into(),
        ctx_window: 200_000,
    };
    let vitals = Vitals {
        step: 3,
        elapsed_s: 2,
        task_tokens: 8,
        rate: 4,
        ctx_used: 16,
        queued: 0,
    };
    for (width, height) in [(18, 8), (12, 6), (8, 4)] {
        let mut terminal =
            Terminal::new(ratatui::backend::TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| draw(frame, &ui, &meta, 8, &vitals, None))
            .expect("draw");
        let symbols = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(!symbols.contains('\x1b'));
        if width >= 12 {
            assert!(
                symbols.contains("[ANSWER]"),
                "wide compact badge: {symbols}"
            );
        } else {
            assert!(symbols.contains(" A "), "narrow compact badge: {symbols}");
        }
    }

    let mut reasoning_ui = Ui {
        busy: true,
        ..Ui::default()
    };
    reasoning_ui.push_chunk(provider::StreamChunk::Reasoning("actual reasoning".into()));
    let mut terminal =
        Terminal::new(ratatui::backend::TestBackend::new(48, 8)).expect("metadata terminal");
    terminal
        .draw(|frame| draw(frame, &reasoning_ui, &meta, 8, &vitals, None))
        .expect("metadata draw");
    let symbols = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        symbols.contains("step 3") && symbols.contains("t+2s"),
        "{symbols}"
    );
    assert!(symbols.contains("8 task tok"), "{symbols}");
    assert!(symbols.contains("[THINK]"), "{symbols}");
    let active_rail = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .find(|cell| cell.symbol() == "┌")
        .expect("active reasoning rail");
    assert_eq!(active_rail.fg, role_color(Role::Primary));

    reasoning_ui.push_chunk(provider::StreamChunk::Answer("actual answer".into()));
    terminal
        .draw(|frame| draw(frame, &reasoning_ui, &meta, 8, &vitals, None))
        .expect("answer channel draw");
    let answer_symbols = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(answer_symbols.contains("[ANSWER]"), "{answer_symbols}");

    reasoning_ui.push_tool(
        ToolBlock::from_lines(vec![("actual tool".into(), Color::Cyan)]).expect("channel tool"),
    );
    terminal
        .draw(|frame| draw(frame, &reasoning_ui, &meta, 8, &vitals, None))
        .expect("tool channel draw");
    let tool_symbols = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(tool_symbols.contains("[TOOL]"), "{tool_symbols}");

    reasoning_ui.busy = false;
    terminal
        .draw(|frame| draw(frame, &reasoning_ui, &meta, 8, &vitals, None))
        .expect("idle metadata draw");
    let idle_rail = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .find(|cell| cell.symbol() == "┌")
        .expect("idle reasoning rail");
    assert_eq!(idle_rail.fg, role_color(Role::Muted));

    let mut hint_before =
        Terminal::new(ratatui::backend::TestBackend::new(80, 8)).expect("reasoning hint terminal");
    hint_before
        .draw(|frame| draw(frame, &reasoning_ui, &meta, 8, &vitals, None))
        .expect("reasoning hint draw");
    let before_symbols = hint_before
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        before_symbols.contains("Ctrl+R reasoning"),
        "{before_symbols}"
    );

    assert!(reasoning_ui.transcript.toggle_reasoning());
    let mut hint_after = Terminal::new(ratatui::backend::TestBackend::new(80, 8))
        .expect("expanded reasoning hint terminal");
    hint_after
        .draw(|frame| draw(frame, &reasoning_ui, &meta, 8, &vitals, None))
        .expect("expanded reasoning hint draw");
    let after_symbols = hint_after
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(after_symbols.contains("Ctrl+R collapse"), "{after_symbols}");
    assert!(
        !after_symbols.contains("Ctrl+R reasoning"),
        "{after_symbols}"
    );

    let mut narrow_ui = Ui {
        busy: true,
        ..Ui::default()
    };
    narrow_ui.push_chunk(provider::StreamChunk::Reasoning("actual reasoning".into()));
    let narrow_vitals = Vitals {
        step: 123,
        elapsed_s: 987,
        task_tokens: 123_456,
        rate: 321,
        ctx_used: 0,
        queued: 0,
    };
    let mut narrow_terminal =
        Terminal::new(ratatui::backend::TestBackend::new(12, 8)).expect("narrow terminal");
    narrow_terminal
        .draw(|frame| draw(frame, &narrow_ui, &meta, 8, &narrow_vitals, None))
        .expect("narrow reasoning draw");
    let narrow_row = narrow_terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .take(12)
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        narrow_row.contains("actual"),
        "reasoning row lost model text: {narrow_row}"
    );

    let mut code_ui = Ui::default();
    code_ui.push_chunk(provider::StreamChunk::Answer(
        "intro\n```rust\nfn main() {}\n```\nend".into(),
    ));
    let mut code_terminal =
        Terminal::new(ratatui::backend::TestBackend::new(48, 12)).expect("code terminal");
    code_terminal
        .draw(|frame| draw(frame, &code_ui, &meta, 8, &vitals, None))
        .expect("code draw");
    let code_cells = code_terminal.backend().buffer().content();
    let fence_rail = code_cells
        .iter()
        .find(|cell| cell.symbol() == "\u{251c}")
        .expect("fence rail");
    assert_eq!(fence_rail.fg, role_color(Role::Border));
    let code_symbols = code_cells
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(code_symbols.contains("rust"), "{code_symbols}");
    let body_rail = code_cells
        .iter()
        .find(|cell| cell.symbol() == "\u{250a}")
        .expect("body rail");
    assert_eq!(body_rail.fg, role_color(Role::Muted));

    let mut clipped_code_ui = Ui::default();
    clipped_code_ui.push_chunk(provider::StreamChunk::Answer(format!(
        "intro\n```rust\n{}",
        (0..16)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n")
    )));
    let mut clipped_code_terminal =
        Terminal::new(ratatui::backend::TestBackend::new(48, 8)).expect("clipped code terminal");
    clipped_code_terminal
        .draw(|frame| draw(frame, &clipped_code_ui, &meta, 8, &vitals, None))
        .expect("clipped code draw");
    let clipped_code_cells = clipped_code_terminal.backend().buffer().content();
    let clipped_body_rail = clipped_code_cells
        .iter()
        .find(|cell| cell.symbol() == "\u{250a}")
        .expect("clipped code keeps body rail from hidden opener");
    assert_eq!(clipped_body_rail.fg, role_color(Role::Muted));
    assert!(!clipped_code_cells
        .iter()
        .any(|cell| cell.symbol() == "\u{251c}"));

    let mut chain_ui = Ui {
        busy: true,
        ..Ui::default()
    };
    chain_ui.push_chunk(provider::StreamChunk::Reasoning("thinking".into()));
    chain_ui.push_tool(
        ToolBlock::from_lines(vec![
            ("tool summary".into(), Color::Cyan),
            ("tool detail".into(), Color::Gray),
        ])
        .expect("connector tool"),
    );
    chain_ui.push_chunk(provider::StreamChunk::Answer("final answer".into()));
    assert!(chain_ui.transcript.toggle_reasoning());
    let mut chain_terminal =
        Terminal::new(ratatui::backend::TestBackend::new(48, 12)).expect("connector terminal");
    chain_terminal
        .draw(|frame| draw(frame, &chain_ui, &meta, 8, &vitals, None))
        .expect("connector draw");
    let chain_cells = chain_terminal.backend().buffer().content();
    let connector_rail = chain_cells
        .iter()
        .find(|cell| cell.symbol() == "├")
        .expect("reasoning-tool connector rail");
    assert_eq!(connector_rail.fg, role_color(Role::Primary));
    let answer_rail = chain_cells
        .iter()
        .find(|cell| cell.symbol() == "╰")
        .expect("tool-answer connector rail");
    assert_eq!(answer_rail.fg, role_color(Role::Primary));

    let mut failure_ui = Ui::default();
    failure_ui.push_tool(
        ToolBlock::from_lines(summarize_event("act: run_shell -> exit 1: boom"))
            .expect("failure tool"),
    );
    let mut failure_terminal =
        Terminal::new(ratatui::backend::TestBackend::new(48, 8)).expect("failure terminal");
    failure_terminal
        .draw(|frame| draw(frame, &failure_ui, &meta, 8, &vitals, None))
        .expect("failure draw");
    let failure_rail = failure_terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .find(|cell| cell.symbol() == "▌")
        .expect("failure tool rail");
    assert_eq!(failure_rail.fg, role_color(Role::Error));

    let mut focused_ui = Ui::default();
    focused_ui.push_tool(
        ToolBlock::from_lines(vec![
            ("focused tool".into(), Color::Cyan),
            ("focused detail".into(), Color::Gray),
        ])
        .expect("focused tool"),
    );
    assert!(focused_ui.transcript.toggle_details());
    let mut focused_terminal =
        Terminal::new(ratatui::backend::TestBackend::new(48, 8)).expect("focused terminal");
    focused_terminal
        .draw(|frame| draw(frame, &focused_ui, &meta, 8, &vitals, None))
        .expect("focused draw");
    let focused_detail_rail = focused_terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .find(|cell| cell.symbol() == "┆")
        .expect("focused detail rail");
    assert_eq!(focused_detail_rail.fg, role_color(Role::Primary));

    ui.panel = Some(Panel::new(
        PanelKind::Tools,
        "Tools · type to filter · Esc close".into(),
        vec![PanelRow {
            key: "搜索工具".into(),
            value: "CJK value".into(),
            ctx: None,
        }],
    ));
    for (width, height) in [(18, 8), (12, 6), (8, 4)] {
        let mut terminal =
            Terminal::new(ratatui::backend::TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| draw(frame, &ui, &meta, 8, &vitals, None))
            .expect("panel draw");
    }

    let mut clipped_ui = Ui::default();
    clipped_ui.push_chunk(provider::StreamChunk::Answer(
        "a very long live answer that must stay within the viewport".into(),
    ));
    let mut terminal =
        Terminal::new(ratatui::backend::TestBackend::new(18, 8)).expect("clipped terminal");
    terminal
        .draw(|frame| draw(frame, &clipped_ui, &meta, 8, &vitals, None))
        .expect("clipped draw");
    let symbols = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        symbols.contains('…'),
        "live line should show a width marker: {symbols}"
    );
}

#[test]
fn responsive_panel_chrome_keeps_actions_visible_in_narrow_frames() {
    let mut tools = Panel::new(
        PanelKind::Tools,
        "Tools · type to filter · Esc close".into(),
        vec![PanelRow {
            key: "search tool".into(),
            value: "read_file".into(),
            ctx: None,
        }],
    );
    tools.query = "tool".into();
    tools.retype();

    let mut history = Panel::new(
        PanelKind::ToolHistory,
        "Tool history · Enter expand · Esc close".into(),
        vec![PanelRow {
            key: "#1 search tool".into(),
            value: "detail line".into(),
            ctx: None,
        }],
    );
    history.query = "tool".into();
    history.retype();
    history.detail_open = true;

    for (name, panel) in [("tools", tools), ("history", history)] {
        for (width, height) in [(18, 8), (12, 6), (8, 4)] {
            let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(width, height))
                .expect("panel terminal");
            terminal
                .draw(|frame| {
                    let area = frame.area();
                    draw_panel(frame, area, &panel)
                })
                .expect("responsive panel draw");
            let symbols = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(
                symbols.contains("Esc"),
                "{name} {width}x{height}: {symbols}"
            );
            if width >= 18 {
                assert!(
                    symbols.contains("Enter"),
                    "wide compact actions disappeared: {name} {symbols}"
                );
            }
            if width >= 12 && height >= 6 {
                assert!(
                    symbols.contains('>') || symbols.contains('🔍'),
                    "query chrome disappeared: {name} {symbols}"
                );
            }
            assert!(!symbols.contains('\x1b'));
        }
    }
}

#[test]
fn live_frame_pressure_stays_bounded_and_stable() {
    let mut ui = Ui {
        busy: true,
        phase: "reasoning".into(),
        ..Ui::default()
    };
    for index in 0..20 {
        ui.push_chunk(provider::StreamChunk::Reasoning(format!(
            "thinking {index}: inspect bounded transcript"
        )));
        ui.push_tool(
            ToolBlock::from_lines(vec![
                (format!("tool {index}: search"), Color::Cyan),
                (format!("detail {index}: 你好 🚀"), Color::Gray),
            ])
            .expect("pressure tool"),
        );
        ui.push_chunk(provider::StreamChunk::Answer(format!(
            "answer {index}: preserve the visible result"
        )));
    }
    assert!(ui.toggle_reasoning());
    assert!(ui.move_tool_focus(-1));
    assert!(ui.toggle_details());

    let meta = ReplMeta {
        tools: Vec::new(),
        provider: "test".into(),
        model: "model".into(),
        base_url: String::new(),
        status_bar: "{provider} · {model}".into(),
        ctx_window: 200_000,
    };
    let vitals = Vitals {
        step: 20,
        elapsed_s: 9,
        task_tokens: 200,
        rate: 22,
        ctx_used: 128,
        queued: 2,
    };

    for (width, height, frames) in [(96, 14, 32), (32, 12, 32), (12, 8, 32), (8, 4, 16)] {
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(width, height))
            .expect("pressure terminal");
        for frame_no in 0..frames {
            ui.frame = frame_no;
            terminal
                .draw(|frame| draw(frame, &ui, &meta, 200, &vitals, None))
                .expect("pressure draw");
            let cells = terminal.backend().buffer().content();
            assert_eq!(cells.len(), width as usize * height as usize);
            assert!(cells.iter().all(|cell| !cell.symbol().contains('\x1b')));
        }
        let symbols = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        if width >= 12 {
            assert!(
                symbols.contains("[ANSWER]"),
                "pressure answer badge: {symbols}"
            );
        } else {
            assert!(
                symbols.contains(" A "),
                "pressure compact answer badge: {symbols}"
            );
        }
    }
}

#[test]
fn busy_live_cursor_keeps_one_cell_at_width_edge() {
    let mut ui = Ui {
        busy: true,
        ..Ui::default()
    };
    ui.push_chunk(provider::StreamChunk::Answer(
        "012345678901234567890".into(),
    ));
    let meta = ReplMeta {
        tools: Vec::new(),
        provider: "test".into(),
        model: "model".into(),
        base_url: String::new(),
        status_bar: "{provider}".into(),
        ctx_window: 200_000,
    };
    let vitals = Vitals {
        step: 0,
        elapsed_s: 0,
        task_tokens: 0,
        rate: 0,
        ctx_used: 0,
        queued: 0,
    };
    let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(18, 8)).expect("terminal");
    terminal
        .draw(|frame| draw(frame, &ui, &meta, 0, &vitals, None))
        .expect("draw");
    assert!(
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .any(|cell| cell.symbol() == "█"),
        "busy cursor must remain visible in a full-width live row"
    );
}

#[test]
fn long_reasoning_clamp_preserves_answer_and_input_slots() {
    let mut ui = Ui {
        busy: true,
        ..Ui::default()
    };
    let reasoning = (0..100)
        .map(|index| format!("r{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let answer = (0..100)
        .map(|index| format!("a{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    ui.push_chunk(provider::StreamChunk::Reasoning(reasoning));
    ui.push_chunk(provider::StreamChunk::Answer(answer));
    let meta = ReplMeta {
        tools: Vec::new(),
        provider: "test".into(),
        model: "model".into(),
        base_url: String::new(),
        status_bar: "{provider} · {model}".into(),
        ctx_window: 200_000,
    };
    let vitals = Vitals {
        step: 4,
        elapsed_s: 7,
        task_tokens: 120,
        rate: 17,
        ctx_used: 0,
        queued: 0,
    };
    let mut terminal =
        Terminal::new(ratatui::backend::TestBackend::new(80, 10)).expect("clamp terminal");
    terminal
        .draw(|frame| draw(frame, &ui, &meta, 120, &vitals, None))
        .expect("clamp draw");
    let symbols = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        symbols.contains("r99"),
        "last reasoning row should remain: {symbols}"
    );
    assert!(
        symbols.contains("a98") && symbols.contains("a99"),
        "answer tail: {symbols}"
    );
    assert!(
        symbols.contains('┌') && symbols.contains('╰'),
        "semantic rails: {symbols}"
    );
    assert!(
        symbols.contains('┊'),
        "reasoning truncation rail: {symbols}"
    );
    assert!(
        symbols.contains("Input") || symbols.contains("Queue"),
        "input slot should remain: {symbols}"
    );
}

#[test]
fn markdown_commit_renders_through_inline_scrollback() {
    let mut terminal = Terminal::with_options(
        ratatui::backend::TestBackend::new(32, 8),
        TerminalOptions {
            viewport: Viewport::Inline(4),
        },
    )
    .expect("terminal");
    let mut ui = Ui::default();
    ui.note_markdown("🤖 # Answer\n**stable**");
    flush_commits(&mut terminal, &mut ui).expect("scrollback");
    assert!(ui.commits.is_empty());
    let symbols = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(symbols.contains("Answer") || symbols.contains("stable"));
    assert!(!symbols.contains('\x1b'));
}

#[test]
fn static_scrollback_preserves_order_and_sanitizes_controls() {
    let mut terminal = Terminal::with_options(
        ratatui::backend::TestBackend::new(48, 20),
        TerminalOptions {
            viewport: Viewport::Inline(8),
        },
    )
    .expect("terminal");
    let mut ui = Ui::default();
    ui.note("first \x1b[2J 你好", role_color(Role::Info));
    ui.note_markdown("second **🚀**");
    ui.push_tool(
        ToolBlock::from_lines(vec![(
            "third tool \x1b]8;;https://invalid\x07".into(),
            role_color(Role::Info),
        )])
        .expect("tool"),
    );
    ui.commit_live_tools();

    flush_commits(&mut terminal, &mut ui).expect("static scrollback");
    let symbols = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    let first = symbols.find("first").expect("first commit");
    let second = symbols.find("second").expect("second commit");
    let third = symbols.find("third tool").expect("tool commit");
    assert!(
        first < second && second < third,
        "commit order changed: {symbols}"
    );
    // TestBackend represents some wide glyphs as replacement cells; width is verified
    // independently while the buffer assertions above cover order and sanitization.
    assert_eq!(unicode_width::UnicodeWidthStr::width("你好 🚀"), 7);
    assert!(!symbols.contains('\x1b') && !symbols.contains("2J"));
}

#[test]
fn focused_live_tool_details_scroll_within_bounded_view() {
    let mut transcript = LiveTranscript::default();
    let mut lines = vec![("tool summary".to_owned(), Color::Cyan)];
    lines.extend((0..20).map(|index| (format!("detail {index:02}"), Color::Gray)));
    transcript.push_tool(ToolBlock::from_lines(lines).expect("long tool"));
    assert!(transcript.toggle_details());
    assert!(transcript.has_scrollable_tool_details());

    let latest = transcript.visible_lines(5);
    assert_eq!(latest.first().map(|line| line.text), Some("tool summary"));
    assert_eq!(latest.last().map(|line| line.text), Some("detail 19"));
    assert!(transcript.scroll_tool_details(1));
    let older = transcript.visible_lines(5);
    assert_eq!(older.first().map(|line| line.text), Some("tool summary"));
    assert_eq!(older.get(1).map(|line| line.text), Some("detail 12"));
    assert_eq!(older.last().map(|line| line.text), Some("detail 15"));

    assert!(transcript.scroll_tool_details(-1));
    assert_eq!(
        transcript.visible_lines(5).last().map(|line| line.text),
        Some("detail 19")
    );
    assert!(!transcript.toggle_details());
    assert!(!transcript.scroll_tool_details(1));
}

#[test]
fn tool_commit_keeps_summary_and_details_together() {
    let mut terminal = Terminal::with_options(
        ratatui::backend::TestBackend::new(32, 8),
        TerminalOptions {
            viewport: Viewport::Inline(5),
        },
    )
    .expect("terminal");
    let mut tool = ToolBlock::from_lines(vec![
        ("tool summary".into(), Color::Cyan),
        ("detail one".into(), Color::Gray),
        ("detail two".into(), Color::Gray),
    ])
    .expect("tool");
    tool.toggle();
    let mut ui = Ui::default();
    ui.commits.push(CommitBlock::Tool(tool));
    flush_commits(&mut terminal, &mut ui).expect("scrollback");
    let symbols = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(symbols.contains("tool summary"));
    assert!(symbols.contains("detail one"));
    assert!(symbols.contains("detail two"));
    assert!(symbols.contains("◈"));
    assert!(symbols.contains("┆"));
}

#[test]
fn tool_history_is_collapsed_and_expandable_after_static_commit() {
    let mut ui = Ui::default();
    ui.push_tool(
        ToolBlock::from_lines(vec![
            ("tool summary".into(), Color::Cyan),
            ("detail one".into(), Color::Gray),
        ])
        .expect("tool"),
    );
    ui.commit_live_tools();
    assert_eq!(ui.tool_history.len(), 1);
    assert_eq!(ui.commits.len(), 1);
    let mut scrollback = Terminal::with_options(
        ratatui::backend::TestBackend::new(40, 8),
        TerminalOptions {
            viewport: Viewport::Inline(4),
        },
    )
    .expect("scrollback terminal");
    flush_commits(&mut scrollback, &mut ui).expect("static tool commit");
    assert!(ui.commits.is_empty());
    let static_symbols = scrollback
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(static_symbols.contains("tool summary"));
    assert!(static_symbols.contains("◈"));
    assert!(!static_symbols.contains("detail one"));
    assert!(ui.toggle_details_or_history());

    let mut meta = ReplMeta {
        tools: Vec::new(),
        provider: "test".into(),
        model: "model".into(),
        base_url: String::new(),
        status_bar: "{provider} · {model}".into(),
        ctx_window: 200_000,
    };
    let vitals = Vitals {
        step: 0,
        elapsed_s: 0,
        task_tokens: 0,
        rate: 0,
        ctx_used: 0,
        queued: 0,
    };
    let swap = Arc::new(provider::SwapProvider::new(Arc::new(
        provider::ScriptedProvider::new(Vec::new()),
    )));
    panel_enter(&mut ui, &mut meta, &swap);
    assert!(ui.panel.as_ref().expect("history panel").detail_open);
    panel_enter(&mut ui, &mut meta, &swap);
    assert!(!ui.panel.as_ref().expect("history panel").detail_open);
    for (width, height) in [(18, 8), (12, 6), (8, 4)] {
        let mut narrow = Terminal::new(ratatui::backend::TestBackend::new(width, height))
            .expect("narrow history terminal");
        narrow
            .draw(|frame| draw(frame, &ui, &meta, 0, &vitals, None))
            .expect("narrow collapsed history draw");
    }
    let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(40, 12)).expect("terminal");
    terminal
        .draw(|frame| draw(frame, &ui, &meta, 0, &vitals, None))
        .expect("collapsed history draw");
    let collapsed = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(collapsed.contains("tool summary"));
    assert!(!collapsed.contains("detail one"));

    ui.panel.as_mut().expect("history panel").detail_open = true;
    for (width, height) in [(18, 8), (12, 6), (8, 4)] {
        let mut narrow = Terminal::new(ratatui::backend::TestBackend::new(width, height))
            .expect("narrow expanded history terminal");
        narrow
            .draw(|frame| draw(frame, &ui, &meta, 0, &vitals, None))
            .expect("narrow expanded history draw");
    }
    let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(40, 12)).expect("terminal");
    terminal
        .draw(|frame| draw(frame, &ui, &meta, 0, &vitals, None))
        .expect("expanded history draw");
    let expanded = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(expanded.contains("detail one"));
    assert!(expanded.contains("▾"));
}

#[test]
fn collapsing_live_tool_consumes_ctrl_o_before_history_fallback() {
    let mut ui = Ui::default();
    ui.push_tool(ToolBlock::from_lines(vec![("old tool".into(), Color::Cyan)]).expect("old tool"));
    ui.commit_live_tools();
    ui.push_tool(
        ToolBlock::from_lines(vec![
            ("live tool".into(), Color::Cyan),
            ("live detail".into(), Color::Gray),
        ])
        .expect("live tool"),
    );

    assert!(ui.toggle_details_or_history());
    assert!(ui.panel.is_none());
    assert!(ui.toggle_details_or_history());
    assert!(ui.panel.is_none());
    assert_eq!(ui.transcript.visible_lines(4)[0].text, "live tool");
}

#[test]
fn tool_history_is_bounded() {
    let mut ui = Ui::default();
    for index in 0..(MAX_TOOL_HISTORY + 4) {
        ui.push_tool(
            ToolBlock::from_lines(vec![(format!("tool {index}"), Color::Cyan)]).expect("tool"),
        );
    }
    ui.commit_live_tools();

    assert_eq!(ui.tool_history.len(), MAX_TOOL_HISTORY);
    assert!(ui
        .tool_history
        .back()
        .is_some_and(|tool| tool.summary() == "tool 67"));
}

#[test]
fn actual_reasoning_is_committed_separately_from_answer() {
    let mut ui = Ui::default();
    ui.push_chunk(provider::StreamChunk::Reasoning(
        "inspect actual state".into(),
    ));
    ui.push_chunk(provider::StreamChunk::Answer("final answer".into()));
    ui.commit_live_reasoning(3, 12);
    assert!(ui.commits.iter().any(|block| matches!(
        block,
        CommitBlock::Reasoning {
            text,
            step: 3,
            elapsed_s: 12,
            tokens: 8,
        } if text == "inspect actual state"
    )));
    assert!(ui
        .transcript
        .visible_lines(4)
        .iter()
        .any(|line| { line.kind == LiveLineKind::Answer && line.text == "final answer" }));
    ui.clear_streams();
    assert!(ui.transcript.visible_lines(4).is_empty());
    assert!(ui.drain_commits().iter().any(|(text, color)| text
        == "💭 [step 3 · t+12s · 8 task tok] inspect actual state"
        && *color == role_color(Role::Muted)));
}

#[test]
fn reasoning_commit_renders_in_inline_scrollback() {
    let mut ui = Ui::default();
    ui.push_chunk(provider::StreamChunk::Reasoning("actual plan".into()));
    ui.commit_live_reasoning(2, 12);
    let mut terminal = Terminal::with_options(
        ratatui::backend::TestBackend::new(40, 8),
        TerminalOptions {
            viewport: Viewport::Inline(4),
        },
    )
    .expect("reasoning terminal");
    flush_commits(&mut terminal, &mut ui).expect("reasoning scrollback");
    let symbols = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(symbols.contains("actual plan"));
    assert!(symbols.contains("t+12s"));
    assert!(symbols.contains("task tok"));
    assert!(symbols.contains("┊"));
    let reasoning_cell = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .find(|cell| cell.symbol() == "a")
        .expect("static reasoning cell");
    assert!(reasoning_cell.modifier.contains(Modifier::DIM));
    assert!(reasoning_cell.modifier.contains(Modifier::ITALIC));
    assert!(ui.commits.is_empty());
    assert!(!symbols.contains('\x1b'));
}

/// iter-28:呈现层折叠 —— 限内不动,超限留头 + `+N` 尾标。
#[test]
fn fold_lines_caps_output() {
    assert_eq!(fold_lines("a\nb", 20), "a\nb");
    let long: String = (0..30)
        .map(|i| format!("l{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let folded = fold_lines(&long, 20);
    assert!(folded.contains("l0") && folded.contains("l19"));
    assert!(!folded.contains("l29"));
    assert!(folded.contains("+10 lines folded"));
}

/// iter-28:启动帧序列 —— 首帧零字形、末帧全幅、宽度单调不减。
#[test]
fn splash_reveals_monotonically() {
    assert!(splash_frame(0, SPLASH_TICKS).chars().all(|c| c == '\n'));
    assert_eq!(splash_frame(SPLASH_TICKS, SPLASH_TICKS), SPLASH.join("\n"));
    let mut prev = 0;
    for t in 0..=SPLASH_TICKS {
        let glyphs = splash_frame(t, SPLASH_TICKS)
            .chars()
            .filter(|c| *c != '\n')
            .count();
        assert!(glyphs >= prev);
        prev = glyphs;
    }
}

/// iter-36:落定 banner 防「标识乱了」—— 宽则居中艺术字(每行 ≤ width 不折)+ tagline,窄则紧凑单行。
#[test]
fn splash_block_guards_width() {
    let wide = splash_block(80);
    assert!(wide.len() > SPLASH.len()); // 含 tagline
    for line in &wide {
        assert!(
            line.chars().count() <= 80,
            "banner 行不得超宽致折行: {line:?}"
        );
    }
    assert!(wide.iter().any(|l| l.contains('_'))); // ASCII 艺术字仍在
    let narrow = splash_block(10);
    assert_eq!(narrow.len(), 1); // 退化单行
    assert!(narrow[0].chars().count() <= 12); // 极窄也不折
    assert!(!has_cjk(&narrow[0]));
}

/// iter-36:所有交互页标题为英文(全局显示英化)。
#[test]
fn panel_titles_are_english() {
    let titles = [
        config_panel().title,
        provider_panel().title,
        tools_panel(&[]).title,
        models_panel(&[], "", "").title,
        agent_panel(&[]).title,
    ];
    for t in &titles {
        assert!(!has_cjk(t), "panel 标题应为英文: {t}");
    }
}

/// 判断串是否含 CJK(用户可见串英化的验收辅助)。
fn has_cjk(s: &str) -> bool {
    s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

/// iter-26:TODO 进度与清单渲染(状态行计数 + 变更快照历史化)。
#[test]
fn todo_progress_and_block_render() {
    assert_eq!(todo_progress(&[]), None);
    let todos = vec![
        Todo {
            content: "a".into(),
            status: "completed".into(),
        },
        Todo {
            content: "b".into(),
            status: "in_progress".into(),
        },
    ];
    assert_eq!(todo_progress(&todos), Some((1, 2)));
    assert_eq!(render_todo_block(&todos), "[✓] a\n[~] b");
}
