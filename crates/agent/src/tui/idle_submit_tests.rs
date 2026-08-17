use super::{
    handle_key_event, process_pending_submit, CommitBlock, KeyEventContext, Panel, PanelKind,
    PanelRow, PendingSubmitContext, StartTask,
};
use crate::ReplMeta;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::sync::Arc;
use std::time::Instant;

fn test_meta() -> ReplMeta {
    ReplMeta {
        tools: Vec::new(),
        provider: "test".into(),
        provider_label: "test".into(),
        model: "model".into(),
        base_url: String::new(),
        status_bar: "{provider} · {model}".into(),
        ctx_window: 200_000,
    }
}

fn test_swap() -> Arc<provider::SwapProvider> {
    Arc::new(provider::SwapProvider::new(Arc::new(
        provider::ScriptedProvider::new(Vec::new()),
    )))
}

fn press_char(c: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
}

fn enter_release() -> Event {
    Event::Key(KeyEvent::new_with_kind(
        KeyCode::Enter,
        KeyModifiers::NONE,
        KeyEventKind::Release,
    ))
}

fn enter_press() -> Event {
    Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
}

fn commit_texts(ui: &super::Ui) -> Vec<&str> {
    ui.commits
        .iter()
        .filter_map(|block| match block {
            CommitBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

struct IdleHarness {
    ui: super::Ui,
    meta: ReplMeta,
    swap: Arc<provider::SwapProvider>,
    bus: agent::TokenBus,
    steer_bus: agent::SteerBus,
    pending: Option<super::ApprovalRequest>,
    task: Option<tokio::task::JoinHandle<()>>,
    task_started: Option<Instant>,
    last_task: Option<String>,
    retry_count: usize,
    pending_submit: Option<String>,
    momentary_hold: bool,
    last_ctrl_c: Option<Instant>,
    pressed: std::collections::HashSet<KeyCode>,
    keylog_path: Option<std::path::PathBuf>,
    history: Vec<provider::Message>,
    agents: agent::Agents,
    commands: Vec<agent::SlashCommand>,
    skills: Vec<agent::Skill>,
    printed: usize,
    last_activity: Option<Instant>,
    start_task: StartTask,
}

impl IdleHarness {
    fn new() -> Self {
        Self {
            ui: super::Ui::default(),
            meta: test_meta(),
            swap: test_swap(),
            bus: agent::null_token_bus(),
            steer_bus: agent::null_steer_bus(),
            pending: None,
            task: None,
            task_started: None,
            last_task: None,
            retry_count: 0,
            pending_submit: None,
            momentary_hold: false,
            last_ctrl_c: None,
            pressed: std::collections::HashSet::new(),
            keylog_path: None,
            history: Vec::new(),
            agents: agent::Agents::default(),
            commands: Vec::new(),
            skills: Vec::new(),
            printed: 0,
            last_activity: None,
            start_task: Box::new(|_, _| tokio::spawn(async {})),
        }
    }

    async fn send(&mut self, event: Event) {
        let last_task = self.last_task.clone();
        let mut context = KeyEventContext {
            ui: &mut self.ui,
            meta: &mut self.meta,
            swap: &self.swap,
            bus: &self.bus,
            steer_bus: &self.steer_bus,
            pending: &mut self.pending,
            task: &mut self.task,
            task_started: &mut self.task_started,
            last_task: &last_task,
            retry_count: &mut self.retry_count,
            pending_submit: &mut self.pending_submit,
            momentary_hold: &mut self.momentary_hold,
            last_ctrl_c: &mut self.last_ctrl_c,
            pressed: &mut self.pressed,
            keylog_path: &self.keylog_path,
            guard: None,
        };
        handle_key_event(event, &mut context)
            .await
            .expect("idle key");
    }

    async fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.send(press_char(c)).await;
        }
    }

    async fn consume_submit(&mut self) -> bool {
        process_pending_submit(&mut PendingSubmitContext {
            ui: &mut self.ui,
            history: &mut self.history,
            meta: &mut self.meta,
            swap: &self.swap,
            agents: &self.agents,
            commands: &self.commands,
            skills: &self.skills,
            session_tokens: 0,
            session_turns: 0,
            pending_submit: &mut self.pending_submit,
            retry_count: &mut self.retry_count,
            last_task: &mut self.last_task,
            task_started: &mut self.task_started,
            last_activity: &mut self.last_activity,
            printed: &mut self.printed,
            task: &mut self.task,
            start_task: &self.start_task,
        })
        .await
        .expect("pending submit")
    }
}

#[tokio::test]
async fn idle_enter_release_submits_prompt_through_key_handler() {
    let mut harness = IdleHarness::new();
    harness.type_text("ship this prompt").await;
    assert!(harness.pending_submit.is_none());
    harness.send(enter_release()).await;
    assert_eq!(harness.pending_submit.as_deref(), Some("ship this prompt"));
    assert!(harness.ui.input.buffer.is_empty());
    assert!(!harness.consume_submit().await);
    assert_eq!(harness.last_task.as_deref(), Some("ship this prompt"));
    assert_eq!(harness.history[0].content, "ship this prompt");
    assert!(harness.task.take().is_some());
}

#[tokio::test]
async fn idle_enter_press_submits_prompt_through_key_handler() {
    let mut harness = IdleHarness::new();
    harness.type_text("typed prompt").await;
    harness.send(enter_press()).await;
    assert_eq!(harness.pending_submit.as_deref(), Some("typed prompt"));
    assert!(!harness.consume_submit().await);
    assert_eq!(harness.last_task.as_deref(), Some("typed prompt"));
}

#[tokio::test]
async fn idle_enter_release_runs_help_command() {
    let mut harness = IdleHarness::new();
    harness.type_text("/help").await;
    harness.send(enter_release()).await;
    assert_eq!(harness.pending_submit.as_deref(), Some("/help"));
    assert!(harness.ui.input.buffer.is_empty());
    assert!(harness.ui.popup.is_none());
    assert!(!harness.consume_submit().await);
    let help = commit_texts(&harness.ui)
        .into_iter()
        .find(|text| text.contains("/login") && text.contains("/exit"))
        .expect("run_command(/help) must emit help text");
    assert!(help.contains("/model"), "{help}");
    assert!(harness.history.is_empty());
    assert!(harness.task.is_none());
}

#[tokio::test]
async fn idle_enter_press_runs_help_command() {
    let mut harness = IdleHarness::new();
    harness.type_text("/help").await;
    harness.send(enter_press()).await;
    assert_eq!(harness.pending_submit.as_deref(), Some("/help"));
    assert!(!harness.consume_submit().await);
    assert!(commit_texts(&harness.ui)
        .iter()
        .any(|text| text.contains("/login")));
}

#[test]
fn dangling_function_releases_become_press() {
    for code in [
        KeyCode::Esc,
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::Tab,
        KeyCode::Backspace,
    ] {
        let mut pressed = std::collections::HashSet::new();
        let ev = super::decide_key(
            &mut pressed,
            &KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Release),
        )
        .unwrap_or_else(|| panic!("dangling {code:?} Release"));
        assert_eq!(ev.code, code);
        assert_eq!(ev.kind, KeyEventKind::Press);
    }
}

