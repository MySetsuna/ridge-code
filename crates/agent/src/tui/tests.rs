use super::*;
#[test]
fn event_colours_are_semantic() {
    assert_eq!(event_color("verify: PASS"), Color::Green);
    assert_eq!(event_color("act: run_shell"), Color::Yellow);
}
#[test]
fn final_answer_gets_assistant_marker() {
    assert_eq!(format_event_plain("(final) hello"), "🤖 hello");
}

/// iter-50:输出流总览化 —— 读只显路径、读回执丢内容、改显 ± diff、写显预览。
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
fn ctx_percent_clamps_and_guards() {
    assert_eq!(ctx_percent(0, 200_000), 0);
    assert_eq!(ctx_percent(6_000, 200_000), 3);
    assert_eq!(ctx_percent(999_999, 100), 100); // 超窗封顶
    assert_eq!(ctx_percent(500, 0), 0); // 窗口未知:防除零
}

#[test]
fn busy_bar_omits_todo_when_empty_and_shows_when_present() {
    let none = fmt_busy_bar("reasoning", &[], 12, 340, 28, 0);
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
    let with = fmt_busy_bar("acting", &todos, 3, 10, 3, 0);
    assert_eq!(with, "⚡ acting · ⏱ 3s · 10 tok · 3 tok/s · todo 1/2");
}

/// iter-33:忙碌粘条显待跑队列深度(纯函数)。
#[test]
fn busy_bar_shows_queue_depth() {
    assert_eq!(
        fmt_busy_bar("reasoning", &[], 5, 100, 20, 0),
        "⚡ reasoning · ⏱ 5s · 100 tok · 20 tok/s"
    );
    assert_eq!(
        fmt_busy_bar("reasoning", &[], 5, 100, 20, 2),
        "⚡ reasoning · ⏱ 5s · 100 tok · 20 tok/s · ⏳2"
    );
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

/// iter-23:重绘判定 —— 脏或 busy(spinner)才画,空闲零重绘。
#[test]
fn draw_only_when_dirty_or_busy() {
    assert!(should_draw(true, false));
    assert!(should_draw(false, true));
    assert!(!should_draw(false, false));
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
