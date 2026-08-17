use ratatui::{backend::TestBackend, Terminal, TerminalOptions, Viewport};

use super::{
    flush_commits, role_color, str_cells, user_prompt_line, ActivityKind, Role, Ui, THEME_BLUE,
    THEME_BORDER, THEME_ICE, THEME_OLIVE,
};

fn frame_rows(terminal: &Terminal<TestBackend>, width: usize) -> Vec<String> {
    terminal
        .backend()
        .buffer()
        .content()
        .chunks(width)
        .map(|row| {
            let mut text = String::new();
            let mut skip = 0usize;
            for cell in row {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                let symbol = cell.symbol();
                text.push_str(symbol);
                skip = str_cells(symbol).saturating_sub(1);
            }
            text.trim_end().to_owned()
        })
        .filter(|row| !row.is_empty())
        .collect()
}

#[test]
fn theme_frame_marks_ask_folds_process_and_uses_roman_chrome() {
    let width = 72u16;
    let mut ui = Ui::default();
    let ask = user_prompt_line("把这段说明白");
    ui.note(ask.clone(), role_color(Role::Command));
    ui.record_activity(ActivityKind::Waiting, "waiting · no reasoning");
    ui.note_markdown("这 是 一 段 被 拉 稀 的 中 文");
    assert!(
        ui.commits.iter().any(|block| matches!(
            block,
            super::CommitBlock::Text { text, .. } if text == &ask
        )),
        "user prompt must be queued as a special transcript line"
    );
    let (title, body) =
        super::project_process_bundle(&[("waiting · no reasoning", ActivityKind::Waiting)]);
    assert_eq!(title, "§ ACTA · 1 steps");
    assert_eq!(body.len(), 1);
    let mut terminal = Terminal::with_options(
        TestBackend::new(width, 24),
        TerminalOptions {
            viewport: Viewport::Inline(16),
        },
    )
    .expect("theme frame terminal");
    flush_commits(&mut terminal, &mut ui).expect("theme frame flush");
    let rows = frame_rows(&terminal, width as usize);
    let text = rows.join("\n");
    assert!(ask.starts_with("¶ ASK · 把这段说明白"), "{ask}");
    assert!(!text.contains("§ ACTA ·"), "{text}");
    assert!(!text.contains("starting"), "{text}");
    assert!(text.contains("ANSWER"), "{text}");
    assert!(!text.contains("鈿?"), "{text}");
    assert!(!text.contains("鈴?"), "{text}");
    assert!(!text.contains("鉁?"), "{text}");
    assert!(!text.contains("鈥?"), "{text}");
    assert!(!text.contains("这 是 一 段"), "{text}");
    assert!(text.contains("这是一段被拉稀的中文"), "{text}");
    assert_eq!(role_color(Role::Primary), THEME_BLUE);
    assert_eq!(role_color(Role::Answer), THEME_ICE);
    assert_eq!(role_color(Role::Command), THEME_OLIVE);
    assert_eq!(role_color(Role::Border), THEME_BORDER);
    assert_eq!(THEME_BLUE, ratatui::style::Color::Rgb(184, 124, 48));
    assert_eq!(THEME_ICE, ratatui::style::Color::Rgb(244, 232, 204));
}

#[test]
fn theme_frame_text_dump_is_stable_enough_to_inspect() {
    let width = 64u16;
    let mut ui = Ui::default();
    ui.note(user_prompt_line("inspect frame"), role_color(Role::Command));
    ui.set_activity("waiting for model");
    ui.note_markdown("parchment body");
    let mut terminal = Terminal::with_options(
        TestBackend::new(width, 24),
        TerminalOptions {
            viewport: Viewport::Inline(16),
        },
    )
    .expect("dump terminal");
    flush_commits(&mut terminal, &mut ui).expect("dump flush");
    let text = frame_rows(&terminal, width as usize).join("\n");
    assert!(text.contains("¶ ASK · inspect frame"), "{text}");
    assert!(!text.contains("§ ACTA ·"), "{text}");
    assert!(text.contains("ANSWER"), "{text}");
    if let Ok(path) = std::env::var("RIDGE_THEME_FRAME_DUMP") {
        if !path.is_empty() {
            std::fs::write(&path, &text).expect("write theme frame dump");
        }
    }
}

#[test]
fn greeting_hides_lifecycle_chatter() {
    let width = 72u16;
    let mut ui = Ui::default();
    ui.note(user_prompt_line("hi"), role_color(Role::Command));
    ui.set_activity("starting task");
    ui.set_activity("next · verifying");
    ui.note(
        "verify: PASS (deterministic gate)",
        role_color(Role::Success),
    );
    ui.note_markdown("你好");
    ui.set_activity("settling result");
    ui.set_activity("completed");
    ui.note(
        "▣ approved · steps=1 · tokens=12",
        role_color(Role::Success),
    );
    assert!(ui
        .activity_history
        .iter()
        .any(|entry| entry.text == "starting task"));
    let mut terminal = Terminal::with_options(
        TestBackend::new(width, 24),
        TerminalOptions {
            viewport: Viewport::Inline(16),
        },
    )
    .expect("greeting terminal");
    flush_commits(&mut terminal, &mut ui).expect("greeting flush");
    let text = frame_rows(&terminal, width as usize).join("\n");
    assert!(text.contains("¶ ASK · hi"), "{text}");
    assert!(text.contains("你好"), "{text}");
    assert!(!text.contains("§ ACTA"), "{text}");
    assert!(!text.contains("starting task"), "{text}");
    assert!(!text.contains("verifying"), "{text}");
    assert!(!text.contains("verify: PASS"), "{text}");
    assert!(!text.contains("approved"), "{text}");
}