#[test]
fn dangling_enter_release_becomes_press() {
    let mut pressed = std::collections::HashSet::new();
    let ev = super::decide_key(
        &mut pressed,
        &KeyEvent::new_with_kind(KeyCode::Enter, KeyModifiers::NONE, KeyEventKind::Release),
    )
    .expect("Windows ConPTY Enter release");
    assert_eq!(ev.code, KeyCode::Enter);
    assert_eq!(ev.kind, KeyEventKind::Press);
    assert_eq!(
        super::input_action(&ev, false, false),
        super::InputAction::Submit
    );
}

#[tokio::test]
async fn empty_idle_enter_release_does_not_submit() {
    let mut harness = IdleHarness::new();
    harness.send(enter_release()).await;
    assert!(harness.pending_submit.is_none());
    assert!(harness.ui.input.buffer.is_empty());
}

fn esc() -> Event {
    Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
}

#[tokio::test]
async fn idle_esc_bracket_a_is_history_nav_not_literal() {
    let mut harness = IdleHarness::new();
    harness.type_text("first-line").await;
    harness.send(enter_release()).await;
    assert!(!harness.consume_submit().await);
    harness.type_text("second-line").await;
    harness.send(esc()).await;
    harness.send(press_char('[')).await;
    harness.send(press_char('A')).await;
    assert!(
        !harness.ui.input.buffer.contains("[A"),
        "CSI leftover inserted: {}",
        harness.ui.input.buffer
    );
    assert_eq!(harness.ui.input.buffer, "first-line");
}

