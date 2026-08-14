use super::{
    handle_key_event, process_pending_submit, CommitBlock, KeyEventContext, PendingSubmitContext,
    StartTask,
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