#[tokio::test]
async fn leftover_bracket_a_in_buffer_navigates() {
    let mut harness = IdleHarness::new();
    harness.type_text("hello[").await;
    harness.send(press_char('A')).await;
    assert!(
        !harness.ui.input.buffer.contains("[A"),
        "leftover CSI inserted: {}",
        harness.ui.input.buffer
    );
    assert_eq!(harness.ui.input.buffer, "hello");
}

#[tokio::test]
async fn leftover_csi_tails_navigate_through_key_handler() {
    for (typed, incoming, forbidden) in [
        ("keep[", 'A', "[A"),
        ("keep[", 'B', "[B"),
        ("keep[", 'C', "[C"),
        ("keep[", 'D', "[D"),
        ("keep[", 'H', "[H"),
        ("keep[", 'F', "[F"),
        ("keep[5", '~', "[5~"),
        ("keep[6", '~', "[6~"),
    ] {
        let mut harness = IdleHarness::new();
        harness.type_text(typed).await;
        harness.send(press_char(incoming)).await;
        assert!(
            !harness.ui.input.buffer.contains(forbidden),
            "typed {typed:?}+{incoming:?} left {forbidden:?} in {}",
            harness.ui.input.buffer
        );
        assert_eq!(harness.ui.input.buffer, "keep", "{typed:?}+{incoming:?}");
    }
}

#[tokio::test]
async fn idle_oa_stays_literal_letters() {
    let mut harness = IdleHarness::new();
    harness.type_text("OA").await;
    assert_eq!(harness.ui.input.buffer, "OA");
    harness.type_text(" GOAL BOARD CODE").await;
    assert_eq!(harness.ui.input.buffer, "OA GOAL BOARD CODE");
    assert!(!harness.ui.input.buffer.contains('['));
}

#[test]
fn shell_helpers_classify_bang_without_eating_plain_text() {
    assert!(super::is_shell_input("!echo hi"));
    assert!(!super::is_shell_input("echo hi"));
    assert_eq!(super::shell_command("!echo hi"), Some("echo hi"));
    assert_eq!(super::shell_command("!"), None);
    assert_eq!(super::shell_command("echo"), None);
    assert_eq!(
        super::shell_input_title(" Input (Enter send)".into(), "!ls"),
        " SHELL (Enter send)"
    );
}

#[tokio::test]
async fn bang_echo_runs_local_shell_without_starting_task() {
    let mut harness = IdleHarness::new();
    harness.type_text("!echo ridge-bang-ok").await;
    harness.send(enter_press()).await;
    assert_eq!(
        harness.pending_submit.as_deref(),
        Some("!echo ridge-bang-ok")
    );
    assert!(!harness.consume_submit().await);
    assert!(harness.task.is_none());
    assert!(harness.history.is_empty());
    let notes = commit_texts(&harness.ui);
    assert!(
        notes
            .iter()
            .any(|text| text.contains("> ! echo ridge-bang-ok")),
        "{notes:?}"
    );
    assert!(
        notes
            .iter()
            .any(|text| text.contains("ridge-bang-ok") && text.contains("exit 0")),
        "{notes:?}"
    );
}

fn sample_panel() -> Panel {
    Panel::new(
        PanelKind::Models,
        "Models".into(),
        vec![
            PanelRow {
                key: "one".into(),
                value: "first".into(),
                ctx: None,
            },
            PanelRow {
                key: "two".into(),
                value: "second".into(),
                ctx: None,
            },
            PanelRow {
                key: "three".into(),
                value: "third".into(),
                ctx: None,
            },
        ],
    )
}

#[tokio::test]
async fn panel_leftover_csi_arrows_select_instead_of_filtering() {
    let mut harness = IdleHarness::new();
    harness.ui.panel = Some(sample_panel());
    assert_eq!(harness.ui.panel.as_ref().unwrap().sel, 0);
    harness.send(press_char('[')).await;
    harness.send(press_char('B')).await;
    let panel = harness.ui.panel.as_ref().expect("panel stays open");
    assert!(
        !panel.query.contains("[B") && !panel.query.contains('['),
        "CSI leftover in filter: {}",
        panel.query
    );
    assert_eq!(panel.sel, 1);
    harness.send(press_char('[')).await;
    harness.send(press_char('B')).await;
    let panel = harness.ui.panel.as_ref().expect("panel stays open");
    assert!(
        panel.query.is_empty(),
        "second CSI leftover in filter: {}",
        panel.query
    );
    assert_eq!(panel.sel, 2, "second Down must advance again");
    harness.send(press_char('[')).await;
    harness.send(press_char('A')).await;
    let panel = harness.ui.panel.as_ref().expect("panel stays open");
    assert!(
        !panel.query.contains("[A"),
        "CSI leftover in filter: {}",
        panel.query
    );
    assert_eq!(panel.sel, 1);
}

#[tokio::test]
async fn panel_literal_bracket_filter_is_replayed_when_not_csi() {
    let mut harness = IdleHarness::new();
    harness.ui.panel = Some(sample_panel());
    harness.send(press_char('[')).await;
    harness.send(press_char('x')).await;
    let panel = harness.ui.panel.as_ref().expect("panel stays open");
    assert_eq!(panel.query, "[x");
}

#[tokio::test]
async fn popup_right_accepts_selected_completion() {
    let mut harness = IdleHarness::new();
    harness.type_text("/h").await;
    assert!(harness.ui.popup.is_some(), "slash popup should open");
    harness
        .send(Event::Key(KeyEvent::new(
            KeyCode::Right,
            KeyModifiers::NONE,
        )))
        .await;
    assert!(harness.ui.popup.is_none());
    assert!(
        harness.ui.input.buffer.starts_with("/h"),
        "right should complete: {}",
        harness.ui.input.buffer
    );
    assert_ne!(harness.ui.input.buffer, "/h");
}

#[tokio::test]
async fn popup_leftover_csi_down_moves_selection() {
    let mut harness = IdleHarness::new();
    harness.type_text("/").await;
    let start = harness
        .ui
        .popup
        .as_ref()
        .map(|popup| popup.selected)
        .expect("slash popup");
    harness.send(press_char('[')).await;
    harness.send(press_char('B')).await;
    assert!(
        !harness.ui.input.buffer.contains("[B"),
        "CSI leftover in input: {}",
        harness.ui.input.buffer
    );
    let popup = harness.ui.popup.as_ref().expect("popup stays");
    assert_ne!(popup.selected, start);
}

#[tokio::test]
async fn bang_without_command_shows_usage() {
    let mut harness = IdleHarness::new();
    harness.type_text("!").await;
    harness.send(enter_press()).await;
    assert!(!harness.consume_submit().await);
    assert!(harness.task.is_none());
    assert!(commit_texts(&harness.ui)
        .iter()
        .any(|text| text.contains("usage: !<command>")));
}

#[tokio::test]
async fn submitted_prompt_is_marked_ask_line() {
    let mut harness = IdleHarness::new();
    harness.type_text("可见提问").await;
    harness.send(enter_press()).await;
    assert!(!harness.consume_submit().await);
    assert!(
        commit_texts(&harness.ui)
            .iter()
            .any(|text| text.contains("¶ ASK · 可见提问")),
        "{:?}",
        commit_texts(&harness.ui)
    );
}
