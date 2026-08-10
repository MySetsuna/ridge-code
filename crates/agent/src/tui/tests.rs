use super::{
    edit_input, handle_device_oauth_event, handle_done_result, handle_input_action,
    handle_key_event, handle_stream_event, handle_tick, handle_token_chunk, keylog_path,
    log_key_event, note_initial_ui, poll_device_oauth, poll_model_catalog, poll_oauth_callback,
    prepare_loop, process_pending_submit, reset_task_ui, run_event_loop, run_event_step,
    session_input_history, superstep_activity, tui_approver, DoneEventContext, EventStepContext,
    KeyEventContext, KeyEventResult, LoopPrepareContext, PendingSubmitContext, StartTask,
    StreamEventContext, TuiLoopContext,
};
use crate::{DeviceOAuthEvent, ReplMeta};
use ratatui::backend::CrosstermBackend;
use std::time::{Duration, Instant};

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

fn test_crossterm_terminal() -> Terminal<CrosstermBackend<std::io::Stdout>> {
    Terminal::with_options(
        CrosstermBackend::new(std::io::stdout()),
        TerminalOptions {
            viewport: Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 80, 24)),
        },
    )
    .expect("terminal")
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, modifiers))
}

fn release_key(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new_with_kind(
        code,
        modifiers,
        KeyEventKind::Release,
    ))
}

fn assert_continue(result: anyhow::Result<KeyEventResult>) {
    assert!(matches!(
        result.expect("key event should be handled"),
        KeyEventResult::Continue
    ));
}

#[tokio::test]
async fn extracted_key_handler_covers_priority_and_edit_paths() {
    let mut ui = Ui::default();
    ui.push_chunk(provider::StreamChunk::Reasoning("reasoning".into()));
    ui.push_chunk(provider::StreamChunk::Answer("answer".into()));
    let mut meta = test_meta();
    let swap = test_swap();
    let bus = agent::null_token_bus();
    let mut pending = None;
    let mut task = None;
    let mut task_started = None;
    let mut retry_count = 0;
    let mut pending_submit = None;
    let mut momentary_hold = false;
    let mut last_ctrl_c = None;
    let mut pressed = std::collections::HashSet::new();
    let keylog_path = None;
    macro_rules! dispatch {
        ($event:expr) => {{
            let mut context = KeyEventContext {
                ui: &mut ui,
                meta: &mut meta,
                swap: &swap,
                bus: &bus,
                pending: &mut pending,
                task: &mut task,
                task_started: &mut task_started,
                retry_count: &mut retry_count,
                pending_submit: &mut pending_submit,
                momentary_hold: &mut momentary_hold,
                last_ctrl_c: &mut last_ctrl_c,
                pressed: &mut pressed,
                keylog_path: &keylog_path,
            };
            handle_key_event($event, &mut context).await
        }};
    }
    macro_rules! action {
        ($action:expr) => {{
            let mut context = KeyEventContext {
                ui: &mut ui,
                meta: &mut meta,
                swap: &swap,
                bus: &bus,
                pending: &mut pending,
                task: &mut task,
                task_started: &mut task_started,
                retry_count: &mut retry_count,
                pending_submit: &mut pending_submit,
                momentary_hold: &mut momentary_hold,
                last_ctrl_c: &mut last_ctrl_c,
                pressed: &mut pressed,
                keylog_path: &keylog_path,
            };
            handle_input_action($action, &mut context);
        }};
    }

    assert_continue(dispatch!(Event::Paste("draft".into())));
    assert_continue(dispatch!(Event::Resize(80, 24)));
    assert_continue(dispatch!(key(KeyCode::Char('x'), KeyModifiers::NONE)));
    assert_continue(dispatch!(key(KeyCode::Backspace, KeyModifiers::NONE)));
    assert_continue(dispatch!(key(KeyCode::Left, KeyModifiers::NONE)));
    assert_continue(dispatch!(key(KeyCode::Right, KeyModifiers::NONE)));
    assert_continue(dispatch!(key(KeyCode::Home, KeyModifiers::NONE)));
    assert_continue(dispatch!(key(KeyCode::End, KeyModifiers::NONE)));
    assert_continue(dispatch!(key(KeyCode::Enter, KeyModifiers::NONE)));
    assert_eq!(pending_submit.as_deref(), Some("draft"));

    assert_continue(dispatch!(key(KeyCode::Char('q'), KeyModifiers::CONTROL)));
    assert!(ui.panel.is_some());
    assert_continue(dispatch!(key(KeyCode::Down, KeyModifiers::NONE)));
    assert_continue(dispatch!(key(KeyCode::Up, KeyModifiers::NONE)));
    assert_continue(dispatch!(key(KeyCode::PageDown, KeyModifiers::NONE)));
    assert_continue(dispatch!(key(KeyCode::PageUp, KeyModifiers::NONE)));
    assert_continue(dispatch!(key(KeyCode::Esc, KeyModifiers::NONE)));
    assert!(ui.panel.is_none());
    ui.device_auth_status = Some("pending device auth".into());
    ui.panel = Some(Panel::new(PanelKind::Queue, "Queue".into(), Vec::new()));
    assert_continue(dispatch!(key(KeyCode::Esc, KeyModifiers::NONE)));
    assert!(ui.device_auth_status.is_none());

    ui.panel = Some(Panel::new(
        PanelKind::Activity,
        "Activity".into(),
        Vec::new(),
    ));
    assert_continue(dispatch!(key(KeyCode::Char('o'), KeyModifiers::CONTROL)));
    assert_continue(dispatch!(key(KeyCode::Char('r'), KeyModifiers::CONTROL)));
    assert_continue(dispatch!(key(KeyCode::Char('a'), KeyModifiers::CONTROL)));
    assert_continue(dispatch!(key(KeyCode::Esc, KeyModifiers::NONE)));
    ui.panel = Some(Panel::new(
        PanelKind::Activity,
        "Activity".into(),
        Vec::new(),
    ));
    ui.panel.as_mut().expect("oauth panel").editing = Some("code".into());
    ui.panel.as_mut().expect("oauth panel").oauth_verifier = Some("state".into());
    assert_continue(dispatch!(key(KeyCode::Esc, KeyModifiers::NONE)));
    assert!(ui
        .panel
        .as_ref()
        .is_some_and(|panel| panel.editing.is_none()));
    assert_continue(dispatch!(key(KeyCode::Esc, KeyModifiers::NONE)));

    assert_continue(dispatch!(key(KeyCode::Char('i'), KeyModifiers::CONTROL)));
    assert!(ui.panel.is_some());
    assert_continue(dispatch!(key(KeyCode::Char('t'), KeyModifiers::CONTROL)));
    assert_continue(dispatch!(key(KeyCode::Esc, KeyModifiers::NONE)));
    assert!(ui.panel.is_none());

    ui.panel = Some(login_panel());
    ui.panel.as_mut().expect("login panel").sel = 2;
    ui.panel.as_mut().expect("login panel").editing = Some(String::new());
    assert_continue(dispatch!(key(KeyCode::Char('x'), KeyModifiers::NONE)));
    assert_continue(dispatch!(key(KeyCode::Backspace, KeyModifiers::NONE)));
    assert_continue(dispatch!(key(KeyCode::Enter, KeyModifiers::NONE)));
    assert_continue(dispatch!(key(KeyCode::Esc, KeyModifiers::NONE)));
    ui.panel = Some(Panel::new(
        PanelKind::Queue,
        "Queue".into(),
        vec![PanelRow {
            key: "queued".into(),
            value: "task".into(),
            ctx: None,
        }],
    ));
    ui.panel.as_mut().expect("queue panel").editing = Some("draft".into());
    assert_continue(dispatch!(key(KeyCode::Char('z'), KeyModifiers::NONE)));
    assert_continue(dispatch!(key(KeyCode::Backspace, KeyModifiers::NONE)));
    assert_continue(dispatch!(key(KeyCode::Esc, KeyModifiers::NONE)));

    action!(InputAction::Insert('/'));
    action!(InputAction::PopupOpen);
    ui.popup = Some(Popup {
        items: vec!["/help".into(), "/model".into()],
        selected: 0,
        anchor: 0,
    });
    if ui.popup.is_some() {
        action!(InputAction::PopupNext);
        action!(InputAction::PopupPrev);
        action!(InputAction::PopupAccept);
    }
    action!(InputAction::Insert('a'));
    action!(InputAction::NewLine);
    action!(InputAction::CursorUpOrHistory);
    action!(InputAction::CursorDownOrHistory);
    action!(InputAction::ToggleDetails);
    action!(InputAction::ToggleReasoning);
    action!(InputAction::ToggleAnswer);
    action!(InputAction::ToggleActivity);
    action!(InputAction::OpenLiveSearch);
    action!(InputAction::Queue);
    action!(InputAction::Insert('f'));
    action!(InputAction::PushNow);
    action!(InputAction::Insert('/'));
    ui.popup = Some(Popup {
        items: vec!["/help".into()],
        selected: 0,
        anchor: 0,
    });
    action!(InputAction::PopupSubmit);
    action!(InputAction::PopupClose);

    ui.push_chunk(provider::StreamChunk::Answer("more answer".into()));
    assert_continue(dispatch!(key(KeyCode::Char(' '), KeyModifiers::CONTROL)));
    assert_continue(dispatch!(release_key(
        KeyCode::Char(' '),
        KeyModifiers::CONTROL
    )));
    assert_continue(dispatch!(key(KeyCode::PageUp, KeyModifiers::NONE)));
    assert_continue(dispatch!(key(KeyCode::PageDown, KeyModifiers::NONE)));
    ui.push_tool(
        ToolBlock::from_lines(vec![
            ("tool summary".into(), Color::Yellow),
            ("detail line one".into(), Color::White),
            ("detail line two".into(), Color::White),
        ])
        .expect("tool block"),
    );
    assert_continue(dispatch!(key(KeyCode::Up, KeyModifiers::ALT)));
    assert_continue(dispatch!(key(KeyCode::Down, KeyModifiers::ALT)));
    assert_continue(dispatch!(key(KeyCode::Char(' '), KeyModifiers::CONTROL)));
    assert_continue(dispatch!(key(KeyCode::Left, KeyModifiers::ALT)));
    assert_continue(dispatch!(key(KeyCode::Right, KeyModifiers::ALT)));
    assert_continue(dispatch!(key(KeyCode::Char(' '), KeyModifiers::NONE)));
    assert_continue(dispatch!(key(KeyCode::PageUp, KeyModifiers::ALT)));
    assert_continue(dispatch!(key(KeyCode::PageDown, KeyModifiers::ALT)));
    assert_continue(dispatch!(release_key(
        KeyCode::Char(' '),
        KeyModifiers::CONTROL
    )));

    let (reply, answer) = std::sync::mpsc::sync_channel(1);
    pending = Some(ApprovalRequest {
        action: "write_file".into(),
        detail: "README.md".into(),
        reply,
    });
    assert_continue(dispatch!(key(KeyCode::Up, KeyModifiers::NONE)));
    assert_continue(dispatch!(key(KeyCode::Char('y'), KeyModifiers::NONE)));
    assert!(answer.recv().expect("approval reply"));
    let (reject_reply, rejected) = std::sync::mpsc::sync_channel(1);
    pending = Some(ApprovalRequest {
        action: "write_file".into(),
        detail: "README.md".into(),
        reply: reject_reply,
    });
    assert_continue(dispatch!(key(KeyCode::Char('n'), KeyModifiers::NONE)));
    assert!(!rejected.recv().expect("rejection reply"));

    ui.busy = true;
    ui.queued.push_back("kept task".into());
    task_started = Some(Instant::now());
    task = Some(tokio::spawn(async {
        tokio::time::sleep(Duration::from_secs(60)).await;
    }));
    action!(InputAction::Interrupt);
    assert!(task.is_none());
    assert!(!ui.busy);

    assert_continue(dispatch!(key(KeyCode::Char('c'), KeyModifiers::CONTROL)));
    assert!(matches!(
        dispatch!(key(KeyCode::Char('c'), KeyModifiers::CONTROL)).expect("second Ctrl-C"),
        KeyEventResult::Exit
    ));
}

#[test]
fn extracted_token_handler_drains_bounded_wake_batch() {
    let mut ui = Ui::default();
    let mut last_activity = None;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    tx.send(provider::StreamChunk::Reasoning("one".into()))
        .expect("reasoning chunk");
    tx.send(provider::StreamChunk::Answer("two".into()))
        .expect("answer chunk");
    handle_token_chunk(
        provider::StreamChunk::Answer("first".into()),
        &mut ui,
        &mut last_activity,
        &mut rx,
    );
    assert!(ui.busy);
    assert!(!ui.waiting);
    assert_eq!(ui.stream_tokens, 1);
    assert!(ui.transcript.has_reasoning());
    assert!(ui.transcript.has_answer());
    assert!(last_activity.is_some());
}

#[tokio::test]
async fn extracted_event_step_covers_stream_approval_done_and_tick_branches() {
    let mut ui = Ui::default();
    let mut meta = test_meta();
    let swap = test_swap();
    let bus = agent::null_token_bus();
    let mut pending = None;
    let mut task = None;
    let mut task_started = None;
    let mut retry_count = 0;
    let mut pending_submit = None;
    let mut momentary_hold = false;
    let mut last_ctrl_c = None;
    let mut pressed = std::collections::HashSet::new();
    let keylog_path = None;
    let mut last_activity = None;
    let mut history = Vec::new();
    let mut printed = 0;
    let last_task = None;
    let mut session_tokens = 0;
    let mut session_turns = 0;
    let start_task: StartTask = Box::new(|_, _| tokio::spawn(async {}));
    let (key_tx, mut key_rx) = tokio::sync::mpsc::unbounded_channel();
    let (token_tx, mut token_rx) = tokio::sync::mpsc::unbounded_channel();
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    let (approval_tx, mut approval_rx) = tokio::sync::mpsc::unbounded_channel();
    let (done_tx, mut done_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut tick = tokio::time::interval(Duration::from_secs(3600));
    let terminal = test_crossterm_terminal();
    let mut animation_due = false;

    macro_rules! step {
        () => {
            run_event_step(EventStepContext {
                ui: &mut ui,
                meta: &mut meta,
                swap: &swap,
                bus: &bus,
                pending: &mut pending,
                task: &mut task,
                task_started: &mut task_started,
                retry_count: &mut retry_count,
                pending_submit: &mut pending_submit,
                momentary_hold: &mut momentary_hold,
                last_ctrl_c: &mut last_ctrl_c,
                pressed: &mut pressed,
                keylog_path: &keylog_path,
                last_activity: &mut last_activity,
                history: &mut history,
                printed: &mut printed,
                last_task: &last_task,
                session_tokens: &mut session_tokens,
                session_turns: &mut session_turns,
                start_task: &start_task,
                key_rx: &mut key_rx,
                token_rx: &mut token_rx,
                event_rx: &mut event_rx,
                approval_rx: &mut approval_rx,
                done_rx: &mut done_rx,
                tick: &mut tick,
                terminal: &terminal,
                animation_due: &mut animation_due,
            })
            .await
            .expect("event step")
        };
    }

    key_tx.send(Event::Resize(80, 24)).expect("key event");
    assert!(step!().dirty);
    token_tx
        .send(provider::StreamChunk::Answer("answer".into()))
        .expect("token event");
    assert!(step!().dirty);
    event_tx
        .send(langgraph::StreamEvent::NodeFinished {
            superstep: 1,
            node: "reason".into(),
        })
        .expect("stream event");
    assert!(step!().dirty);
    let (reply, answer) = std::sync::mpsc::sync_channel(1);
    approval_tx
        .send(ApprovalRequest {
            action: "write_file".into(),
            detail: "README.md".into(),
            reply,
        })
        .expect("approval event");
    assert!(step!().dirty);
    assert!(pending.is_some());
    done_tx
        .send(Err("invalid request".into()))
        .expect("done event");
    assert!(step!().dirty);
    assert!(!ui.busy);
    drop(answer);

    let _ = step!();
    assert!(!ui.waiting);
}

#[tokio::test]
async fn extracted_prepare_loop_covers_idle_poll_and_draw_decision() {
    let mut ui = Ui::default();
    let mut meta = test_meta();
    let swap = test_swap();
    let mut model_catalog_rx = None;
    let mut pending_submit = None;
    let mut task = None;
    let mut history = Vec::new();
    let agents = Arc::new(agent::Agents::default());
    let commands = Vec::new();
    let skills = Vec::new();
    let mut retry_count = 0;
    let mut last_task = None;
    let mut task_started = None;
    let mut last_activity = None;
    let mut printed = 0;
    let start_task: StartTask = Box::new(|_, _| tokio::spawn(async {}));
    let mut terminal = test_crossterm_terminal();
    let mut live_cache = LiveOutputCache::default();
    let pending = None;
    let mut dirty = false;
    let mut animation_due = false;

    assert!(!prepare_loop(&mut LoopPrepareContext {
        ui: &mut ui,
        meta: &mut meta,
        swap: &swap,
        model_catalog_rx: &mut model_catalog_rx,
        pending_submit: &mut pending_submit,
        task: &mut task,
        history: &mut history,
        agents: &agents,
        commands: &commands,
        skills: &skills,
        session_tokens: 0,
        session_turns: 0,
        retry_count: &mut retry_count,
        last_task: &mut last_task,
        task_started: &mut task_started,
        last_activity: &mut last_activity,
        printed: &mut printed,
        start_task: &start_task,
        terminal: &mut terminal,
        live_cache: &mut live_cache,
        pending: &pending,
        dirty: &mut dirty,
        animation_due: &mut animation_due,
    })
    .await
    .expect("idle prepare"));
    dirty = true;
    assert!(!prepare_loop(&mut LoopPrepareContext {
        ui: &mut ui,
        meta: &mut meta,
        swap: &swap,
        model_catalog_rx: &mut model_catalog_rx,
        pending_submit: &mut pending_submit,
        task: &mut task,
        history: &mut history,
        agents: &agents,
        commands: &commands,
        skills: &skills,
        session_tokens: 0,
        session_turns: 0,
        retry_count: &mut retry_count,
        last_task: &mut last_task,
        task_started: &mut task_started,
        last_activity: &mut last_activity,
        printed: &mut printed,
        start_task: &start_task,
        terminal: &mut terminal,
        live_cache: &mut live_cache,
        pending: &pending,
        dirty: &mut dirty,
        animation_due: &mut animation_due,
    })
    .await
    .expect("draw prepare"));
    assert!(!dirty);
}

#[test]
fn extracted_tick_handler_transitions_waiting_and_splash() {
    let mut ui = Ui::default();
    let terminal = test_crossterm_terminal();
    let idle = None;
    let pending = None;
    for _ in 0..SPLASH_TICKS {
        let _ = handle_tick(&mut ui, &idle, &pending, &terminal);
    }
    assert_eq!(ui.splash, SPLASH_TICKS);
    ui.busy = true;
    let stale = Some(Instant::now() - Duration::from_secs(9));
    assert!(!handle_tick(&mut ui, &stale, &pending, &terminal));
    assert!(ui.waiting);
}

#[test]
fn extracted_session_and_catalog_helpers_cover_idle_configuration_paths() {
    let history = vec![provider::Message::user("remember this")];
    let _ = session_input_history(&history);
    let mut ui = Ui::default();
    note_initial_ui(&mut ui, true, &history);
    assert!(!ui.commits.is_empty());

    let (approval_tx, _approval_rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = tui_approver(true, approval_tx.clone());
    let _ = tui_approver(false, approval_tx);

    let mut meta = test_meta();
    let swap = test_swap();
    let (catalog_tx, catalog_rx) = tokio::sync::oneshot::channel();
    drop(catalog_tx);
    let mut receiver = Some(catalog_rx);
    assert!(poll_model_catalog(&mut ui, &mut meta, &swap, &mut receiver,));
    assert!(ui.model_catalog.as_ref().is_some_and(Vec::is_empty));
}

#[test]
fn extracted_keylog_helper_writes_explicit_diagnostic_path() {
    let previous_flag = std::env::var_os("RIDGE_KEYLOG");
    std::env::set_var("RIDGE_KEYLOG", "1");
    assert!(keylog_path().is_some());
    if let Some(value) = previous_flag {
        std::env::set_var("RIDGE_KEYLOG", value);
    } else {
        std::env::remove_var("RIDGE_KEYLOG");
    }
    let path = std::env::temp_dir().join(format!("ridge-code-keylog-{}.txt", std::process::id()));
    let keylog_path = Some(path.clone());
    log_key_event(&Event::Resize(80, 24), &keylog_path);
    let log = std::fs::read_to_string(&path).expect("key log");
    assert!(log.contains("Resize"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn extracted_edit_input_helper_covers_cursor_and_history_actions() {
    let mut ui = Ui::default();
    edit_input(InputAction::Insert('a'), &mut ui);
    edit_input(InputAction::Backspace, &mut ui);
    edit_input(InputAction::Left, &mut ui);
    edit_input(InputAction::Right, &mut ui);
    edit_input(InputAction::Home, &mut ui);
    edit_input(InputAction::End, &mut ui);
    edit_input(InputAction::NewLine, &mut ui);
    ui.input.buffer = "wrapped input".into();
    ui.input.cursor = ui.input.buffer.chars().count();
    edit_input(InputAction::CursorUpOrHistory, &mut ui);
    edit_input(InputAction::CursorDownOrHistory, &mut ui);
    ui.input.history = vec!["previous".into()];
    ui.input.cursor = 0;
    edit_input(InputAction::CursorUpOrHistory, &mut ui);
    edit_input(InputAction::CursorDownOrHistory, &mut ui);
}

#[tokio::test]
async fn extracted_pending_submit_covers_command_and_task_paths() {
    let mut ui = Ui::default();
    let mut history = Vec::new();
    let mut meta = test_meta();
    let swap = test_swap();
    let agents = agent::Agents::default();
    let commands = Vec::new();
    let skills = Vec::new();
    let mut pending_submit = Some("/help".to_string());
    let mut retry_count = 0;
    let mut last_task = None;
    let mut task_started = None;
    let mut last_activity = None;
    let mut printed = 0;
    let mut task = None;
    let start_task: StartTask = Box::new(|_, _| tokio::spawn(async {}));

    assert!(!process_pending_submit(&mut PendingSubmitContext {
        ui: &mut ui,
        history: &mut history,
        meta: &mut meta,
        swap: &swap,
        agents: &agents,
        commands: &commands,
        skills: &skills,
        session_tokens: 0,
        session_turns: 0,
        pending_submit: &mut pending_submit,
        retry_count: &mut retry_count,
        last_task: &mut last_task,
        task_started: &mut task_started,
        last_activity: &mut last_activity,
        printed: &mut printed,
        task: &mut task,
        start_task: &start_task,
    })
    .await
    .expect("slash command"));
    assert!(pending_submit.is_none());

    pending_submit = Some("run this task".into());
    assert!(!process_pending_submit(&mut PendingSubmitContext {
        ui: &mut ui,
        history: &mut history,
        meta: &mut meta,
        swap: &swap,
        agents: &agents,
        commands: &commands,
        skills: &skills,
        session_tokens: 0,
        session_turns: 0,
        pending_submit: &mut pending_submit,
        retry_count: &mut retry_count,
        last_task: &mut last_task,
        task_started: &mut task_started,
        last_activity: &mut last_activity,
        printed: &mut printed,
        task: &mut task,
        start_task: &start_task,
    })
    .await
    .expect("task submit"));
    assert_eq!(last_task.as_deref(), Some("run this task"));
    assert!(task.take().is_some());
}

#[tokio::test]
async fn extracted_event_loop_exits_after_takeover_signal() {
    let (key_tx, key_rx) = tokio::sync::mpsc::unbounded_channel();
    key_tx
        .send(key(KeyCode::Char('c'), KeyModifiers::CONTROL))
        .expect("first Ctrl-C");
    key_tx
        .send(key(KeyCode::Char('c'), KeyModifiers::CONTROL))
        .expect("second Ctrl-C");
    let (_approval_tx, approval_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_token_tx, token_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_done_tx, done_rx) = tokio::sync::mpsc::unbounded_channel();
    let terminal = test_crossterm_terminal();
    let context = TuiLoopContext {
        swap: test_swap(),
        skills: Vec::new(),
        agents: Arc::new(agent::Agents::default()),
        commands: Vec::new(),
        history: Vec::new(),
        meta: test_meta(),
        terminal,
        ui: Ui::default(),
        live_cache: LiveOutputCache::default(),
        model_catalog_rx: None,
        approval_rx,
        event_rx,
        token_rx,
        done_rx,
        key_rx,
        tick: tokio::time::interval(Duration::from_secs(3600)),
        bus: agent::null_token_bus(),
        start_task: Box::new(|_, _| tokio::spawn(async {})),
        keylog_path: None,
        pending: None,
        task: None,
        session_tokens: 0,
        session_turns: 0,
        printed: 0,
        task_started: None,
        last_activity: None,
        pending_submit: None,
        retry_count: 0,
        last_task: None,
        pressed: std::collections::HashSet::new(),
        momentary_hold: false,
        last_ctrl_c: None,
        dirty: true,
        animation_due: false,
    };
    run_event_loop(context).await.expect("event loop");
}

#[test]
fn event_colours_are_semantic() {
    assert_eq!(event_color("verify: PASS"), Color::Green);
    assert_eq!(event_color("act: run_shell"), Color::Yellow);
    assert_eq!(event_color("(final) done"), role_color(Role::Answer));
}

#[test]
fn extracted_stream_loop_handlers_cover_node_and_tool_boundaries() {
    let mut ui = Ui::default();
    let task_started = None;
    let mut last_activity = None;
    let mut printed = 0;
    handle_stream_event(
        langgraph::StreamEvent::NodeFinished {
            superstep: 1,
            node: "reason".into(),
        },
        &mut StreamEventContext {
            ui: &mut ui,
            task_started: &task_started,
            last_activity: &mut last_activity,
            printed: &mut printed,
        },
    );
    assert!(ui.busy);
    let mut state = agent::AgentState::new("inspect");
    state.messages = vec![
        "act: read_file -> line one".into(),
        "(final) verified".into(),
    ];
    state.todos = vec![agent::Todo {
        content: "verify output".into(),
        status: "in_progress".into(),
    }];
    state.pending_call = Some(provider::ToolCall {
        id: "call-1".into(),
        name: "read_file".into(),
        arguments: serde_json::json!({"path":"README.md"}),
    });
    handle_stream_event(
        langgraph::StreamEvent::Superstep {
            step: 2,
            active: Vec::new(),
            state,
        },
        &mut StreamEventContext {
            ui: &mut ui,
            task_started: &task_started,
            last_activity: &mut last_activity,
            printed: &mut printed,
        },
    );
    assert_eq!(ui.superstep, 2);
    assert!(ui.pending_call.is_some());
    assert_eq!(printed, 2);
}

#[tokio::test]
async fn extracted_done_handler_records_non_retryable_failure() {
    let mut ui = Ui::default();
    let mut history = Vec::new();
    let mut task = None;
    let mut pending_submit = None;
    let mut momentary_hold = false;
    let mut task_started = None;
    let mut last_activity = None;
    let mut printed = 0;
    let mut retry_count = 0;
    let last_task = None;
    let mut session_tokens = 0;
    let mut session_turns = 0;
    let start_task: StartTask = Box::new(|_, _| panic!("non-retryable error must not retry"));
    handle_done_result(
        Err("invalid request".into()),
        &mut DoneEventContext {
            ui: &mut ui,
            history: &mut history,
            task: &mut task,
            pending_submit: &mut pending_submit,
            momentary_hold: &mut momentary_hold,
            task_started: &mut task_started,
            last_activity: &mut last_activity,
            printed: &mut printed,
            retry_count: &mut retry_count,
            last_task: &last_task,
            session_tokens: &mut session_tokens,
            session_turns: &mut session_turns,
            start_task: &start_task,
        },
    );
    assert!(!ui.busy);
    assert_eq!(retry_count, 0);
    assert_eq!(ui.activity, "stopped · error");

    let mut successful = agent::AgentState::new("verified");
    successful.approved = true;
    successful.steps = 2;
    successful.total_tokens = 9;
    successful.input_tokens = 3;
    successful.output_tokens = 6;
    successful.messages = vec!["(final) verified".into()];
    let mut success_history = Vec::new();
    let mut success_task = None;
    let mut success_pending = None;
    let mut success_hold = false;
    let mut success_started = Some(Instant::now());
    let mut success_activity = Some(Instant::now());
    let mut success_printed = 1;
    let mut success_retries = 1;
    let mut session_tokens = 0;
    let mut session_turns = 0;
    ui.queued.push_back("next task".into());
    handle_done_result(
        Ok(successful),
        &mut DoneEventContext {
            ui: &mut ui,
            history: &mut success_history,
            task: &mut success_task,
            pending_submit: &mut success_pending,
            momentary_hold: &mut success_hold,
            task_started: &mut success_started,
            last_activity: &mut success_activity,
            printed: &mut success_printed,
            retry_count: &mut success_retries,
            last_task: &None,
            session_tokens: &mut session_tokens,
            session_turns: &mut session_turns,
            start_task: &start_task,
        },
    );
    assert_eq!(ui.phase, "completed");
    assert_eq!(session_tokens, 9);
    assert_eq!(session_turns, 1);
    assert_eq!(success_retries, 0);
    assert_eq!(success_pending.as_deref(), Some("next task"));

    let mut stopped = agent::AgentState::new("not approved");
    stopped.approved = false;
    stopped.steps = 3;
    stopped.messages = vec!["(final) needs review".into()];
    handle_done_result(
        Ok(stopped),
        &mut DoneEventContext {
            ui: &mut ui,
            history: &mut success_history,
            task: &mut success_task,
            pending_submit: &mut success_pending,
            momentary_hold: &mut success_hold,
            task_started: &mut success_started,
            last_activity: &mut success_activity,
            printed: &mut success_printed,
            retry_count: &mut success_retries,
            last_task: &None,
            session_tokens: &mut session_tokens,
            session_turns: &mut session_turns,
            start_task: &start_task,
        },
    );
    assert_eq!(ui.phase, "stopped");

    let retry_start: StartTask = Box::new(|_, _| tokio::spawn(async {}));
    let retry_last_task = Some("retry this".to_string());
    let mut retry_task = None;
    let mut retry_pending = None;
    let mut retry_hold = false;
    let mut retry_started = None;
    let mut retry_activity = None;
    let mut retry_printed = 0;
    let mut retry_count = 0;
    handle_done_result(
        Err("provider timeout".into()),
        &mut DoneEventContext {
            ui: &mut ui,
            history: &mut success_history,
            task: &mut retry_task,
            pending_submit: &mut retry_pending,
            momentary_hold: &mut retry_hold,
            task_started: &mut retry_started,
            last_activity: &mut retry_activity,
            printed: &mut retry_printed,
            retry_count: &mut retry_count,
            last_task: &retry_last_task,
            session_tokens: &mut session_tokens,
            session_turns: &mut session_turns,
            start_task: &retry_start,
        },
    );
    assert_eq!(retry_count, 1);
    assert!(retry_task.take().is_some());

    let mut exhausted_task = None;
    let mut exhausted_pending = None;
    let mut exhausted_hold = false;
    let mut exhausted_started = None;
    let mut exhausted_activity = None;
    let mut exhausted_printed = 0;
    let mut exhausted_retries = 10;
    handle_done_result(
        Err("provider timeout".into()),
        &mut DoneEventContext {
            ui: &mut ui,
            history: &mut success_history,
            task: &mut exhausted_task,
            pending_submit: &mut exhausted_pending,
            momentary_hold: &mut exhausted_hold,
            task_started: &mut exhausted_started,
            last_activity: &mut exhausted_activity,
            printed: &mut exhausted_printed,
            retry_count: &mut exhausted_retries,
            last_task: &retry_last_task,
            session_tokens: &mut session_tokens,
            session_turns: &mut session_turns,
            start_task: &retry_start,
        },
    );
    assert_eq!(exhausted_retries, 0);
    assert!(exhausted_task.is_none());
}

#[test]
fn extracted_helpers_cover_task_reset_oauth_and_idle_polling() {
    let mut ui = Ui {
        busy: true,
        waiting: true,
        stream_tokens: 12,
        pending_call: Some(provider::ToolCall {
            id: "call".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({}),
        }),
        ..Ui::default()
    };
    reset_task_ui(&mut ui);
    assert!(ui.busy);
    assert!(!ui.waiting);
    assert_eq!(ui.stream_tokens, 0);
    assert!(ui.pending_call.is_none());

    let mut meta = ReplMeta {
        tools: Vec::new(),
        provider: "test".into(),
        provider_label: "test".into(),
        model: "model".into(),
        base_url: String::new(),
        status_bar: "{provider} 路 {model}".into(),
        ctx_window: 200_000,
    };
    let swap = Arc::new(provider::SwapProvider::new(Arc::new(
        provider::ScriptedProvider::new(Vec::new()),
    )));
    handle_device_oauth_event(
        DeviceOAuthEvent::Ready {
            user_code: "CODE".into(),
            opened: true,
        },
        &mut ui,
        &mut meta,
        &swap,
    );
    assert!(ui
        .device_auth_status
        .as_deref()
        .is_some_and(|text| text.contains("CODE")));
    handle_device_oauth_event(
        DeviceOAuthEvent::Ready {
            user_code: "OPEN".into(),
            opened: false,
        },
        &mut ui,
        &mut meta,
        &swap,
    );
    handle_device_oauth_event(
        DeviceOAuthEvent::Complete(Err("device stopped".into())),
        &mut ui,
        &mut meta,
        &swap,
    );
    assert!(ui
        .device_auth_status
        .as_deref()
        .is_some_and(|text| text.contains("device stopped")));
    assert!(poll_device_oauth(&mut ui).is_none());
    assert!(poll_oauth_callback(&mut ui).is_none());
    assert_eq!(superstep_activity("", None), "settling result");
    assert_eq!(superstep_activity("next", None), "next 路 next");
}

#[test]
fn read_file_result_is_collapsed_but_expandable() {
    let message = "act: read_file -> first line\nsecond line\nthird line";
    let block = tool_preview(message).expect("read result should be a tool block");
    let compact = block.live_lines();
    assert!(compact
        .iter()
        .any(|line| line.text.contains("Read complete")));
    assert!(compact
        .iter()
        .any(|line| line.text.contains("Ctrl+O details")));
    assert!(!compact.iter().any(|line| line.text == "second line"));

    let mut expanded = block;
    assert!(expanded.toggle());
    assert!(expanded
        .live_lines()
        .iter()
        .any(|line| line.text.contains("second line")));
}

#[test]
fn long_tool_preview_keeps_both_file_ends_when_expanded() {
    let content = (0..24)
        .map(|index| format!("line {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let preview = preview_lines(&content, 12);
    let rendered = preview
        .iter()
        .map(|(line, _)| line.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("line 0"));
    assert!(rendered.contains("line 23"));
    assert!(rendered.contains("folded"));
    assert!(!rendered.contains("line 12"));
}

#[test]
fn read_file_call_and_result_share_one_collapsed_tool_block() {
    let call = r#"reason#1: tool_call read_file {"path":"src/x.rs"}"#;
    let result = "act: read_file -> first line\nsecond line\nthird line";
    let mut ui = Ui::default();
    ui.push_tool(tool_preview(call).expect("read call"));
    ui.push_tool(tool_preview(result).expect("read result"));

    let compact = ui.transcript.visible_lines(8);
    assert_eq!(
        compact
            .iter()
            .filter(|line| line.kind == LiveLineKind::ToolSummary)
            .count(),
        1,
        "call/result must collapse to one tool block"
    );
    assert!(compact
        .iter()
        .any(|line| line.text.contains("Read complete")));
    assert!(!compact.iter().any(|line| line.text == "second line"));

    assert!(ui.toggle_details());
    let expanded = ui.transcript.visible_lines(8);
    assert!(expanded
        .iter()
        .any(|line| line.text.contains("Read src/x.rs")));
    assert!(expanded
        .iter()
        .any(|line| line.text.contains("second line")));
    ui.commit_live_tools();
    assert_eq!(ui.tool_history.len(), 1);
}

#[test]
fn consecutive_read_file_results_group_into_one_collapsed_batch() {
    let mut ui = Ui::default();
    for (index, path) in [(1, "src/alpha.rs"), (2, "src/beta.rs")] {
        let call = format!(r#"reason#{index}: tool_call read_file {{"path":"{path}"}}"#);
        let result = format!("act: read_file -> line {index}\nnext line {index}");
        ui.push_tool(tool_preview(&call).expect("read call"));
        ui.push_tool(tool_preview(&result).expect("read result"));
    }

    let compact = ui.transcript.visible_lines(8);
    let summaries = compact
        .iter()
        .filter(|line| line.kind == LiveLineKind::ToolSummary)
        .collect::<Vec<_>>();
    assert_eq!(
        summaries.len(),
        1,
        "adjacent reads should occupy one row: {compact:?}"
    );
    assert!(summaries[0].text.contains("Read batch · 2 files"));
    assert!(compact
        .iter()
        .any(|line| line.text.contains("Ctrl+O details")));
    assert!(!compact.iter().any(|line| line.text.contains("next line")));

    assert!(ui.toggle_details());
    let expanded = ui.transcript.visible_lines(16);
    assert!(expanded
        .iter()
        .any(|line| line.text.contains("Read src/alpha.rs")));
    assert!(expanded
        .iter()
        .any(|line| line.text.contains("Read src/beta.rs")));
    assert!(expanded
        .iter()
        .any(|line| line.text.contains("next line 1")));
    assert!(expanded
        .iter()
        .any(|line| line.text.contains("next line 2")));
}

#[test]
fn read_file_batch_preserves_detail_order_and_bound() {
    let mut ui = Ui::default();
    for index in 0..4 {
        let path = format!("src/read-{index}.rs");
        let call = format!(r#"reason#{index}: tool_call read_file {{"path":"{path}"}}"#);
        let body = (0..20)
            .map(|line| format!("file {index} line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = format!("act: read_file -> {body}");
        ui.push_tool(tool_preview(&call).expect("read call"));
        ui.push_tool(tool_preview(&result).expect("read result"));
    }
    assert!(ui.toggle_details());
    ui.commit_live_tools();

    assert_eq!(ui.tool_history.len(), 1);
    let batch = ui.tool_history.front().expect("read batch");
    assert!(batch.summary().contains("Read batch · 4 files"));
    let first = batch
        .summary()
        .find("src/read-0.rs")
        .expect("first path indexed");
    let last = batch
        .summary()
        .find("src/read-2.rs")
        .expect("third path indexed");
    assert!(
        first < last,
        "read summary lost arrival order: {}",
        batch.summary()
    );
    assert!(batch.summary().contains("+1 more"));
    let details = batch.details_text();
    assert!(
        details.lines().count() <= 32,
        "detail bound exceeded: {details}"
    );
    assert!(
        details.contains("file 0 line 0"),
        "detail head lost: {details}"
    );
    assert!(
        details.contains("file 3 line 19"),
        "detail tail lost: {details}"
    );
}

#[test]
fn read_file_batch_does_not_cross_other_tools() {
    let mut ui = Ui::default();
    let read_call = |index: usize| {
        tool_preview(&format!(
            r#"reason#{index}: tool_call read_file {{"path":"src/read-{index}.rs"}}"#
        ))
        .expect("read call")
    };
    let read_result = |index: usize| {
        tool_preview(&format!("act: read_file -> content {index}")).expect("read result")
    };

    ui.push_tool(read_call(1));
    ui.push_tool(read_result(1));
    ui.push_tool(
        tool_preview(r#"reason#2: tool_call run_shell {"cmd":"echo gap"}"#).expect("shell call"),
    );
    ui.push_tool(tool_preview("act: run_shell -> gap").expect("shell result"));
    ui.push_tool(read_call(3));
    ui.push_tool(read_result(3));

    let summaries = ui
        .transcript
        .visible_lines(16)
        .into_iter()
        .filter(|line| line.kind == LiveLineKind::ToolSummary)
        .map(|line| line.text.to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        summaries.len(),
        3,
        "the shell must split read batches: {summaries:?}"
    );
    assert_eq!(
        summaries
            .iter()
            .filter(|line| line.contains("Read batch"))
            .count(),
        0
    );
    assert_eq!(
        summaries
            .iter()
            .filter(|line| line.contains("Read complete"))
            .count(),
        2
    );
    assert!(summaries.iter().any(|line| line.contains("run_shell")));
}

#[test]
fn activity_history_is_bounded_deduplicated_and_newest_first() {
    let mut ui = Ui::default();
    ui.set_activity("starting task");
    ui.set_activity("starting task");
    for step in 0..(MAX_ACTIVITY_HISTORY + 3) {
        ui.set_activity(format!("node · {step}"));
    }

    assert_eq!(ui.activity_history.len(), MAX_ACTIVITY_HISTORY);
    assert_eq!(ui.activity, format!("node · {}", MAX_ACTIVITY_HISTORY + 2));
    // A task boundary is a retained audit anchor, so it survives transient
    // node chatter while the bounded history continues to evict old chatter.
    assert_eq!(ui.activity_history.front().unwrap().text, "starting task");
    assert_eq!(
        ui.activity_history.back().unwrap().text,
        format!("node · {}", MAX_ACTIVITY_HISTORY + 2)
    );

    let panel = activity_panel(&ui.activity_history);
    assert_eq!(panel.kind, PanelKind::Activity);
    assert_eq!(panel.rows.len(), MAX_ACTIVITY_HISTORY);
    assert_eq!(panel.rows[0].key, "SYS now");
    assert_eq!(
        panel.rows[0].value,
        format!("node · {}", MAX_ACTIVITY_HISTORY + 2)
    );

    ui.open_activity_panel();
    ui.set_activity("completed");
    assert_eq!(
        ui.panel
            .as_ref()
            .and_then(|panel| panel.selected())
            .map(|row| row.value.as_str()),
        Some("completed")
    );
    ui.toggle_activity_panel();
    assert!(ui.panel.is_none());
}

#[test]
fn lifecycle_boundaries_keep_phase_and_activity_aligned() {
    let mut ui = Ui {
        phase: "verifying".into(),
        ..Ui::default()
    };

    ui.mark_task_outcome_with_reason(true, None);
    assert_eq!(ui.phase, "completed");
    assert_eq!(ui.activity, "completed");
    assert_eq!(
        ui.activity_history.back().unwrap().kind,
        ActivityKind::Completed
    );

    ui.mark_task_outcome_with_reason(false, None);
    assert_eq!(ui.phase, "stopped");
    assert_eq!(ui.activity, "stopped · not approved");
    assert_eq!(
        ui.activity_history.back().unwrap().kind,
        ActivityKind::Error
    );

    ui.mark_error();
    assert_eq!(ui.phase, "error");
    assert_eq!(ui.activity, "stopped · error");

    ui.mark_takeover_ready();
    assert_eq!(ui.phase, "takeover");
    assert_eq!(ui.activity, "takeover ready");

    ui.mark_approval_required();
    assert_eq!(ui.phase, "approval");
    assert_eq!(ui.activity, "approval required · user can take over");
}

#[test]
fn halt_reason_display_keeps_stall_diagnosis_and_recovery_bounded() {
    assert_eq!(
        halt_reason_display(HaltReason::Stall),
        "no verified progress"
    );
    assert!(halt_reason_guidance(HaltReason::Stall).contains("inspect reasoning/tools"));
    assert!(halt_reason_guidance(HaltReason::Stall).len() < 80);

    let mut ui = Ui::default();
    ui.mark_task_outcome_with_reason(false, Some(halt_reason_display(HaltReason::Stall)));
    assert_eq!(ui.phase, "stopped");
    assert_eq!(ui.activity, "stopped · not approved · no verified progress");
    assert_eq!(
        ui.activity_history.back().unwrap().kind,
        ActivityKind::Error
    );
}

#[test]
fn activity_history_retains_actionable_boundaries_during_node_chatter() {
    let mut ui = Ui::default();
    ui.record_activity(ActivityKind::Waiting, "waiting · no stream for 8s");
    ui.record_activity(ActivityKind::Conclusion, "settling result");
    ui.record_activity(ActivityKind::Completed, "completed");

    for step in 0..(MAX_ACTIVITY_HISTORY + 4) {
        ui.record_activity(ActivityKind::Reasoning, format!("node · reason {step}"));
    }

    assert_eq!(ui.activity_history.len(), MAX_ACTIVITY_HISTORY);
    for text in ["waiting · no stream for 8s", "settling result", "completed"] {
        assert!(
            ui.activity_history.iter().any(|entry| entry.text == text),
            "retained activity signal missing: {text}"
        );
    }
    assert_eq!(ui.activity_history.back().unwrap().text, "node · reason 15");
}

#[test]
fn activity_panel_keeps_latest_event_visible_on_narrow_terminal() {
    let mut ui = Ui::default();
    ui.set_activity("model · thinking");
    ui.set_activity("tool · read_file");
    let panel = activity_panel(&ui.activity_history);
    let mut terminal =
        Terminal::new(ratatui::backend::TestBackend::new(32, 10)).expect("activity terminal");
    terminal
        .draw(|frame| draw_panel(frame, frame.area(), &panel))
        .expect("activity draw");
    let symbols = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        symbols.contains("read_file"),
        "latest activity missing: {symbols}"
    );
}

#[test]
fn activity_panel_can_expand_selected_event_detail() {
    let mut ui = Ui::default();
    ui.set_activity("model · waiting for a long-running investigation conclusion");
    let mut panel = activity_panel(&ui.activity_history);

    assert!(panel.supports_detail());
    assert!(panel.toggle_detail());
    assert!(panel.detail_open);

    let mut terminal =
        Terminal::new(ratatui::backend::TestBackend::new(40, 12)).expect("activity terminal");
    terminal
        .draw(|frame| draw_panel(frame, frame.area(), &panel))
        .expect("activity detail draw");
    let symbols = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        symbols.contains("long-running"),
        "activity detail missing: {symbols}"
    );
}

#[test]
fn reasoning_history_panel_exposes_think_rail_and_detail_anchor() {
    let mut ui = Ui::default();
    ui.push_chunk(provider::StreamChunk::Reasoning("inspect state".into()));
    ui.commit_live_reasoning(3, 2);
    let mut panel = reasoning_history_panel(&ui.reasoning_history);
    assert!(panel.toggle_detail());

    let mut terminal =
        Terminal::new(ratatui::backend::TestBackend::new(48, 14)).expect("reasoning terminal");
    terminal
        .draw(|frame| draw_panel(frame, frame.area(), &panel))
        .expect("reasoning draw");
    let symbols = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        symbols.contains("THINK"),
        "semantic detail anchor missing: {symbols}"
    );
    assert!(
        symbols.contains("┃") || symbols.contains("│"),
        "reasoning rail missing: {symbols}"
    );
}

#[test]
fn reasoning_history_row_identifies_thinking_and_character_count() {
    let mut ui = Ui::default();
    let body = "inspect state";
    ui.push_chunk(provider::StreamChunk::Reasoning(body.into()));
    ui.commit_live_reasoning(3, 2);

    let panel = reasoning_history_panel(&ui.reasoning_history);
    let row = panel.rows.first().expect("reasoning history row");
    assert!(row.key.contains("THINK"), "{}", row.key.as_str());
    assert!(
        row.key.contains(&format!("{} chars", body.chars().count())),
        "{}",
        row.key.as_str()
    );
    assert!(row.key.contains("p#"), "{}", row.key.as_str());

    let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(48, 8))
        .expect("reasoning history metadata terminal");
    terminal
        .draw(|frame| draw_panel(frame, frame.area(), &panel))
        .expect("reasoning history metadata draw");
    let symbols = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        symbols.contains("THINK"),
        "row label disappeared: {symbols}"
    );
    assert!(symbols.contains("chars"), "row size disappeared: {symbols}");
}

#[test]
fn live_inspector_detail_anchor_tracks_answer_and_thinking_focus() {
    let mut ui = Ui::default();
    ui.push_chunk(provider::StreamChunk::Reasoning("plan".into()));
    ui.push_chunk(provider::StreamChunk::Answer("answer".into()));
    assert!(ui.open_live_history());
    let panel = ui.panel.as_mut().expect("live panel");
    assert!(panel.toggle_detail());

    let mut terminal =
        Terminal::new(ratatui::backend::TestBackend::new(56, 14)).expect("live terminal");
    terminal
        .draw(|frame| draw_panel(frame, frame.area(), panel))
        .expect("answer detail draw");
    let answer_symbols = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        answer_symbols.contains("ANSWER"),
        "answer detail anchor missing: {answer_symbols}"
    );

    panel.move_down();
    terminal
        .draw(|frame| draw_panel(frame, frame.area(), panel))
        .expect("thinking detail draw");
    let thinking_symbols = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        thinking_symbols.contains("THINK"),
        "thinking detail anchor missing: {thinking_symbols}"
    );
}

#[test]
fn wide_live_inspector_keeps_block_list_and_detail_side_by_side() {
    let mut ui = Ui::default();
    ui.push_chunk(provider::StreamChunk::Reasoning("plan".into()));
    ui.push_chunk(provider::StreamChunk::Answer("answer detail".into()));
    assert!(ui.open_live_history());
    let panel = ui.panel.as_mut().expect("live panel");
    assert!(panel.toggle_detail());

    let mut terminal =
        Terminal::new(ratatui::backend::TestBackend::new(96, 16)).expect("wide live terminal");
    terminal
        .draw(|frame| draw_panel(frame, frame.area(), panel))
        .expect("wide live detail draw");
    let buffer = terminal.backend().buffer();
    let area = buffer.area();
    let width = area.width as usize;
    let divider_visible = buffer.content().iter().enumerate().any(|(index, cell)| {
        let x = (index % width) as u16;
        let y = (index / width) as u16;
        cell.symbol() == "│"
            && x >= 20
            && x < area.width.saturating_sub(20)
            && y >= 2
            && y < area.height.saturating_sub(2)
    });
    let symbols = buffer
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        divider_visible,
        "wide inspector lost adaptive divider: {symbols}"
    );
    assert!(
        symbols.contains("ANSWER"),
        "answer detail anchor missing: {symbols}"
    );
    assert!(
        symbols.contains("answer detail"),
        "answer content missing: {symbols}"
    );
}

#[test]
fn audit_detail_reuses_markdown_heading_style() {
    let mut panel = Panel::new(
        PanelKind::ReasoningHistory,
        "reasoning".into(),
        vec![PanelRow {
            key: "#1 step 4 · 8 tok · +2s".into(),
            value:
                "# Decision\n\n> [!WARNING] caution\n> keep this\n\n```rust\nlet answer = 1;\n```"
                    .into(),
            ctx: None,
        }],
    );
    assert!(panel.toggle_detail());

    let mut terminal =
        Terminal::new(ratatui::backend::TestBackend::new(56, 16)).expect("markdown detail");
    terminal
        .draw(|frame| draw_panel(frame, frame.area(), &panel))
        .expect("markdown detail draw");
    let cells = terminal.backend().buffer().content();
    assert!(
        cells.iter().any(|cell| {
            cell.symbol() == "#"
                && cell.fg == role_color(Role::Primary)
                && cell.modifier.contains(Modifier::BOLD)
        }),
        "markdown heading lost semantic style"
    );
    let symbols = cells.iter().map(|cell| cell.symbol()).collect::<String>();
    assert!(
        symbols.contains("let"),
        "markdown code detail missing: {symbols}"
    );
    assert!(
        cells.iter().any(|cell| cell.fg == role_color(Role::Warn)),
        "markdown alert lost semantic warning style"
    );
}

#[test]
fn reasoning_history_detail_preserves_markdown_alert_edges() {
    let panel = Panel::new(
        PanelKind::ReasoningHistory,
        "reasoning".into(),
        vec![PanelRow {
            key: "#1 reasoning".into(),
            value: "> [!WARNING] protect the boundary\n> continue this conclusion".into(),
            ctx: None,
        }],
    );
    let detail = panel.selected().expect("detail row");
    let mut cache = DetailLayoutCache::default();
    cache.prepare(
        panel.content_revision,
        panel.selected_index().expect("selection"),
        &detail.value,
        panel.kind,
        &detail.key,
        96,
    );
    let rendered = cache
        .text()
        .lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert!(
        rendered.iter().any(|line| line.contains("┌ WARNING")),
        "alert top edge missing: {rendered:?}"
    );
    assert!(
        rendered.iter().any(|line| line.contains("└ continue")),
        "alert bottom edge missing: {rendered:?}"
    );
}

#[test]
fn narrow_open_detail_prioritizes_audit_body_over_competing_list_rows() {
    let mut panel = Panel::new(
        PanelKind::ReasoningHistory,
        "reasoning".into(),
        vec![
            PanelRow {
                key: "#1 selected".into(),
                value: "# conclusion\n\nretain the selected reasoning body".into(),
                ctx: None,
            },
            PanelRow {
                key: "#2 LIST-ONLY-ROW".into(),
                value: "this row should yield the narrow detail viewport".into(),
                ctx: None,
            },
        ],
    );
    assert!(panel.toggle_detail());

    let mut terminal =
        Terminal::new(ratatui::backend::TestBackend::new(40, 12)).expect("narrow detail");
    terminal
        .draw(|frame| draw_panel(frame, frame.area(), &panel))
        .expect("narrow detail draw");
    let symbols = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        symbols.contains("retain the selected"),
        "selected detail was squeezed out: {symbols}"
    );
    assert!(
        !symbols.contains("LIST-ONLY-ROW"),
        "narrow modal still spent rows on the competing list: {symbols}"
    );
    assert!(
        symbols.contains("Esc"),
        "close affordance disappeared: {symbols}"
    );
}

#[test]
fn narrow_audit_modal_clears_underlying_gutters() {
    let panel = Panel::new(
        PanelKind::AnswerHistory,
        "answers".into(),
        vec![PanelRow {
            key: "#1 answer".into(),
            value: "answer body".into(),
            ctx: None,
        }],
    );
    let mut terminal =
        Terminal::new(ratatui::backend::TestBackend::new(40, 12)).expect("audit terminal");
    terminal
        .draw(|frame| {
            frame.render_widget(
                Block::default().style(Style::default().fg(Color::Red)),
                frame.area(),
            );
            draw_panel(frame, frame.area(), &panel);
        })
        .expect("audit draw");
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer.cell((0, 0)).expect("gutter cell").fg, Color::Reset);
    assert_eq!(buffer.cell((1, 5)).expect("gutter cell").fg, Color::Reset);
}

#[test]
fn narrow_answer_metadata_wraps_at_word_boundaries() {
    let panel = Panel::new(
        PanelKind::AnswerHistory,
        "answers".into(),
        vec![PanelRow {
            key: "#1 ANSWER · step 1 · 25 tok · +0s · 51 chars · p#2".into(),
            value: "answer body".into(),
            ctx: None,
        }],
    );
    let mut terminal =
        Terminal::new(ratatui::backend::TestBackend::new(18, 12)).expect("narrow answers");
    terminal
        .draw(|frame| draw_panel(frame, frame.area(), &panel))
        .expect("narrow answers draw");
    let width = 18;
    let rows = (0..12)
        .map(|y| {
            (0..width)
                .map(|x| terminal.backend().buffer().cell((x, y)).unwrap().symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert!(
        rows.iter().any(|row| row.contains("▸ #1 ANSWER")),
        "answer label disappeared: {rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.contains("▸ step 1")),
        "metadata split through a word: {rows:?}"
    );
}

#[test]
fn medium_answer_hint_preserves_expand_and_close_actions() {
    let panel = Panel::new(
        PanelKind::AnswerHistory,
        "answers".into(),
        vec![PanelRow {
            key: "#1 ANSWER".into(),
            value: "answer body".into(),
            ctx: None,
        }],
    );
    for width in [32, 40] {
        let mut terminal =
            Terminal::new(ratatui::backend::TestBackend::new(width, 12)).expect("medium answers");
        terminal
            .draw(|frame| draw_panel(frame, frame.area(), &panel))
            .expect("medium answers draw");
        let symbols = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            symbols.contains("Enter expand"),
            "expand affordance clipped at {width}: {symbols}"
        );
        assert!(
            symbols.contains("Esc close"),
            "close affordance clipped at {width}: {symbols}"
        );
    }
}

#[test]
fn narrow_history_hint_names_expand_action() {
    let panel = Panel::new(
        PanelKind::AnswerHistory,
        "answers".into(),
        vec![PanelRow {
            key: "#1 ANSWER".into(),
            value: "answer body".into(),
            ctx: None,
        }],
    );

    let narrow = panel_hint(&panel, 18);
    assert!(
        narrow.contains("Enter") && narrow.contains('↗'),
        "missing expand affordance: {narrow}"
    );
    assert!(narrow.contains("Esc"), "missing close affordance: {narrow}");
    assert!(str_cells(&narrow) <= 18, "hint overflow: {narrow}");

    let micro = panel_hint(&panel, 14);
    assert!(
        micro.contains("Enter") && micro.contains('↗'),
        "missing micro expand affordance: {micro}"
    );
    assert!(
        micro.contains("Esc"),
        "missing micro close affordance: {micro}"
    );
    assert!(str_cells(&micro) <= 14, "micro hint overflow: {micro}");
}

#[test]
fn narrow_live_history_hint_names_expand_action() {
    let panel = Panel::new(
        PanelKind::LiveHistory,
        "live".into(),
        vec![PanelRow {
            key: "#1 ANSWER".into(),
            value: "answer body".into(),
            ctx: None,
        }],
    );

    let narrow = panel_hint(&panel, 18);
    assert!(narrow.contains("^Space"), "hold/follow hidden: {narrow}");
    assert!(
        narrow.contains("Enter") && narrow.contains('↗'),
        "missing live expand affordance: {narrow}"
    );
    assert!(narrow.contains("Esc"), "missing close affordance: {narrow}");
    assert!(str_cells(&narrow) <= 18, "hint overflow: {narrow}");

    let micro = panel_hint(&panel, 14);
    assert!(micro.contains("^Sp"), "micro hold/follow hidden: {micro}");
    assert!(
        micro.contains("Enter") && micro.contains('↗'),
        "missing micro live expand affordance: {micro}"
    );
    assert!(
        micro.contains("Esc"),
        "missing micro close affordance: {micro}"
    );
    assert!(str_cells(&micro) <= 14, "micro hint overflow: {micro}");
}

#[test]
fn detail_layout_cache_reuses_same_snapshot_and_invalidates_on_width_or_panel() {
    let panel = Panel::new(
        PanelKind::ReasoningHistory,
        "reasoning".into(),
        vec![PanelRow {
            key: "#1 decision".into(),
            value: "# Decision\n\n```rust\nlet answer = 1;\n```".into(),
            ctx: None,
        }],
    );
    let detail = panel.selected().expect("detail row");
    let mut cache = DetailLayoutCache::default();
    let first = cache.prepare(
        panel.content_revision,
        panel.selected_index().expect("selection"),
        &detail.value,
        panel.kind,
        &detail.key,
        32,
    );
    let second = cache.prepare(
        panel.content_revision,
        panel.selected_index().expect("selection"),
        &detail.value,
        panel.kind,
        &detail.key,
        32,
    );
    assert_eq!(first, second);
    assert_eq!(cache.rebuilds(), 1);

    let _ = cache.prepare(
        panel.content_revision,
        panel.selected_index().expect("selection"),
        &detail.value,
        panel.kind,
        &detail.key,
        24,
    );
    assert_eq!(cache.rebuilds(), 2);

    let replacement = Panel::new(
        PanelKind::ReasoningHistory,
        "reasoning".into(),
        vec![PanelRow {
            key: "#2 decision".into(),
            value: "replacement".into(),
            ctx: None,
        }],
    );
    let replacement_detail = replacement.selected().expect("replacement row");
    let _ = cache.prepare(
        replacement.content_revision,
        replacement.selected_index().expect("replacement selection"),
        &replacement_detail.value,
        replacement.kind,
        &replacement_detail.key,
        24,
    );
    assert_eq!(cache.rebuilds(), 3);
}

#[test]
fn panel_items_cache_reuses_wrapped_snapshot_and_invalidates_on_view_changes() {
    let mut panel = Panel::new(
        PanelKind::ReasoningHistory,
        "reasoning".into(),
        vec![
            PanelRow {
                key: "#1 first".into(),
                value: "a long reasoning detail that wraps in a narrow audit list".into(),
                ctx: None,
            },
            PanelRow {
                key: "#2 second".into(),
                value: "another reasoning detail".into(),
                ctx: None,
            },
        ],
    );
    let mut cache = PanelItemsCache::default();
    assert_eq!(cache.items(&panel, 24).len(), 2);
    assert_eq!(cache.items(&panel, 24).len(), 2);
    assert_eq!(cache.rebuilds(), 1);
    let (visible, selected) = cache.viewport(&panel, 24, 1, Some(1));
    assert_eq!(visible.len(), 1, "viewport should omit off-screen items");
    assert_eq!(
        selected,
        Some(0),
        "selection must be remapped into the window"
    );

    panel.query = "second".into();
    panel.retype();
    assert_eq!(cache.items(&panel, 24).len(), 1);
    assert_eq!(cache.rebuilds(), 2);

    panel.detail_open = true;
    let _ = cache.items(&panel, 24);
    assert_eq!(cache.rebuilds(), 3);
    let _ = cache.items(&panel, 16);
    assert_eq!(cache.rebuilds(), 4);
}

#[test]
fn standard_panel_selection_reuses_wrapped_items() {
    let mut panel = Panel::new(
        PanelKind::Config,
        "config".into(),
        (0..32)
            .map(|index| PanelRow {
                key: format!("setting-{index}"),
                value: "a value that wraps in a narrow list".into(),
                ctx: None,
            })
            .collect(),
    );
    let mut cache = PanelItemsCache::default();

    let _ = cache.items(&panel, 20);
    assert_eq!(cache.rebuilds(), 1);
    panel.move_down();
    let _ = cache.items(&panel, 20);
    assert_eq!(
        cache.rebuilds(),
        1,
        "standard list selection is rendered by ListState, not item content"
    );

    panel.query = "setting-3".into();
    panel.retype();
    let _ = cache.items(&panel, 20);
    assert_eq!(cache.rebuilds(), 2, "query changes still invalidate rows");
}

#[test]
fn detail_panel_selection_still_rebuilds_semantic_markers() {
    let mut panel = Panel::new(
        PanelKind::AnswerHistory,
        "answers".into(),
        vec![
            PanelRow {
                key: "#1 answer".into(),
                value: "first answer".into(),
                ctx: None,
            },
            PanelRow {
                key: "#2 answer".into(),
                value: "second answer".into(),
                ctx: None,
            },
        ],
    );
    let mut cache = PanelItemsCache::default();

    let _ = cache.items(&panel, 32);
    assert_eq!(cache.rebuilds(), 1);
    panel.move_down();
    let _ = cache.items(&panel, 32);
    assert_eq!(
        cache.rebuilds(),
        2,
        "detail rows encode the selected marker in their content"
    );
}

#[test]
fn panel_viewport_range_respects_wrapped_item_heights() {
    let heights = [1, 2, 1, 3, 1, 1];
    let (start, end) = panel_viewport_range(&heights, 4, Some(4));
    assert!(start <= 4 && 4 < end, "selected item fell outside viewport");
    let used = heights[start..end].iter().sum::<usize>();
    assert!(
        used <= 4 || heights[4] > 4,
        "window exceeded physical budget"
    );

    let (start, end) = panel_viewport_range(&heights, 3, None);
    assert_eq!((start, end), (0, 2));
}

#[test]
fn detail_layout_cache_uses_ratatui_rendered_height_for_cjk_markdown() {
    let panel = Panel::new(
        PanelKind::ReasoningHistory,
        "reasoning".into(),
        vec![PanelRow {
            key: "#1 decision".into(),
            value: "# 结论\n\n> [!WARNING] 注意\n> 保留这段\n\n```rust\nlet answer = 1;\n```"
                .into(),
            ctx: None,
        }],
    );
    let detail = panel.selected().expect("detail row");
    let width = 13;
    let mut cache = DetailLayoutCache::default();
    let rows = cache.prepare(
        panel.content_revision,
        panel.selected_index().expect("selection"),
        &detail.value,
        panel.kind,
        &detail.key,
        width,
    );
    let expected = Paragraph::new(cache.text())
        .wrap(Wrap { trim: false })
        .line_count(width)
        .max(1);
    assert_eq!(rows, expected);
    assert!(
        rows >= 6,
        "CJK/markdown detail unexpectedly collapsed: {rows}"
    );
}

#[test]
fn activity_panel_enter_toggles_detail_without_closing() {
    let mut ui = Ui::default();
    ui.set_activity("model · waiting for investigation");
    ui.open_activity_panel();
    let mut meta = ReplMeta {
        tools: Vec::new(),
        provider: "test".into(),
        provider_label: "test".into(),
        model: "model".into(),
        base_url: String::new(),
        status_bar: "{provider} · {model}".into(),
        ctx_window: 200_000,
    };
    let swap = Arc::new(provider::SwapProvider::new(Arc::new(
        provider::ScriptedProvider::new(Vec::new()),
    )));

    panel_enter(&mut ui, &mut meta, &swap);
    assert!(ui.panel.as_ref().is_some_and(|panel| panel.detail_open));
    panel_enter(&mut ui, &mut meta, &swap);
    assert!(ui.panel.as_ref().is_some_and(|panel| !panel.detail_open));
}

#[test]
fn activity_panel_refresh_preserves_live_selection_and_detail() {
    let mut ui = Ui::default();
    ui.set_activity("model · thinking");
    ui.open_activity_panel();
    assert!(ui.panel.as_mut().expect("activity panel").toggle_detail());

    ui.set_activity("tool · read_file");

    let panel = ui.panel.as_ref().expect("refreshed activity panel");
    assert_eq!(panel.kind, PanelKind::Activity);
    assert!(panel.detail_open);
    assert_eq!(
        panel.selected().map(|row| row.value.as_str()),
        Some("model · thinking")
    );
}

#[test]
fn queue_panel_exposes_fifo_and_removes_only_pending_intent() {
    let mut ui = Ui::default();
    ui.queued.push_back("first pending request".into());
    ui.queued.push_back("second pending request".into());
    ui.open_queue_panel();

    let panel = ui.panel.as_ref().expect("queue panel");
    assert_eq!(panel.kind, PanelKind::Queue);
    assert_eq!(panel.selected_index(), Some(0));
    assert_eq!(
        panel.selected().map(|row| row.value.as_str()),
        Some("first pending request")
    );

    assert_eq!(
        ui.remove_queued(0).as_deref(),
        Some("first pending request")
    );
    ui.refresh_queue_panel();
    assert_eq!(ui.queued.len(), 1);
    assert_eq!(
        ui.panel
            .as_ref()
            .and_then(|panel| panel.selected())
            .map(|row| row.value.as_str()),
        Some("second pending request")
    );
    assert!(ui.toggle_queue_panel());
    assert!(ui.panel.is_none());
}

#[test]
fn narrow_activity_row_is_single_line_and_keeps_tag() {
    let row = PanelRow {
        key: "THK now".into(),
        value: "reasoning · scanning a very long node path".into(),
        ctx: None,
    };
    let compact = compact_activity_item(&row, 18);
    assert!(compact.starts_with("THK›"));
    assert!(str_cells(&compact) <= 18, "activity overflow: {compact}");
    assert!(!compact.contains("very long node path"));
}

#[test]
fn activity_ledger_tags_lifecycle_without_changing_current_phase() {
    let mut ui = Ui::default();
    ui.set_activity("model · thinking");
    ui.record_activity(ActivityKind::Queue, "queued · inspect tests");
    ui.record_activity(ActivityKind::Waiting, "waiting · no stream for 8s");
    ui.set_activity("approval required · user can take over");

    assert_eq!(ui.activity, "approval required · user can take over");
    assert_eq!(
        ui.activity_history
            .iter()
            .map(|entry| entry.kind)
            .collect::<Vec<_>>(),
        vec![
            ActivityKind::Reasoning,
            ActivityKind::Queue,
            ActivityKind::Waiting,
            ActivityKind::Approval,
        ]
    );
    let panel = activity_panel(&ui.activity_history);
    assert_eq!(panel.rows[0].key, "ASK now");
    assert_eq!(panel.rows[1].key, "WAIT #3");
    assert_eq!(panel.rows[2].key, "QUE #2");
}

#[test]
fn activity_classifier_exposes_investigation_verification_and_conclusion() {
    let mut ui = Ui::default();
    ui.set_activity("node · reason");
    assert_eq!(
        ui.activity_history.back().map(|entry| entry.kind),
        Some(ActivityKind::Reasoning)
    );
    ui.set_activity("node · verify");
    assert_eq!(
        ui.activity_history.back().map(|entry| entry.kind),
        Some(ActivityKind::Verification)
    );
    ui.set_activity("node · running tools");
    assert_eq!(
        ui.activity_history.back().map(|entry| entry.kind),
        Some(ActivityKind::Tool)
    );
    ui.set_activity("node · wrapping up");
    assert_eq!(
        ui.activity_history.back().map(|entry| entry.kind),
        Some(ActivityKind::Conclusion)
    );
    ui.set_activity("settling result");
    assert_eq!(
        ui.activity_history.back().map(|entry| entry.kind),
        Some(ActivityKind::Conclusion)
    );
    assert_eq!(ActivityKind::Reasoning.tag(), "THK");
    assert_eq!(ActivityKind::Verification.tag(), "CHK");
    assert_eq!(ActivityKind::Conclusion.tag(), "SUM");
}

#[test]
fn activity_classifier_keeps_chinese_lifecycle_states_observable() {
    let cases = [
        ("调查中：读取上下文", ActivityKind::Reasoning),
        ("starting task", ActivityKind::Run),
        ("等待模型响应", ActivityKind::Waiting),
        ("验证工具结果", ActivityKind::Verification),
        ("形成结论", ActivityKind::Conclusion),
        ("接管已就绪", ActivityKind::Takeover),
        ("任务完成", ActivityKind::Completed),
        ("执行失败", ActivityKind::Error),
        ("审批待确认", ActivityKind::Approval),
    ];
    let mut ui = Ui::default();
    for (text, expected) in cases {
        ui.set_activity(text);
        assert_eq!(
            ui.activity_history.back().map(|entry| entry.kind),
            Some(expected),
            "{text}"
        );
    }
}

#[test]
fn retained_activity_leaves_a_static_anchor_with_a_detail_affordance() {
    let mut ui = Ui::default();
    ui.set_activity("waiting · no stream for 8s");

    assert!(matches!(
        ui.commits.as_slice(),
        [CommitBlock::Activity {
            sequence: 1,
            kind: ActivityKind::Waiting,
            text,
        }] if text == "waiting · no stream for 8s"
    ));
    let rendered = ui
        .drain_commits()
        .into_iter()
        .map(|(text, _)| text)
        .collect::<Vec<_>>();
    assert_eq!(
        rendered,
        vec!["⟦WAIT #1⟧ waiting · no stream for 8s  [Ctrl+T activity]"]
    );
}

#[test]
fn task_start_leaves_run_anchor_in_native_scrollback() {
    let mut ui = Ui::default();
    ui.set_activity("starting task");

    assert_eq!(
        ui.activity_history.back().map(|entry| entry.kind),
        Some(ActivityKind::Run)
    );
    assert!(matches!(
        ui.commits.as_slice(),
        [CommitBlock::Activity {
            sequence: 1,
            kind: ActivityKind::Run,
            text,
        }] if text == "starting task"
    ));

    let mut terminal = Terminal::with_options(
        ratatui::backend::TestBackend::new(40, 8),
        TerminalOptions {
            viewport: Viewport::Inline(4),
        },
    )
    .expect("run boundary terminal");
    flush_commits(&mut terminal, &mut ui).expect("run boundary scrollback");
    let symbols = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(symbols.contains("RUN"), "run tag missing: {symbols}");
    assert!(
        symbols.contains("starting task"),
        "run boundary missing: {symbols}"
    );
    assert!(
        symbols.contains("Ctrl+T"),
        "run detail hint missing: {symbols}"
    );
}

#[test]
fn task_start_does_not_promote_agent_ready_system_chatter() {
    let mut ui = Ui::default();
    ui.set_activity("agent ready");

    assert_eq!(
        ui.activity_history.back().map(|entry| entry.kind),
        Some(ActivityKind::System)
    );
    assert!(ui.commits.is_empty(), "system chatter entered scrollback");
}

#[test]
fn attention_shortcuts_explain_missing_history() {
    let cases = [
        (InputAction::ToggleDetails, "no tool details or history"),
        (
            InputAction::ToggleReasoning,
            "no reasoning output or history",
        ),
        (InputAction::ToggleAnswer, "no recoverable answer history"),
    ];
    for (action, expected) in cases {
        let mut ui = Ui::default();
        apply_attention_action(&mut ui, action);
        assert!(matches!(
            ui.commits.as_slice(),
            [CommitBlock::Text { text, .. }] if text == expected
        ));
    }

    let mut ui = Ui::default();
    apply_attention_action(&mut ui, InputAction::ToggleActivity);
    assert!(matches!(
        ui.panel.as_ref().map(|panel| panel.kind),
        Some(PanelKind::Activity)
    ));
    assert!(
        ui.commits.is_empty(),
        "activity panel emitted an empty-state note"
    );
}

#[test]
fn plan_snapshot_uses_a_bounded_reasoning_activity_anchor() {
    let mut ui = Ui::default();
    ui.record_plan("[✓] inspect context\n[~] verify output\n[ ] publish result");

    assert_eq!(
        ui.activity_history.back().map(|entry| entry.kind),
        Some(ActivityKind::Plan)
    );
    let panel = activity_panel(&ui.activity_history);
    assert_eq!(panel.rows[0].key, "PLAN now");
    assert!(panel.rows[0].value.contains("verify output"));

    let mut terminal = Terminal::with_options(
        ratatui::backend::TestBackend::new(48, 10),
        TerminalOptions {
            viewport: Viewport::Inline(5),
        },
    )
    .expect("plan terminal");
    flush_commits(&mut terminal, &mut ui).expect("plan scrollback");
    let symbols = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(symbols.contains("PLAN"), "plan tag missing: {symbols}");
    assert!(
        symbols.contains("Ctrl+T"),
        "plan detail affordance missing: {symbols}"
    );
    assert!(
        symbols.contains("verify output"),
        "plan body missing: {symbols}"
    );
    assert!(!symbols.contains('\x1b'));
}

#[test]
fn retained_activity_anchor_wraps_in_native_scrollback() {
    let mut ui = Ui::default();
    ui.set_activity("waiting · no stream for 8s");
    let mut terminal = Terminal::with_options(
        ratatui::backend::TestBackend::new(32, 8),
        TerminalOptions {
            viewport: Viewport::Inline(4),
        },
    )
    .expect("activity terminal");

    flush_commits(&mut terminal, &mut ui).expect("activity scrollback");
    let symbols = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(symbols.contains("WAIT"), "activity tag missing: {symbols}");
    assert!(
        symbols.contains("Ctrl+T"),
        "activity detail affordance missing: {symbols}"
    );
    assert!(!symbols.contains('\x1b'));
}

#[test]
fn verification_activity_leaves_a_static_anchor_with_a_detail_affordance() {
    let mut ui = Ui::default();
    ui.set_activity("verify output · checking tool result");
    let mut terminal = Terminal::with_options(
        ratatui::backend::TestBackend::new(40, 8),
        TerminalOptions {
            viewport: Viewport::Inline(4),
        },
    )
    .expect("verification terminal");

    flush_commits(&mut terminal, &mut ui).expect("verification scrollback");
    let symbols = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        symbols.contains("CHK"),
        "verification tag missing: {symbols}"
    );
    assert!(
        symbols.contains("Ctrl+T"),
        "verification detail affordance missing: {symbols}"
    );
    let body = symbols
        .replace('│', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        body.contains("checking tool result"),
        "verification body missing: {symbols}"
    );
    assert!(!symbols.contains('\x1b'));
}

#[test]
fn static_activity_anchor_emphasizes_semantic_tag() {
    let mut ui = Ui::default();
    ui.set_activity("waiting · no stream for 8s");
    let mut terminal = Terminal::with_options(
        ratatui::backend::TestBackend::new(64, 8),
        TerminalOptions {
            viewport: Viewport::Inline(4),
        },
    )
    .expect("activity style terminal");

    flush_commits(&mut terminal, &mut ui).expect("activity style scrollback");
    let tag = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .find(|cell| cell.symbol() == "W")
        .expect("WAIT tag");
    assert_eq!(tag.fg, role_color(Role::Warn));
    assert!(tag.modifier.contains(Modifier::BOLD));
}

#[test]
fn wide_busy_chrome_surfaces_recent_activity_breadcrumb() {
    let mut ui = Ui {
        busy: true,
        activity: "tool · read_file".into(),
        ..Ui::default()
    };
    ui.activity_started = Some(std::time::Instant::now());
    ui.record_activity(ActivityKind::System, "agent ready");
    ui.record_activity(ActivityKind::Reasoning, "model · thinking");
    ui.record_activity(ActivityKind::Tool, "tool · read_file");
    let line = top_chrome(
        &ui,
        &Vitals {
            step: 2,
            elapsed_s: 4,
            task_tokens: 12,
            rate: 3,
            ctx_used: 20,
            queued: 1,
        },
        96,
    );
    let text = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(text.contains("+"), "activity age missing: {text}");
    assert!(
        text.contains("⟦SYS›THK›TLS⟧"),
        "activity trail missing: {text}"
    );
}

#[test]
fn takeover_records_interrupting_boundary_before_abort() {
    let mut ui = Ui::default();
    mark_takeover_requested(&mut ui);
    assert_eq!(
        ui.activity_history.back().map(|entry| entry.kind),
        Some(ActivityKind::Takeover)
    );
    assert_eq!(
        ui.activity_history.back().map(|entry| entry.text.as_str()),
        Some("interrupting · cancelling current turn")
    );
}

#[test]
fn idle_chrome_keeps_takeover_outcome_visible() {
    let mut ui = Ui::default();
    ui.set_activity("takeover ready");
    let text = top_chrome(
        &ui,
        &Vitals {
            step: 0,
            elapsed_s: 0,
            task_tokens: 0,
            rate: 0,
            ctx_used: 0,
            queued: 2,
        },
        40,
    )
    .spans
    .iter()
    .map(|span| span.content.as_ref())
    .collect::<String>();
    assert!(text.contains("takeover"), "takeover outcome hidden: {text}");
}

#[test]
fn narrow_idle_chrome_keeps_activity_kind_and_full_takeover_outcome() {
    let mut ui = Ui::default();
    ui.set_activity("takeover ready");
    for width in [32, 40] {
        let text = top_chrome(
            &ui,
            &Vitals {
                step: 0,
                elapsed_s: 0,
                task_tokens: 0,
                rate: 0,
                ctx_used: 0,
                queued: 0,
            },
            width,
        )
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
        assert!(
            text.contains("TAKE"),
            "activity kind hidden at {width}: {text}"
        );
        assert!(
            text.contains("takeover ready"),
            "takeover outcome clipped at {width}: {text}"
        );
        assert!(str_cells(&text) <= width as usize);
    }
}

#[test]
fn completed_idle_surface_exposes_answer_recovery_at_narrow_width() {
    let mut ui = Ui::default();
    ui.note_markdown("final answer body remains in the Answer archive");
    ui.record_activity(ActivityKind::Conclusion, "settling result");
    ui.record_activity(ActivityKind::Completed, "completed");

    let lines = live_empty_state_for_test(&ui, 32, 6);
    let rendered = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    let text = rendered.join("\n");

    assert!(text.contains("DONE"), "completion summary missing: {text}");
    assert!(
        text.contains("answer archived"),
        "answer state missing: {text}"
    );
    assert!(text.contains("ANS"), "answer channel missing: {text}");
    assert!(
        text.contains("SUM") && text.contains("settling result"),
        "conclusion summary missing: {text}"
    );
    assert!(text.contains("^A"), "answer recovery hint missing: {text}");
    assert!(
        text.contains("^R"),
        "reasoning recovery hint missing: {text}"
    );
    assert!(
        rendered.iter().all(|line| str_cells(line) <= 32),
        "summary overflowed narrow frame: {rendered:?}"
    );
    assert!(
        text.contains("ANS · final answer"),
        "bounded answer excerpt missing: {text}"
    );
}

#[test]
fn completed_idle_surface_exposes_reasoning_excerpt_when_room_exists() {
    let mut ui = Ui::default();
    ui.push_chunk(provider::StreamChunk::Reasoning(
        "plan checks the held viewport before answering".into(),
    ));
    ui.commit_live_reasoning(2, 3);
    ui.note_markdown("final answer remains visible");
    ui.record_activity(ActivityKind::Conclusion, "settling result");
    ui.record_activity(ActivityKind::Completed, "completed");

    let text = live_empty_state_for_test(&ui, 32, 6)
        .iter()
        .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
        .collect::<String>();

    assert!(
        text.contains("THK · plan checks"),
        "bounded reasoning excerpt missing: {text}"
    );
    assert!(
        text.contains("ANS · final answer"),
        "answer excerpt missing: {text}"
    );
    assert!(
        text.contains("^R"),
        "reasoning recovery hint missing: {text}"
    );
    assert!(
        live_empty_state_for_test(&ui, 32, 6).iter().all(|line| {
            line.spans
                .iter()
                .map(|span| str_cells(span.content.as_ref()))
                .sum::<usize>()
                <= 32
        }),
        "summary overflowed with reasoning excerpt"
    );

    let narrow = live_empty_state_for_test(&ui, 24, 6)
        .iter()
        .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
        .collect::<String>();
    assert!(
        !narrow.contains("THK ·"),
        "reasoning excerpt should yield to controls: {narrow}"
    );
    assert!(
        narrow.contains("^R"),
        "narrow reasoning control missing: {narrow}"
    );
}

#[test]
fn completed_idle_surface_keeps_semantic_roles_for_answer_reasoning_and_conclusion() {
    let mut ui = Ui::default();
    ui.push_chunk(provider::StreamChunk::Reasoning("think clearly".into()));
    ui.commit_live_reasoning(1, 1);
    ui.note_markdown("answer clearly");
    ui.record_activity(ActivityKind::Conclusion, "result ready");
    ui.record_activity(ActivityKind::Completed, "completed");

    let lines = live_empty_state_for_test(&ui, 40, 6);
    let tag = |prefix: &str| {
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content.as_ref() == prefix)
            .unwrap_or_else(|| panic!("missing semantic idle tag: {prefix}"))
    };
    let answer = tag("ANS · ");
    assert_eq!(answer.style.fg, Some(role_color(Role::Primary)));
    assert!(answer.style.add_modifier.contains(Modifier::BOLD));
    let thinking = tag("THK · ");
    assert_eq!(thinking.style.fg, Some(role_color(Role::Reasoning)));
    assert!(thinking.style.add_modifier.contains(Modifier::BOLD));
    let conclusion = tag("SUM · ");
    assert_eq!(conclusion.style.fg, Some(role_color(Role::Success)));
    assert!(conclusion.style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn live_surface_prefers_current_answer_over_stale_conclusion_activity() {
    let mut ui = Ui {
        busy: true,
        ..Ui::default()
    };
    ui.record_activity(ActivityKind::Conclusion, "settling result");
    ui.push_chunk(provider::StreamChunk::Answer("answer is streaming".into()));

    let title = live_surface_title(&ui, 64);
    assert!(
        title.contains("LIVE · ANS"),
        "current answer hidden: {title}"
    );
    assert!(
        !title.contains("LIVE · SUM"),
        "stale conclusion won: {title}"
    );
}

#[test]
fn live_surface_keeps_lifecycle_badge_during_answer_inspection() {
    let mut ui = Ui {
        busy: true,
        phase: "answering".into(),
        ..Ui::default()
    };
    ui.set_activity("verify · deterministic gate");
    ui.push_chunk(provider::StreamChunk::Answer("answer is streaming".into()));

    for width in [40, 80, 96] {
        let title = live_surface_title(&ui, width);
        assert!(title.contains("LIVE · ANS"), "width={width}: {title}");
        assert!(title.contains("· CHK"), "width={width}: {title}");
        assert!(
            str_cells(&title) <= width as usize,
            "width={width}: {title}"
        );
    }

    assert!(ui.hold_live());
    let vitals = Vitals {
        step: 4,
        elapsed_s: 2,
        task_tokens: 9,
        rate: 7,
        ctx_used: 12,
        queued: 0,
    };
    let anchor = live_phase_anchor(&ui, &vitals, 40).expect("held phase anchor");
    let text = anchor
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(text.contains("CHK"), "{text}");
    assert!(str_cells(&text) <= 40, "{text}");
}

#[test]
fn wide_completed_idle_surface_uses_bounded_result_card() {
    let mut ui = Ui::default();
    ui.push_chunk(provider::StreamChunk::Reasoning("check state".into()));
    ui.commit_live_reasoning(1, 1);
    ui.note_markdown_with_meta("final answer", 1, 1, 17);
    ui.record_activity(ActivityKind::Conclusion, "result ready");
    ui.record_activity(ActivityKind::Completed, "completed");

    let lines = live_empty_state_for_test(&ui, 64, 10);
    let nonempty = lines
        .iter()
        .filter(|line| !line.spans.is_empty())
        .collect::<Vec<_>>();
    assert!(nonempty.first().is_some_and(|line| {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
            .starts_with("╭─")
    }));
    assert!(nonempty.last().is_some_and(|line| {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
            .starts_with("╰")
    }));
    assert!(nonempty.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref() == "ANS · ")
    }));
    assert!(
        nonempty.iter().any(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
                .contains("step 1")
        }),
        "completed result card should retain Answer context metadata"
    );
    assert!(
        nonempty.iter().any(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
                .contains("THK · step 1")
        }),
        "completed result card should retain Reasoning context metadata"
    );
    assert!(lines.iter().all(|line| {
        line.spans
            .iter()
            .map(|span| str_cells(span.content.as_ref()))
            .sum::<usize>()
            <= 64
    }));
}

#[test]
fn wide_short_result_card_is_content_aware_and_centered() {
    let mut ui = Ui::default();
    ui.push_chunk(provider::StreamChunk::Reasoning("checked".into()));
    ui.commit_live_reasoning(1, 1);
    ui.note_markdown_with_meta("OK", 1, 1, 2);
    ui.record_activity(ActivityKind::Conclusion, "ready");
    ui.record_activity(ActivityKind::Completed, "completed");

    let lines = live_empty_state_for_test(&ui, 96, 10);
    let top = lines
        .iter()
        .find(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("╭─"))
        })
        .expect("result card top border");
    let top_text = top
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(
        top_text.starts_with(' '),
        "card should be centered: {top_text}"
    );
    assert!(str_cells(&top_text) < 96, "card should shrink: {top_text}");
    assert!(
        top_text.contains("DONE"),
        "completion title missing: {top_text}"
    );
    assert!(lines.iter().all(|line| {
        line.spans
            .iter()
            .map(|span| str_cells(span.content.as_ref()))
            .sum::<usize>()
            <= 96
    }));
}

#[test]
fn partial_completed_idle_surface_keeps_partial_answer_truthful() {
    let mut ui = Ui::default();
    ui.push_chunk(provider::StreamChunk::Answer("partial response".into()));
    ui.commit_live_answers("interrupted before final response", 3, 4);
    ui.record_activity(ActivityKind::Conclusion, "result interrupted");
    ui.record_activity(ActivityKind::Completed, "completed");

    let text = live_empty_state_for_test(&ui, 64, 10)
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(text.contains("partial answer retained"), "{text}");
    assert!(text.contains("PARTIAL · "), "partial label missing: {text}");
    assert!(
        text.contains("step 3") && text.contains("4 task tok"),
        "partial context metadata missing: {text}"
    );
    assert!(
        !text.contains("ANS · step 3"),
        "partial answer was presented as complete: {text}"
    );
}

#[test]
fn error_idle_surface_keeps_partial_answer_recoverable() {
    let mut ui = Ui::default();
    ui.push_chunk(provider::StreamChunk::Answer(
        "partial provider response".into(),
    ));
    ui.commit_live_answers("provider failed after streaming", 4, 9);
    ui.mark_task_outcome_with_reason(false, Some(halt_reason_display(HaltReason::Stall)));

    let text = live_empty_state_for_test(&ui, 64, 10)
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(text.contains("ERR"), "error state missing: {text}");
    assert!(
        text.contains("PARTIAL · ") && text.contains("partial provider response"),
        "error state hid retained partial Answer: {text}"
    );
    assert!(
        text.contains("step 4") && text.contains("task tok"),
        "error state hid partial Answer metadata: {text}"
    );
    assert!(
        text.contains("no verified progress"),
        "error state hid deterministic halt reason: {text}"
    );
}

#[test]
fn completed_idle_surface_keeps_whole_labels_at_extreme_widths() {
    let mut ui = Ui::default();
    ui.note_markdown("final answer body remains in the Answer archive");
    ui.record_activity(ActivityKind::Conclusion, "settling result");
    ui.record_activity(ActivityKind::Completed, "completed");

    for width in [18, 24] {
        let rendered = live_empty_state_for_test(&ui, width, 6)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let text = rendered.join("\n");

        assert!(
            text.contains("DONE"),
            "completion tag missing at {width}: {text}"
        );
        assert!(
            text.contains("SUM"),
            "conclusion tag missing at {width}: {text}"
        );
        assert!(
            text.contains("ANS"),
            "answer tag missing at {width}: {text}"
        );
        assert!(
            text.contains("^A"),
            "answer shortcut missing at {width}: {text}"
        );
        assert!(
            text.contains("^R"),
            "reasoning shortcut missing at {width}: {text}"
        );
        assert!(
            !text.contains('…'),
            "semantic labels must not be ellipsized at {width}: {text}"
        );
        assert!(
            !text.contains("final answer body"),
            "answer excerpt should yield to recovery controls at {width}: {text}"
        );
        assert!(
            rendered
                .iter()
                .all(|line| str_cells(line) <= width as usize),
            "summary overflowed at {width}: {rendered:?}"
        );
    }
}

#[test]
fn empty_live_states_keep_whole_intervention_labels_at_narrow_widths() {
    let text = |lines: &[Line<'static>]| {
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>()
    };

    for width in [18, 24, 32] {
        let mut ready = Ui::default();
        let ready_text = text(&live_empty_state_for_test(&ready, width, 5));
        assert!(ready_text.contains("READY"), "ready tag missing at {width}");
        assert!(
            !ready_text.contains('…'),
            "ready action was semantically clipped at {width}: {ready_text}"
        );

        ready.queued.push_back("/queued".to_owned());
        let queued_text = text(&live_empty_state_for_test(&ready, width, 5));
        assert!(
            queued_text.contains("QUEUE"),
            "queue tag missing at {width}"
        );
        assert!(
            queued_text.contains("Enter") || queued_text.contains("^Enter"),
            "queue intervention missing at {width}: {queued_text}"
        );
        assert!(
            !queued_text.contains('…'),
            "queue action was semantically clipped at {width}: {queued_text}"
        );

        ready.busy = true;
        ready.activity = "model · thinking".to_owned();
        let busy_text = text(&live_empty_state_for_test(&ready, width, 5));
        assert!(busy_text.contains("LIVE"), "live tag missing at {width}");
        assert!(
            busy_text.contains("^Space") || busy_text.contains("Ctrl+Space"),
            "hold intervention missing at {width}: {busy_text}"
        );
        assert!(
            !busy_text.contains('…'),
            "live action was semantically clipped at {width}: {busy_text}"
        );
        assert!(
            live_empty_state_for_test(&ready, width, 5)
                .iter()
                .all(|line| {
                    line.spans
                        .iter()
                        .map(|span| str_cells(span.content.as_ref()))
                        .sum::<usize>()
                        <= width as usize
                }),
            "empty live state overflowed at {width}"
        );
    }
}

#[test]
fn idle_conclusion_without_answer_does_not_advertise_empty_archive() {
    let mut ui = Ui::default();
    ui.record_activity(ActivityKind::Conclusion, "settling result");

    let text = live_empty_state_for_test(&ui, 32, 5)
        .iter()
        .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
        .collect::<String>();

    assert!(text.contains("SUM"), "conclusion summary missing: {text}");
    assert!(text.contains("Ctrl+T"), "activity recovery missing: {text}");
    assert!(
        !text.contains("Ctrl+A"),
        "empty answer archive advertised: {text}"
    );
}

#[test]
fn different_tool_events_do_not_merge_without_same_name_adjacency() {
    let mut ui = Ui::default();
    ui.push_tool(
        tool_preview(r#"reason#1: tool_call read_file {"path":"src/x.rs"}"#).expect("read call"),
    );
    ui.push_tool(
        tool_preview("reason#2: tool_call search {\"pattern\":\"needle\"}").expect("search call"),
    );

    let lines = ui.transcript.visible_lines(8);
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.kind == LiveLineKind::ToolSummary)
            .count(),
        2,
        "unrelated calls must stay separate"
    );
}

#[test]
fn live_output_wraps_long_lines_to_terminal_cells() {
    let rows = wrap_live_spans(vec![Span::raw("abcdefgh")], 4);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0]
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>(),
        "abcd"
    );
    assert_eq!(
        rows[1]
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>(),
        "efgh"
    );
}

#[test]
fn live_output_prefers_word_boundaries_before_hard_wrapping() {
    let text =
        "fixture reasoning: waiting without network; queue and takeover remain available [Ctrl+R history]";
    let row_text = |rows: &[Vec<Span<'static>>]| {
        rows.iter()
            .map(|row| {
                row.iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
    };
    for rendered in [
        row_text(&wrap_live_spans(vec![Span::raw(text)], 40)),
        row_text(&wrap_live_spans_tail(vec![Span::raw(text)], 40, 3)),
    ] {
        assert!(
            rendered.iter().all(|line| str_cells(line) <= 40),
            "{rendered:?}"
        );
        for word in [
            "reasoning",
            "network",
            "queue",
            "takeover",
            "available",
            "history",
        ] {
            assert!(
                rendered.iter().any(|line| line.contains(word)),
                "{word} split by live reflow: {rendered:?}"
            );
        }
    }
}

#[test]
fn committed_semantic_prefixes_survive_narrow_reflow() {
    let line_text = |line: &Line<'static>| {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    };
    let within = |line: &Line<'static>, width: usize| {
        line.spans
            .iter()
            .map(|span| str_cells(span.content.as_ref()))
            .sum::<usize>()
            <= width
    };
    let long = "abcdefghijklmnopqrstuvwxyz0123456789";

    let answer = wrap_commit_lines(answer_commit_lines(&format!("\u{1f916} {long}")), 18);
    assert!(answer.len() > 1);
    assert!(line_text(&answer[0]).starts_with("\u{256d} ANSWER "));
    assert!(answer.iter().all(|line| within(line, 18)));
    assert!(answer
        .iter()
        .skip(1)
        .all(|line| line_text(line).starts_with("\u{2502} ") && within(line, 18)));

    let reasoning = wrap_commit_lines(
        vec![Line::from(Span::styled(
            format!("\u{257a} step 1 {long}"),
            Style::default().fg(role_color(Role::Reasoning)),
        ))],
        18,
    );
    assert!(reasoning.len() > 1);
    assert!(line_text(&reasoning[0]).starts_with("\u{257a} "));
    assert!(reasoning.iter().all(|line| within(line, 18)));
    assert!(reasoning
        .iter()
        .skip(1)
        .all(|line| line_text(line).starts_with("\u{2502} ") && within(line, 18)));

    let tool = wrap_commit_lines(vec![Line::from(Span::raw(format!("\u{25c8} {long}")))], 18);
    assert!(tool.len() > 1);
    assert!(tool.iter().all(|line| within(line, 18)));
    assert!(tool
        .iter()
        .skip(1)
        .all(|line| line_text(line).starts_with("  \u{2506} ") && within(line, 18)));
}

#[test]
fn committed_reasoning_text_keeps_its_continuation_rail_after_narrow_reflow() {
    let text = "┊ THK[step 2 · t+3s · 11 task tok] the reasoning body remains visible across a narrow terminal width  [Ctrl+R history]";
    let lines = wrap_commit_lines(
        vec![Line::from(Span::styled(
            text,
            Style::default().fg(role_color(Role::Reasoning)),
        ))],
        18,
    );
    let line_text = |line: &Line<'static>| {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    };
    assert!(lines.len() > 1);
    assert!(line_text(&lines[0]).starts_with("┊ "));
    assert!(lines.iter().skip(1).all(|line| {
        line_text(line).starts_with("│ ")
            && line
                .spans
                .iter()
                .map(|span| str_cells(span.content.as_ref()))
                .sum::<usize>()
                <= 18
    }));
}

#[test]
fn zwj_emoji_stays_one_display_cluster_when_wrapped() {
    let rows = wrap_live_spans(vec![Span::raw("👩‍🔬A")], 2);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0]
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>(),
        "👩‍🔬"
    );
    assert_eq!(
        rows[1]
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>(),
        "A"
    );
    assert!(rows.iter().all(|row| {
        row.iter()
            .map(|span| str_cells(span.content.as_ref()))
            .sum::<usize>()
            <= 2
    }));
}

#[test]
fn zwj_emoji_tail_and_input_share_cluster_width() {
    assert_eq!(tail_display_cells("prefix👩‍🔬", 3, 1), "…👩‍🔬");
    let (lines, row, col) = wrap_input("👩‍🔬A", 4, 2);
    assert_eq!(lines, vec!["👩‍🔬", "A"]);
    assert_eq!((row, col), (1, 1));
}

#[test]
fn cjk_reasoning_and_answer_reflow_keep_semantic_rails_within_cells() {
    let mut ui = Ui {
        busy: true,
        ..Ui::default()
    };
    ui.push_chunk(provider::StreamChunk::Reasoning(
        "调查阶段正在读取上下文并核对工具结果。\n思考尾部".into(),
    ));
    ui.push_chunk(provider::StreamChunk::Answer(
        "回答开头保持可读并随宽度重流。\n最终回答尾部".into(),
    ));
    let vitals = Vitals {
        step: 3,
        elapsed_s: 5,
        task_tokens: 24,
        rate: 6,
        ctx_used: 12,
        queued: 0,
    };

    for width in [32, 40] {
        let mut cache = LiveOutputCache::default();
        let lines = cache.lines(&ui.transcript, width, 8, true, &vitals);
        let text = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(
            text.contains("最终回答尾部"),
            "answer tail lost at {width}: {text}"
        );
        assert!(
            text.contains("╰") || text.contains("┃"),
            "answer rail lost at {width}: {text}"
        );
        assert!(lines.iter().all(|line| {
            line.spans
                .iter()
                .map(|span| str_cells(span.content.as_ref()))
                .sum::<usize>()
                <= width as usize
        }));
    }
}

#[test]
fn live_rail_uses_semantic_kind_and_focus_only() {
    assert_eq!(
        live_rail(LiveLineKind::Answer, false, None),
        Some(("┃", Role::Primary))
    );
    assert_eq!(
        live_rail(LiveLineKind::Reasoning, false, None),
        Some(("┌", Role::Reasoning))
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
fn live_phase_markers_adapt_without_adding_rows() {
    let reasoning = Some("💭 [step 2 · t+4s · 12 task tok]");
    let wide = live_phase_marker(LiveLineKind::Reasoning, reasoning, None, 96)
        .expect("wide reasoning marker");
    let medium = live_phase_marker(LiveLineKind::Reasoning, reasoning, None, 64)
        .expect("medium reasoning marker");
    assert!(wide.starts_with(" THINK "), "{wide}");
    assert!(medium.starts_with(" THK "), "{medium}");
    assert_eq!(wide.lines().count(), 1);
    assert_eq!(medium.lines().count(), 1);

    assert_eq!(
        live_phase_marker(LiveLineKind::Reasoning, reasoning, None, 40).as_deref(),
        reasoning
    );
    assert_eq!(
        live_phase_marker(
            LiveLineKind::ToolSummary,
            None,
            Some(LiveLineKind::Answer),
            96
        )
        .as_deref(),
        Some(" TOOL ")
    );
    assert!(live_phase_marker(
        LiveLineKind::ToolSummary,
        None,
        Some(LiveLineKind::ToolDetail),
        96
    )
    .is_none());
    assert!(str_cells(&wide) <= 96);
    assert!(str_cells(&medium) <= 64);
}

#[test]
fn live_rows_surface_observed_phase_labels_in_one_projection() {
    let mut ui = Ui {
        busy: true,
        ..Ui::default()
    };
    ui.push_chunk(provider::StreamChunk::Reasoning("plan".into()));
    ui.push_tool(ToolBlock::from_lines(vec![("tool summary".into(), Color::Cyan)]).expect("tool"));
    ui.push_chunk(provider::StreamChunk::Answer("answer".into()));
    let vitals = Vitals {
        step: 2,
        elapsed_s: 4,
        task_tokens: 12,
        rate: 3,
        ctx_used: 8,
        queued: 0,
    };
    let mut cache = LiveOutputCache::default();
    let lines = cache.lines(&ui.transcript, 96, 12, true, &vitals);
    let text = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(text.contains("THINK"), "{text}");
    assert!(text.contains("TOOL"), "{text}");
    assert!(text.contains("ANSWER"), "{text}");
    assert!(lines.iter().all(|line| {
        line.spans
            .iter()
            .map(|span| str_cells(span.content.as_ref()))
            .sum::<usize>()
            <= 96
    }));
}

#[test]
fn stream_channel_badges_keep_actual_output_semantics() {
    assert_eq!(
        stream_channel_badge(LiveChannel::Reasoning),
        ("[THINK]", Role::Reasoning)
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
fn top_chrome_keeps_brand_and_channel_on_flat_surface() {
    let mut ui = Ui {
        busy: true,
        phase: "answering".into(),
        activity: "model · thinking".into(),
        ..Ui::default()
    };
    ui.push_chunk(provider::StreamChunk::Answer("answer".into()));
    let vitals = Vitals {
        step: 1,
        elapsed_s: 1,
        task_tokens: 2,
        rate: 2,
        ctx_used: 0,
        queued: 0,
    };
    let line = top_chrome(&ui, &vitals, 96);
    let span = |needle: &str| {
        line.spans
            .iter()
            .find(|span| span.content.contains(needle))
            .unwrap_or_else(|| panic!("missing chrome span: {needle}"))
    };
    assert_eq!(span("RidgeCode").style.bg, None);
    assert_eq!(span("[ANSWER]").style.bg, None);
    assert_eq!(
        line.spans.last().and_then(|span| span.style.fg),
        Some(role_color(Role::Primary))
    );
    assert!(line
        .spans
        .iter()
        .any(|span| span.content.contains("model · thinking")));
}

#[test]
fn top_chrome_keeps_token_telemetry_in_the_bottom_status_contract() {
    let mut ui = Ui {
        busy: true,
        phase: "answering".into(),
        activity: "model · answering".into(),
        ..Ui::default()
    };
    ui.push_chunk(provider::StreamChunk::Answer("answer".into()));
    let text = top_chrome(
        &ui,
        &Vitals {
            step: 2,
            elapsed_s: 4,
            task_tokens: 40,
            rate: 12,
            ctx_used: 20,
            queued: 1,
        },
        96,
    )
    .spans
    .iter()
    .map(|span| span.content.as_ref())
    .collect::<String>();
    assert!(text.contains("t+4s"), "{text}");
    assert!(text.contains("12/s"), "{text}");
    assert!(!text.contains(" in "), "{text}");
    assert!(!text.contains(" out "), "{text}");
    assert!(!text.contains("effort"), "{text}");
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
    ui.push_chunk(provider::StreamChunk::Answer(
        "answer0\nanswer1\nanswer2".into(),
    ));
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
    assert!(ui.scroll_live(1));
    let inspected = top_chrome(&ui, &vitals, 96)
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(inspected.contains("HOLD"), "{inspected}");
}

#[test]
fn top_chrome_surfaces_reasoning_visibility_alongside_tools() {
    let mut ui = Ui {
        busy: true,
        phase: "answering".into(),
        ..Ui::default()
    };
    ui.push_chunk(provider::StreamChunk::Reasoning("plan".into()));
    ui.push_tool(ToolBlock::from_lines(vec![("search".into(), Color::Cyan)]).expect("tool"));
    ui.push_chunk(provider::StreamChunk::Answer("answer".into()));
    let vitals = Vitals {
        step: 2,
        elapsed_s: 3,
        task_tokens: 40,
        rate: 12,
        ctx_used: 0,
        queued: 0,
    };

    for width in [120, 96, 80, 64, 48] {
        let text = top_chrome(&ui, &vitals, width)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(str_cells(&text) <= width as usize, "width={width}: {text}");
        if width >= 48 {
            assert!(text.contains("THINK"), "width={width}: {text}");
        }
    }
}

#[test]
fn progress_diagnostic_surfaces_deterministic_loop_counters() {
    assert_eq!(fmt_progress_diagnostic(0, 0, 0), None);
    assert_eq!(
        fmt_progress_diagnostic(2, 1, 5).as_deref(),
        Some("inspect 5/12 · same 2/3 · errors 1/5")
    );

    let ui = Ui {
        busy: true,
        phase: "reasoning".into(),
        stall: 2,
        err_streak: 1,
        explore_streak: 5,
        ..Ui::default()
    };
    let vitals = Vitals {
        step: 4,
        elapsed_s: 8,
        task_tokens: 40,
        rate: 12,
        ctx_used: 0,
        queued: 0,
    };
    for width in [18, 24, 32, 40, 48, 64, 96, 120] {
        let text = top_chrome(&ui, &vitals, width)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(str_cells(&text) <= width as usize, "width={width}: {text}");
        if width == 120 {
            assert!(text.contains("inspect 5/12"), "{text}");
            assert!(text.contains("same 2/3"), "{text}");
            assert!(text.contains("errors 1/5"), "{text}");
        }
    }
}

#[test]
fn live_phase_anchor_keeps_activity_inside_held_viewport() {
    let mut ui = Ui {
        busy: true,
        phase: "reasoning".into(),
        activity: "reasoning · inspect prior tool".into(),
        ..Ui::default()
    };
    ui.push_chunk(provider::StreamChunk::Answer("answer tail".into()));
    let vitals = Vitals {
        step: 7,
        elapsed_s: 0,
        task_tokens: 0,
        rate: 0,
        ctx_used: 0,
        queued: 0,
    };

    assert!(ui.scroll_live(1));
    let line = live_phase_anchor(&ui, &vitals, 40).expect("held viewport anchor");
    let text = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(text.contains("HOLD"));
    assert!(text.contains("inspect prior tool"));
    assert!(str_cells(&text) <= 40);
    assert!(ui.follow_live());
    assert!(live_phase_anchor(&ui, &vitals, 40).is_none());
}

#[test]
fn held_live_anchor_keeps_waiting_target_visible() {
    let mut ui = Ui {
        busy: true,
        waiting: true,
        phase: "reasoning".into(),
        ..Ui::default()
    };
    ui.push_chunk(provider::StreamChunk::Answer("waiting tail".into()));
    let vitals = Vitals {
        step: 3,
        elapsed_s: 1,
        task_tokens: 2,
        rate: 0,
        ctx_used: 4,
        queued: 0,
    };

    assert!(ui.hold_live());
    let mut text = |pending_call: bool| {
        ui.pending_call = pending_call.then(|| provider::ToolCall {
            id: "wait-1".into(),
            name: "search".into(),
            arguments: serde_json::json!({}),
        });
        live_phase_anchor(&ui, &vitals, 40)
            .expect("held waiting anchor")
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    };

    let model = text(false);
    assert!(model.contains("waiting"), "{model}");
    assert!(model.contains("model"), "{model}");
    assert!(str_cells(&model) <= 40, "{model}");

    let tool = text(true);
    assert!(tool.contains("waiting"), "{tool}");
    assert!(tool.contains("tool"), "{tool}");
    assert!(str_cells(&tool) <= 40, "{tool}");
}

#[test]
fn held_wait_anchor_prioritizes_waiting_target_over_optional_breadcrumbs() {
    let mut ui = Ui {
        busy: true,
        waiting: true,
        phase: "reasoning".into(),
        ..Ui::default()
    };
    ui.set_activity("node · verify");
    ui.push_chunk(provider::StreamChunk::Reasoning("plan".into()));
    ui.push_tool(ToolBlock::from_lines(vec![("search".into(), Color::Cyan)]).expect("tool"));
    ui.push_chunk(provider::StreamChunk::Answer("answer".into()));
    let vitals = Vitals {
        step: 3,
        elapsed_s: 1,
        task_tokens: 2,
        rate: 0,
        ctx_used: 4,
        queued: 0,
    };
    assert!(ui.hold_live());

    for (target, pending_call) in [("model", false), ("tool", true)] {
        ui.pending_call = pending_call.then(|| provider::ToolCall {
            id: "wait-1".into(),
            name: "search".into(),
            arguments: serde_json::json!({}),
        });
        for width in [18, 24, 32, 40] {
            let text = live_phase_anchor(&ui, &vitals, width)
                .expect("held waiting anchor")
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>();
            assert!(
                text.contains("waiting"),
                "target={target}, width={width}: {text}"
            );
            assert!(
                text.contains(target),
                "target={target}, width={width}: {text}"
            );
            assert!(str_cells(&text) <= width as usize, "width={width}: {text}");
        }
    }
}

#[test]
fn live_phase_anchor_adapts_trace_to_terminal_width() {
    let mut ui = Ui {
        busy: true,
        phase: "answering".into(),
        ..Ui::default()
    };
    ui.push_chunk(provider::StreamChunk::Reasoning("plan".into()));
    ui.push_tool(ToolBlock::from_lines(vec![("search".into(), Color::Cyan)]).expect("tool"));
    ui.push_chunk(provider::StreamChunk::Answer("answer".into()));
    let vitals = Vitals {
        step: 3,
        elapsed_s: 1,
        task_tokens: 2,
        rate: 3,
        ctx_used: 4,
        queued: 0,
    };
    assert!(ui.hold_live());

    let text = |width| {
        live_phase_anchor(&ui, &vitals, width)
            .expect("phase anchor")
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    };
    let wide = text(96);
    assert!(wide.contains("THK›TLS›ANS"), "{wide}");
    assert!(str_cells(&wide) <= 96);
    let narrow = text(40);
    assert!(narrow.contains("T›L›A"), "{narrow}");
    assert!(!narrow.contains("THK›TLS›ANS"), "{narrow}");
    assert!(str_cells(&narrow) <= 40);
}

#[test]
fn top_chrome_surfaces_waiting_target_after_event_silence() {
    let mut ui = Ui {
        busy: true,
        waiting: true,
        phase: "reasoning".into(),
        ..Ui::default()
    };
    let vitals = Vitals {
        step: 2,
        elapsed_s: 12,
        task_tokens: 0,
        rate: 0,
        ctx_used: 0,
        queued: 0,
    };
    let model = top_chrome(&ui, &vitals, 96)
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(model.contains("waiting"), "{model}");
    assert!(model.contains("model"), "{model}");

    ui.pending_call = Some(provider::ToolCall {
        id: "wait-1".into(),
        name: "search".into(),
        arguments: serde_json::json!({}),
    });
    let tool = top_chrome(&ui, &vitals, 96)
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(tool.contains("waiting"), "{tool}");
    assert!(tool.contains("tool"), "{tool}");
}

#[test]
fn narrow_busy_chrome_keeps_waiting_target_without_live_channel() {
    let mut ui = Ui {
        busy: true,
        waiting: true,
        phase: "reasoning".into(),
        ..Ui::default()
    };
    let vitals = Vitals {
        step: 2,
        elapsed_s: 12,
        task_tokens: 0,
        rate: 0,
        ctx_used: 0,
        queued: 0,
    };

    for (target, pending_call) in [("model", false), ("tool", true)] {
        ui.pending_call = pending_call.then(|| provider::ToolCall {
            id: "wait-1".into(),
            name: "search".into(),
            arguments: serde_json::json!({}),
        });
        for width in [18, 24, 32, 40] {
            let text = top_chrome(&ui, &vitals, width)
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>();
            assert!(str_cells(&text) <= width as usize, "width={width}: {text}");
            assert!(text.contains("waiting"), "width={width}: {text}");
            assert!(
                text.contains(target),
                "target={target}, width={width}: {text}"
            );
        }
    }
}

#[test]
fn medium_busy_chrome_exposes_semantic_activity_chip_without_live_channel() {
    let mut ui = Ui {
        busy: true,
        phase: "verify".into(),
        ..Ui::default()
    };
    ui.set_activity("node · verify");
    for width in [48, 56, 63] {
        let text = top_chrome(
            &ui,
            &Vitals {
                step: 2,
                elapsed_s: 4,
                task_tokens: 12,
                rate: 3,
                ctx_used: 4,
                queued: 0,
            },
            width,
        )
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
        assert!(
            text.contains("CHK"),
            "verification chip missing at {width}: {text}"
        );
        assert!(str_cells(&text) <= width as usize, "width={width}: {text}");
    }

    ui.set_activity("settling result");
    let text = top_chrome(
        &ui,
        &Vitals {
            step: 2,
            elapsed_s: 4,
            task_tokens: 12,
            rate: 3,
            ctx_used: 4,
            queued: 0,
        },
        56,
    )
    .spans
    .iter()
    .map(|span| span.content.as_ref())
    .collect::<String>();
    assert!(text.contains("SUM"), "conclusion chip missing: {text}");
}

#[test]
fn medium_busy_chrome_keeps_hold_priority_over_activity_chip() {
    let mut ui = Ui {
        busy: true,
        ..Ui::default()
    };
    ui.set_activity("node · verify");
    ui.push_chunk(provider::StreamChunk::Answer("answer".into()));
    assert!(ui.hold_live());
    let text = top_chrome(
        &ui,
        &Vitals {
            step: 2,
            elapsed_s: 4,
            task_tokens: 12,
            rate: 3,
            ctx_used: 4,
            queued: 0,
        },
        56,
    )
    .spans
    .iter()
    .map(|span| span.content.as_ref())
    .collect::<String>();
    assert!(text.contains("HOLD"), "hold priority lost: {text}");
    assert!(str_cells(&text) <= 56);
}

#[test]
fn narrow_busy_chrome_keeps_waiting_phase_visible() {
    let mut ui = Ui {
        busy: true,
        waiting: true,
        activity: "waiting".into(),
        ..Ui::default()
    };
    ui.push_chunk(provider::StreamChunk::Answer("answer".into()));
    let vitals = Vitals {
        step: 3,
        elapsed_s: 8,
        task_tokens: 12,
        rate: 1,
        ctx_used: 20,
        queued: 1,
    };
    for width in [18, 24, 32, 40, 80] {
        let line = top_chrome(&ui, &vitals, width);
        let text = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(str_cells(&text) <= width as usize, "width={width}: {text}");
        assert!(
            text.contains("waiting"),
            "waiting phase must survive narrow chrome at width={width}: {text}"
        );
        if width < 48 {
            assert!(
                text.contains("⏭1"),
                "front queue marker must survive narrow chrome at width={width}: {text}"
            );
        }
    }
}

#[test]
fn narrow_busy_chrome_keeps_hold_beacon_visible() {
    let mut ui = Ui {
        busy: true,
        phase: "answering".into(),
        activity: "answering · inspect prior output".into(),
        ..Ui::default()
    };
    ui.push_chunk(provider::StreamChunk::Answer("answer".into()));
    assert!(ui.hold_live());
    let vitals = Vitals {
        step: 7,
        elapsed_s: 8,
        task_tokens: 12,
        rate: 1,
        ctx_used: 20,
        queued: 1,
    };
    for width in [18, 24, 32, 40, 47] {
        let line = top_chrome(&ui, &vitals, width);
        let text = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(str_cells(&text) <= width as usize, "width={width}: {text}");
        assert!(
            text.contains("HOLD"),
            "narrow busy chrome must show the held live viewport at width={width}: {text}"
        );
        let (input, _) = input_chrome(InputChromeArgs {
            busy: true,
            queued: 1,
            width,
            reasoning_expanded: false,
            has_reasoning: false,
            has_reasoning_history: false,
            has_live_answer: false,
            has_answer_history: false,
            has_live_history: false,
            has_tools: false,
            has_history: false,
            has_scrollable_tool_details: false,
            has_live_output: true,
            live_inspecting: true,
        });
        assert!(
            input.contains("^Space"),
            "narrow input chrome must expose follow while held at width={width}: {input}"
        );
    }
}

#[test]
fn narrow_live_matrix_keeps_state_and_takeover_signals_observable() {
    let meta = ReplMeta {
        tools: Vec::new(),
        provider: "openai".into(),
        provider_label: "openai".into(),
        model: "gpt-5".into(),
        base_url: String::new(),
        status_bar: "{provider} · {model} · {tokens}".into(),
        ctx_window: 200_000,
    };
    let vitals = Vitals {
        step: 3,
        elapsed_s: 8,
        task_tokens: 12,
        rate: 1,
        ctx_used: 20,
        queued: 1,
    };
    let rendered = |ui: &Ui, width: u16, height: u16, approval: Option<&ApprovalRequest>| {
        let mut terminal =
            Terminal::new(ratatui::backend::TestBackend::new(width, height)).expect("matrix");
        terminal
            .draw(|frame| draw(frame, ui, &meta, 42, &vitals, approval))
            .expect("matrix draw");
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(width as usize)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    };

    for &(width, height) in &[(18, 7), (24, 10), (32, 14), (40, 7), (80, 14)] {
        let mut waiting = Ui {
            busy: true,
            waiting: true,
            activity: "waiting".into(),
            ..Ui::default()
        };
        waiting.push_chunk(provider::StreamChunk::Answer("answer".into()));
        let waiting_text = rendered(&waiting, width, height, None);
        assert!(
            waiting_text.contains("waiting"),
            "waiting hidden at {width}x{height}: {waiting_text}"
        );

        let mut tool = Ui {
            busy: true,
            activity: "tool · search".into(),
            ..Ui::default()
        };
        tool.push_tool(
            ToolBlock::from_lines(vec![
                ("tool: search".into(), Color::Cyan),
                ("  result detail".into(), Color::Gray),
            ])
            .expect("tool block"),
        );
        let tool_text = rendered(&tool, width, height, None);
        assert!(
            tool_text.contains("search"),
            "tool activity hidden at {width}x{height}: {tool_text}"
        );

        let mut queued = Ui {
            busy: true,
            activity: "model · thinking".into(),
            ..Ui::default()
        };
        queued.queued.push_back("priority intent".into());
        queued.input.insert_str("draft");
        let queued_text = rendered(&queued, width, height, None);
        assert!(
            queued_text.contains("priority") || queued_text.contains("Q:"),
            "queue affordance hidden at {width}x{height}: {queued_text}"
        );
    }

    let (_reply, _receiver) = std::sync::mpsc::sync_channel(1);
    let approval = ApprovalRequest {
        action: "run_shell".into(),
        detail: "+ cargo test\n- cargo check".into(),
        reply: _reply,
    };
    for &(width, height) in &[(18, 7), (32, 10), (80, 14)] {
        let text = rendered(&Ui::default(), width, height, Some(&approval));
        assert!(
            text.contains("Permission") || text.contains("Allow"),
            "approval affordance hidden at {width}x{height}: {text}"
        );
    }
}

#[test]
fn live_empty_state_keeps_takeover_affordance_across_widths() {
    let ui = Ui {
        busy: true,
        activity: "planning next step".into(),
        ..Ui::default()
    };

    for (width, rows) in [(6, 4), (11, 4), (18, 6), (32, 6), (48, 6), (80, 6)] {
        let lines = live_empty_state_for_test(&ui, width, rows);
        let text = lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect::<String>();
        assert!(
            text.contains("Esc"),
            "empty LIVE state must expose takeover at {width} columns: {text}"
        );
        assert!(
            lines.iter().all(|line| {
                line.spans
                    .iter()
                    .map(|span| str_cells(span.content.as_ref()))
                    .sum::<usize>()
                    <= width as usize
            }),
            "empty LIVE state overflow at {width} columns: {text}"
        );
    }
}

#[test]
fn reasoning_answer_transition_rail_is_bounded() {
    assert_eq!(
        live_rail(
            LiveLineKind::Reasoning,
            false,
            Some(LiveLineKind::ToolSummary)
        ),
        Some(("┌", Role::Reasoning))
    );
    assert_eq!(
        live_rail(
            LiveLineKind::Reasoning,
            false,
            Some(LiveLineKind::Reasoning)
        ),
        Some(("│", Role::Reasoning))
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
        Some(Role::Reasoning)
    );
    assert_eq!(
        active_reasoning_tail_role(LiveLineKind::Reasoning, false, true),
        Some(Role::Reasoning)
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
    assert_eq!(format_event_plain("(final)"), "🤖 [empty answer]");
    assert_eq!(format_event_plain("reason#2: (final)"), "🤖 [empty answer]");
    assert!(is_final_event("(final)"));
    assert!(is_final_event("reason#2: (final)"));
    assert!(!is_final_event("(final)not-a-marker"));
    assert!(is_final_event("reason#2: (final) hello"));
    assert!(!is_final_event("reason#2: tool_call search {}"));
}

#[test]
fn unfinished_answer_is_retained_for_error_and_non_final_stops() {
    let error: Result<AgentState, String> = Err("provider stopped".into());
    assert_eq!(
        unfinished_answer_reason(&error),
        Some("run ended before final response")
    );

    let stopped: Result<AgentState, String> = Ok(AgentState::default());
    assert_eq!(
        unfinished_answer_reason(&stopped),
        Some("run stopped before final response")
    );

    let mut completed = AgentState::default();
    completed.messages.push("(final) done".into());
    let completed = Ok(completed);
    assert_eq!(unfinished_answer_reason(&completed), None);
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
    assert!(r[0].0.contains("Read src/x.rs"), "{}", r[0].0);
    // 读回执:摘要行只回执字数,内容进入可折叠的有界详情。
    let a = summarize_event("act: read_file -> 一二三四五");
    assert!(a[0].0.contains("Read complete"), "{}", a[0].0);
    assert!(!a[0].0.contains("一二三"), "内容不应回显");
    assert!(a.iter().any(|(line, _)| line.contains("一二三")));
    // 改:git-diff 式 ± 行,红减绿增。
    let e = summarize_event(
        r#"reason#2: tool_call edit_file {"path":"a.rs","old_string":"let n=1;","new_string":"let n=2;"}"#,
    );
    assert!(e[0].0.contains("Edit a.rs"), "{}", e[0].0);
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
    assert!(w[0].0.contains("Write b.rs"), "{}", w[0].0);
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
    assert!(batch[0].0.contains("4 files / 4 edits"), "{batch:?}");
    assert!(batch[0].0.contains("src/a.rs"), "{batch:?}");
    assert!(batch[0].0.contains("src/c.rs"), "{batch:?}");
    assert!(batch[0].0.contains("… +1 more"), "摘要路径须有界:{batch:?}");
    assert!(!batch[0].0.contains("src/d.rs"), "摘要不应溢出:{batch:?}");
    assert!(batch
        .iter()
        .any(|(line, color)| { line.starts_with("  - ") && *color == role_color(Role::Error) }));
    assert!(batch
        .iter()
        .any(|(line, color)| { line.starts_with("  + ") && *color == role_color(Role::Success) }));
}

#[test]
fn empty_tool_observation_is_explicit_across_projections() {
    let message = "act: run_shell ->   ";
    let summary = summarize_event(message);
    assert_eq!(summary.len(), 1);
    assert!(summary[0].0.contains("run_shell: no output"));

    let block = tool_preview(message).expect("empty observation should remain inspectable");
    assert!(block.summary().contains("no output"));
    assert!(!block.has_details());
    assert_eq!(block.details_text(), "no output");
    assert_eq!(block.commit_lines().len(), 1);

    let mut ui = Ui::default();
    ui.push_tool(block);
    assert!(ui
        .transcript
        .visible_lines(4)
        .iter()
        .any(|line| line.text.contains("no output")));
    ui.commit_live_tools();
    let panel = tool_history_panel(&ui.tool_history);
    assert!(panel.rows[0].key.contains("no output"));
    assert_eq!(panel.rows[0].value, "no output");
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
    assert_eq!(fmt_reasoning_meta(0, 2, 8), "THK[t+2s · 8 task tok] ");
    assert_eq!(
        fmt_reasoning_meta(3, 12, 34),
        "THK[step 3 · t+12s · 34 task tok] "
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
fn busy_signal_keeps_activity_rail_free_of_duplicate_token_telemetry() {
    let todos = vec![Todo {
        content: "inspect".into(),
        status: "in_progress".into(),
    }];
    let signal = fmt_busy_signal("tool · search · step 4", &todos, 12, 3, 2, None);
    assert_eq!(
        signal,
        "⚡ tool · search · step 4 · t+12s · 3/s · todo 0/1 · ⏳2"
    );
    assert!(!signal.contains(" in "));
    assert!(!signal.contains(" out "));
    assert!(!signal.contains("effort"));
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
        has_reasoning_history: false,
        has_live_answer: false,
        has_answer_history: false,
        has_live_history: false,
        has_tools,
        has_history,
        has_scrollable_tool_details: false,
        has_live_output: false,
        live_inspecting: false,
    })
}

#[test]
fn input_chrome_exposes_submit_or_queue_mode() {
    let (idle, idle_role) = chrome(false, 0, 80, false, true, false, false);
    assert!(idle.contains("Input"));
    assert!(idle.contains("Ctrl+R reasoning"));
    let (idle_send, _) = chrome(false, 0, 64, false, true, false, false);
    assert!(idle_send.contains("Enter send"), "{idle_send}");
    assert!(idle_send.contains("Tab complete"), "{idle_send}");
    assert!(!idle.contains("Ctrl+O"));
    assert!(!idle.contains("Alt+↑/↓ focus"));
    assert_eq!(idle_role, Role::Primary);

    let (queued, queued_role) = chrome(true, 2, 80, false, true, false, false);
    assert!(queued.contains("Queue [2]"));
    assert!(queued.contains("Ctrl+Enter front"));
    assert!(queued.contains("Ctrl+C takeover"));
    assert!(queued.contains("Ctrl+R reasoning"));
    assert!(!queued.contains("Ctrl+O"));
    assert_eq!(queued_role, Role::Primary);

    let (reasoning_history, _) = input_chrome(InputChromeArgs {
        busy: false,
        queued: 0,
        width: 96,
        reasoning_expanded: false,
        has_reasoning: false,
        has_reasoning_history: true,
        has_live_answer: false,
        has_answer_history: false,
        has_live_history: false,
        has_tools: false,
        has_history: false,
        has_scrollable_tool_details: false,
        has_live_output: false,
        live_inspecting: false,
    });
    assert!(reasoning_history.contains("Ctrl+R history"));
    assert!(
        reasoning_history.contains("Ctrl+T activity"),
        "{reasoning_history}"
    );
    assert!(str_cells(&reasoning_history) <= 94, "{reasoning_history}");

    let (answers_history, _) = input_chrome(InputChromeArgs {
        busy: false,
        queued: 0,
        width: 96,
        reasoning_expanded: false,
        has_reasoning: false,
        has_reasoning_history: false,
        has_live_answer: false,
        has_answer_history: true,
        has_live_history: false,
        has_tools: false,
        has_history: false,
        has_scrollable_tool_details: false,
        has_live_output: false,
        live_inspecting: false,
    });
    assert!(answers_history.contains("Ctrl+A answers"));
    assert!(
        answers_history.contains("Ctrl+T activity"),
        "{answers_history}"
    );
    assert!(str_cells(&answers_history) <= 94, "{answers_history}");

    for width in [12, 18, 24] {
        let (compact, _) = chrome(true, 2, width, false, true, false, false);
        assert!(
            compact.contains('↵'),
            "busy queue affordance hidden at {width}: {compact}"
        );
        assert!(
            str_cells(&compact) <= width.saturating_sub(2) as usize,
            "busy compact overflow at {width}: {compact}"
        );
    }

    let (idle_history, _) = chrome(false, 0, 80, false, true, false, true);
    assert!(idle_history.contains("Ctrl+O history"));
    assert!(!idle_history.contains("Ctrl+O details"));

    for width in [14_u16, 18, 24] {
        let (compact, _) = input_chrome(InputChromeArgs {
            busy: false,
            queued: 0,
            width,
            reasoning_expanded: false,
            has_reasoning: false,
            has_reasoning_history: false,
            has_live_answer: false,
            has_answer_history: true,
            has_live_history: false,
            has_tools: false,
            has_history: false,
            has_scrollable_tool_details: false,
            has_live_output: false,
            live_inspecting: false,
        });
        assert!(
            compact.contains('↵'),
            "idle submit affordance hidden at {width}: {compact}"
        );
        assert!(
            compact.contains('⇥'),
            "idle completion affordance hidden at {width}: {compact}"
        );
        assert!(
            str_cells(&compact) <= width.saturating_sub(2) as usize,
            "idle compact overflow at {width}: {compact}"
        );
    }

    for width in [10_u16, 18, 24, 40] {
        let (compact, _) = input_chrome(InputChromeArgs {
            busy: false,
            queued: 0,
            width,
            reasoning_expanded: false,
            has_reasoning: false,
            has_reasoning_history: false,
            has_live_answer: false,
            has_answer_history: false,
            has_live_history: false,
            has_tools: false,
            has_history: false,
            has_scrollable_tool_details: false,
            has_live_output: false,
            live_inspecting: false,
        });
        assert!(
            compact.contains('↵') || compact.contains("Enter"),
            "idle submit hidden at {width}: {compact}"
        );
        assert!(
            compact.contains('⇥') || compact.contains("Tab"),
            "idle completion hidden at {width}: {compact}"
        );
        assert!(
            str_cells(&compact) <= width.saturating_sub(2) as usize,
            "idle input overflow at {width}: {compact}"
        );
    }

    let (busy_tools, _) = chrome(true, 2, 80, false, true, true, false);
    assert!(busy_tools.contains("Queue [2]"));
    assert!(busy_tools.contains("Alt+↑/↓"));
    assert!(busy_tools.contains("^O details"));
    assert!(busy_tools.contains("^C takeover"));
    assert!(busy_tools.contains("^R"));

    let (busy_answer, _) = input_chrome(InputChromeArgs {
        busy: true,
        queued: 1,
        width: 80,
        reasoning_expanded: false,
        has_reasoning: false,
        has_reasoning_history: false,
        has_live_answer: true,
        has_answer_history: false,
        has_live_history: true,
        has_tools: false,
        has_history: false,
        has_scrollable_tool_details: false,
        has_live_output: true,
        live_inspecting: false,
    });
    assert!(busy_answer.contains("Ctrl+A focus"), "{busy_answer}");

    let (held_answer, _) = input_chrome(InputChromeArgs {
        busy: true,
        queued: 1,
        width: 96,
        reasoning_expanded: false,
        has_reasoning: false,
        has_reasoning_history: false,
        has_live_answer: true,
        has_answer_history: false,
        has_live_history: true,
        has_tools: false,
        has_history: false,
        has_scrollable_tool_details: false,
        has_live_output: true,
        live_inspecting: true,
    });
    assert!(held_answer.contains("^A answer"), "{held_answer}");

    let (live_inspect, _) = input_chrome(InputChromeArgs {
        busy: false,
        queued: 0,
        width: 96,
        reasoning_expanded: false,
        has_reasoning: false,
        has_reasoning_history: false,
        has_live_answer: false,
        has_answer_history: false,
        has_live_history: true,
        has_tools: false,
        has_history: false,
        has_scrollable_tool_details: false,
        has_live_output: true,
        live_inspecting: false,
    });
    assert!(live_inspect.contains("PgUp/PgDn page"));
    assert!(live_inspect.contains("Ctrl+I inspect"));
    let (live_follow, _) = input_chrome(InputChromeArgs {
        busy: false,
        queued: 0,
        width: 96,
        reasoning_expanded: false,
        has_reasoning: false,
        has_reasoning_history: false,
        has_live_answer: false,
        has_answer_history: false,
        has_live_history: true,
        has_tools: false,
        has_history: false,
        has_scrollable_tool_details: false,
        has_live_output: true,
        live_inspecting: true,
    });
    assert!(live_follow.contains("Alt+End follow"));
    let (held_focus, _) = input_chrome(InputChromeArgs {
        busy: true,
        queued: 1,
        width: 96,
        reasoning_expanded: false,
        has_reasoning: true,
        has_reasoning_history: false,
        has_live_answer: true,
        has_answer_history: false,
        has_live_history: true,
        has_tools: false,
        has_history: false,
        has_scrollable_tool_details: false,
        has_live_output: true,
        live_inspecting: true,
    });
    assert!(held_focus.contains("Alt+←/→ focus"), "{held_focus}");

    let (busy_tools_expanded, _) = chrome(true, 2, 80, true, true, true, false);
    assert!(busy_tools_expanded.contains("^C takeover"));
    assert!(busy_tools_expanded.contains("^R collapse"));
    let (busy_tools_scrolled, _) = input_chrome(InputChromeArgs {
        busy: true,
        queued: 2,
        width: 160,
        reasoning_expanded: true,
        has_reasoning: true,
        has_reasoning_history: false,
        has_live_answer: false,
        has_answer_history: false,
        has_live_history: false,
        has_tools: true,
        has_history: false,
        has_scrollable_tool_details: true,
        has_live_output: false,
        live_inspecting: false,
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

    let (wide_idle_shortcuts, _) = chrome(false, 0, 120, false, false, false, false);
    let expected_shortcut =
        if cfg!(windows) && std::env::var("RIDGE_TUI_KITTY").ok().as_deref() != Some("1") {
            "Alt+Enter/Ctrl+J newline"
        } else {
            "Shift/Alt+Enter newline"
        };
    assert_eq!(multiline_shortcut_label(true), "Shift/Alt+Enter newline");
    assert_eq!(multiline_shortcut_label(false), "Alt+Enter/Ctrl+J newline");
    assert!(wide_idle_shortcuts.contains(expected_shortcut));

    let (medium_without_tools, _) = chrome(true, 10, 64, false, true, false, false);
    assert!(!medium_without_tools.contains("Alt+↑/↓ focus"));

    let (medium_with_tools, _) = chrome(true, 10, 64, false, true, true, false);
    assert!(medium_with_tools.contains("^Enter front"));
    assert!(medium_with_tools.contains("^C takeover"));
    assert!(medium_with_tools.contains("^O details"));
    assert!(medium_with_tools.contains("^R"));

    let (narrow_medium_with_tools, _) = chrome(true, 10, 56, false, true, true, false);
    assert!(narrow_medium_with_tools.contains("↵ queue"));
    assert!(narrow_medium_with_tools.contains("^O details"));
    assert!(narrow_medium_with_tools.contains("^R"));

    let (medium_with_tools_expanded, _) = chrome(true, 10, 64, true, true, true, false);
    assert!(medium_with_tools_expanded.contains("^R"));

    let (narrow, narrow_role) = chrome(true, 10, 15, false, false, true, false);
    assert_eq!(narrow, " Q:[10]↵^C^O ");
    assert_eq!(narrow_role, Role::Primary);
    assert!(str_cells(&narrow) <= 13);
    assert!(narrow.contains("^C"), "takeover disappeared: {narrow}");

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

    for width in [12, 15, 18, 32] {
        let (busy, _) = chrome(true, 2, width, false, true, true, false);
        assert!(busy.contains("^C"), "takeover hidden at {width}: {busy}");
        assert!(str_cells(&busy) <= width.saturating_sub(2) as usize);
    }
}

#[test]
fn narrow_idle_history_keeps_answer_and_reasoning_entrypoints_visible() {
    let answer = input_chrome(InputChromeArgs {
        busy: false,
        queued: 0,
        width: 32,
        reasoning_expanded: false,
        has_reasoning: false,
        has_reasoning_history: false,
        has_live_answer: false,
        has_answer_history: true,
        has_live_history: false,
        has_tools: false,
        has_history: false,
        has_scrollable_tool_details: false,
        has_live_output: false,
        live_inspecting: false,
    })
    .0;
    assert!(answer.contains("^A"), "answer archive hidden: {answer}");
    assert!(str_cells(&answer) <= 30);

    let reasoning = input_chrome(InputChromeArgs {
        busy: false,
        queued: 0,
        width: 32,
        reasoning_expanded: false,
        has_reasoning: false,
        has_reasoning_history: true,
        has_live_answer: false,
        has_answer_history: false,
        has_live_history: false,
        has_tools: false,
        has_history: false,
        has_scrollable_tool_details: false,
        has_live_output: false,
        live_inspecting: false,
    })
    .0;
    assert!(
        reasoning.contains("^R"),
        "reasoning archive hidden: {reasoning}"
    );
    assert!(str_cells(&reasoning) <= 30);
}

#[test]
fn busy_tool_action_rail_preserves_reasoning_and_focus_at_medium_widths() {
    for width in [72_u16, 80, 88, 95] {
        let (text, role) = input_chrome(InputChromeArgs {
            busy: true,
            queued: 2,
            width,
            reasoning_expanded: false,
            has_reasoning: true,
            has_reasoning_history: false,
            has_live_answer: false,
            has_answer_history: false,
            has_live_history: false,
            has_tools: true,
            has_history: false,
            has_scrollable_tool_details: false,
            has_live_output: false,
            live_inspecting: false,
        });
        assert_eq!(role, Role::Primary);
        assert!(
            str_cells(&text) <= width.saturating_sub(2) as usize,
            "width={width}: {text}"
        );
        assert!(text.contains("^C"), "width={width}: {text}");
        assert!(text.contains("^R"), "width={width}: {text}");
        assert!(text.contains("^O"), "width={width}: {text}");
        assert!(text.contains("Alt+↑/↓"), "width={width}: {text}");
        if width < 88 {
            assert!(
                text.contains("queue"),
                "queue affordance hidden at {width}: {text}"
            );
        } else {
            assert!(
                text.contains("Enter queue"),
                "queue affordance hidden at {width}: {text}"
            );
        }
    }

    let (wide, _) = chrome(true, 2, 96, false, true, true, false);
    assert!(wide.contains("Ctrl+R reasoning"), "{wide}");
    assert!(wide.contains("Ctrl+O details"), "{wide}");
    assert!(wide.contains("Alt+↑/↓ focus"), "{wide}");
}

#[test]
fn busy_live_inspection_prioritizes_follow_and_takeover() {
    for width in [48_u16, 56, 64, 72, 80, 88, 96] {
        let (text, role) = input_chrome(InputChromeArgs {
            busy: true,
            queued: 2,
            width,
            reasoning_expanded: false,
            has_reasoning: true,
            has_reasoning_history: false,
            has_live_answer: false,
            has_answer_history: false,
            has_live_history: true,
            has_tools: true,
            has_history: true,
            has_scrollable_tool_details: true,
            has_live_output: true,
            live_inspecting: true,
        });
        assert!(text.contains("Alt+End follow"), "{width}: {text}");
        assert!(text.contains("^C takeover"), "{width}: {text}");
        if width >= 72 {
            assert!(text.contains("Esc/^C takeover"), "{width}: {text}");
        }
        if width >= 72 {
            assert!(text.contains("Space toggle"), "{width}: {text}");
        }
        if width >= 88 {
            assert!(text.contains("Alt+←/→ focus"), "{width}: {text}");
        } else if width >= 80 {
            assert!(text.contains("Alt<> focus"), "{width}: {text}");
        } else if width >= 72 {
            assert!(text.contains("←→"), "{width}: {text}");
        }
        assert!(str_cells(&text) <= width.saturating_sub(2) as usize);
        assert_eq!(role, Role::Primary);
    }
}

#[test]
fn held_inspector_space_toggles_only_semantic_blocks() {
    let space = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
    assert!(live_semantic_toggle_action(&space, false, true, true));
    assert!(!live_semantic_toggle_action(&space, false, false, true));
    assert!(!live_semantic_toggle_action(&space, true, true, true));
    assert!(!live_semantic_toggle_action(&space, false, true, false));
    assert!(!live_semantic_toggle_action(
        &KeyEvent::new(KeyCode::Char(' '), KeyModifiers::SHIFT),
        false,
        true,
        true
    ));
}

#[test]
fn semantic_toggle_dispatches_to_existing_tool_and_reasoning_state() {
    let mut tools = Ui::default();
    tools.push_tool(
        ToolBlock::from_lines(vec![
            ("read_file".into(), Color::Cyan),
            ("detail line".into(), Color::Gray),
        ])
        .expect("tool"),
    );
    tools.push_chunk(provider::StreamChunk::Answer("answer".into()));
    assert!(tools.hold_live());
    assert!(tools.toggle_focused_semantic());
    assert!(tools
        .transcript
        .visible_lines(8)
        .iter()
        .any(|line| line.text == "detail line"));
    assert!(tools.toggle_focused_semantic());
    assert!(!tools
        .transcript
        .visible_lines(8)
        .iter()
        .any(|line| line.text == "detail line"));

    let mut reasoning = Ui::default();
    reasoning.push_chunk(provider::StreamChunk::Reasoning("plan".into()));
    assert!(reasoning.hold_live());
    assert!(reasoning.toggle_focused_semantic());
    assert!(reasoning.transcript.is_reasoning_expanded());
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
    assert_eq!(
        detail_match_scroll("01234567890123456789 needle", "needle", 10, 2),
        1,
        "long wrapped line should position the matching segment"
    );
    assert_eq!(
        detail_match_scroll("你好你好你好needle", "needle", 4, 2),
        2,
        "CJK cell width must affect matching-row positioning"
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
fn tool_history_detail_scroll_moves_around_search_anchor() {
    let text = "zero\none\nneedle\nlast";
    assert_eq!(detail_scroll_position(text, "needle", 40, 2, 0), 1);
    assert_eq!(detail_scroll_position(text, "needle", 40, 2, -4), 0);
    assert_eq!(detail_scroll_position(text, "needle", 40, 2, 4), 2);
    assert_eq!(detail_scroll_position(text, "", 40, 2, 4), 2);
}

#[test]
fn narrow_frame_retains_context_and_token_status() {
    let ui = Ui::default();
    let meta = ReplMeta {
        tools: Vec::new(),
        provider: "test".into(),
        provider_label: "test".into(),
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
        if width >= 32 {
            assert!(
                symbols.contains("C80") || symbols.contains("ctx"),
                "context telemetry hidden at {width}: {symbols}"
            );
        }
        assert!(
            symbols.contains("I~160K") || symbols.contains("in ~160000"),
            "input token telemetry hidden at {width}: {symbols}"
        );
        assert!(
            symbols.contains("O0") || symbols.contains("out 0"),
            "output token telemetry hidden at {width}: {symbols}"
        );
        assert!(
            symbols.contains("Edef") || symbols.contains("effort default"),
            "effort telemetry hidden at {width}: {symbols}"
        );
        assert!(
            symbols.contains("12_345")
                || symbols.contains("12,345")
                || symbols.contains("12345")
                || width < 48,
            "token telemetry hidden at {width}: {symbols}"
        );
    }
}

#[test]
fn compact_status_prioritizes_live_telemetry_by_width() {
    let wide = compact_status_line(
        72, "openai", "gpt-5", "80%", 12_345, "~160000", "42", "high",
    );
    assert!(wide.contains("openai/gpt-5"));
    assert!(wide.contains("I~160K O42"));
    assert!(wide.contains("Ehigh"));
    assert!(wide.contains("T12K"));

    let medium = compact_status_line(
        40, "openai", "gpt-5", "80%", 12_345, "~160000", "42", "default",
    );
    assert!(medium.contains("C80%"));
    assert!(medium.contains("I~160K O42"));
    assert!(medium.contains("Edef"));
    assert!(str_cells(&medium) <= 40);

    let tiny = compact_status_line(
        18, "openai", "gpt-5", "80%", 12_345, "~160000", "42", "default",
    );
    assert!(tiny.contains("Edef"));
    assert!(tiny.contains("I~160K"));
    assert!(str_cells(&tiny) <= 18);
}

#[test]
fn compact_status_projection_separates_labels_without_changing_text() {
    let compact = compact_status_line(
        40, "openai", "gpt-5", "80%", 12_345, "~160000", "42", "default",
    );
    let projected = status_line_projection(&compact);
    let line = projected.lines.first().expect("status line");
    let rendered = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();

    assert_eq!(rendered, compact);
    assert!(line.spans.iter().any(|span| {
        span.content == "I"
            && span.style.fg == Some(role_color(Role::Label))
            && span.style.add_modifier.contains(Modifier::DIM)
    }));
    assert!(line.spans.iter().any(|span| {
        span.content == "~160K" && span.style.fg == Some(role_color(Role::Metric))
    }));
}

#[test]
fn device_auth_code_is_visible_in_live_frame() {
    let ui = Ui {
        device_auth_status: Some("Device code: TEST-1234".into()),
        ..Ui::default()
    };
    let meta = ReplMeta {
        tools: Vec::new(),
        provider: "openai".into(),
        provider_label: "openai".into(),
        model: "gpt-5".into(),
        base_url: String::new(),
        status_bar: "ready".into(),
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
    let mut terminal =
        Terminal::new(ratatui::backend::TestBackend::new(80, 12)).expect("device auth terminal");
    terminal
        .draw(|frame| draw(frame, &ui, &meta, 0, &vitals, None))
        .expect("device auth draw");
    let symbols = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        symbols.contains("TEST-1234"),
        "device code hidden: {symbols}"
    );
    assert!(
        symbols.contains("auth.openai.com/codex/device"),
        "device URL hidden: {symbols}"
    );
    assert!(
        symbols.contains('╭'),
        "device auth modal lost rounded frame: {symbols}"
    );
}

#[test]
fn input_surface_uses_rounded_frame() {
    let ui = Ui::default();
    let meta = ReplMeta {
        tools: Vec::new(),
        provider: "openai".into(),
        provider_label: "openai".into(),
        model: "gpt-5".into(),
        base_url: String::new(),
        status_bar: "ready".into(),
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
    let mut terminal =
        Terminal::new(ratatui::backend::TestBackend::new(80, 12)).expect("input terminal");
    terminal
        .draw(|frame| draw(frame, &ui, &meta, 0, &vitals, None))
        .expect("input draw");
    let symbols = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    for corner in ['╭', '╮', '╰', '╯'] {
        assert!(
            symbols.contains(corner),
            "input corner {corner} missing: {symbols}"
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

#[test]
fn named_provider_selection_keeps_profile_identity_separate_from_wire_kind() {
    let cfg = Config::parse(
        r#"{
            "providers": [{
                "name": "Zai",
                "kind": "openai",
                "model": "glm-4.6",
                "base_url": "https://open.bigmodel.cn/api/paas/v4"
            }]
        }"#,
    );
    assert_eq!(named_profile_name(&cfg, "zai").as_deref(), Some("Zai"));
    assert_eq!(named_profile_name(&cfg, "openai"), None);
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
    let p = models_panel_with_effort(&grouped, "test", "b", "medium");
    assert_eq!(p.kind, PanelKind::Models);
    // key 格式: "provider · model_id"
    assert_eq!(p.selected().map(|r| r.key.as_str()), Some("test · b"));
    assert_eq!(p.rows[0].ctx, Some(128_000)); // 携真实窗口供选中缓存
    assert!(p.rows[2].value.contains('?')); // 缺 ctx 显 ?
}

#[test]
fn models_panel_keeps_chatgpt_in_dedicated_group() {
    let grouped = vec![
        ("zai".into(), vec![mi("glm-4.6", None)]),
        (
            CHATGPT_MODEL_GROUP.into(),
            vec![mi("gpt-5.6-sol", Some(200_000))],
        ),
    ];
    let p = models_panel_with_effort(&grouped, CHATGPT_MODEL_GROUP, "gpt-5.6-sol", "medium");
    assert_eq!(
        p.selected().map(|r| r.key.as_str()),
        Some("ChatGPT (Codex) · gpt-5.6-sol")
    );
    assert!(p
        .rows
        .iter()
        .any(|row| row.key.starts_with("ChatGPT (Codex) · ")));
}

#[test]
fn models_panel_exposes_effort_group_and_current_value() {
    let p = models_panel_with_effort(&[], CHATGPT_MODEL_GROUP, "gpt-5.6-sol", "high");
    assert_eq!(
        p.rows
            .iter()
            .find(|row| row.key == "Effort · high")
            .map(|row| row.value.as_str()),
        Some("current")
    );
    assert!(p.rows.iter().any(|row| row.key == "Effort · max"));
    assert!(p.title.contains("effort high"));
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
fn slash_popup_surfaces_goal_and_attention_commands() {
    for (input, expected) in [
        ("/go", "/goal"),
        ("/in", "/inspect"),
        ("/rea", "/reasoning"),
    ] {
        let mut state = InputState::default();
        state.insert_str(input);
        let popup = build_popup(&state).expect("slash command should be discoverable");
        assert_eq!(popup.items, vec![expected.to_owned()], "input={input}");
    }
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
    assert_eq!(
        panel_action(&KeyEvent::new(KeyCode::PageUp, KeyModifiers::ALT)),
        PanelAction::DetailPageUp
    );
    assert_eq!(
        panel_action(&KeyEvent::new(KeyCode::PageDown, KeyModifiers::ALT)),
        PanelAction::DetailPageDown
    );
    assert_eq!(panel_action(&press(KeyCode::Home)), PanelAction::First);
    assert_eq!(panel_action(&press(KeyCode::End)), PanelAction::Last);
    assert_eq!(panel_action(&press(KeyCode::Enter)), PanelAction::Enter);
    assert_eq!(panel_action(&press(KeyCode::Esc)), PanelAction::Esc);
    assert_eq!(panel_action(&press(KeyCode::Delete)), PanelAction::Remove);
    assert_eq!(
        panel_action(&KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL)),
        PanelAction::Remove
    );
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
        provider_label: "test".into(),
        model: "model".into(),
        base_url: String::new(),
        status_bar: String::new(),
        ctx_window: 200_000,
    };
    let swap = Arc::new(provider::SwapProvider::new(Arc::new(
        provider::ScriptedProvider::new(Vec::new()),
    )));
    let agents = agent::Agents::default();
    let catalog = CommandCatalog {
        agents: &agents,
        commands: &[],
        skills: &[],
    };

    let should_exit = run_command(
        "/history",
        &mut ui,
        &mut history,
        &mut meta,
        &swap,
        &catalog,
        CommandStats {
            tokens: 0,
            turns: 0,
        },
    )
    .await
    .expect("history command");

    assert!(!should_exit);
    assert!(matches!(
        ui.panel.as_ref().map(|p| p.kind),
        Some(PanelKind::ToolHistory)
    ));
    assert!(ui.panel.as_ref().is_some_and(|p| p.rows.len() == 1));
    ui.panel = None;
    ui.push_chunk(provider::StreamChunk::Reasoning("live inspect".into()));
    run_command(
        "/inspect",
        &mut ui,
        &mut history,
        &mut meta,
        &swap,
        &catalog,
        CommandStats {
            tokens: 0,
            turns: 0,
        },
    )
    .await
    .expect("inspect command");
    assert!(matches!(
        ui.panel.as_ref().map(|p| p.kind),
        Some(PanelKind::LiveHistory)
    ));
    run_command(
        "/queue",
        &mut ui,
        &mut history,
        &mut meta,
        &swap,
        &catalog,
        CommandStats {
            tokens: 0,
            turns: 0,
        },
    )
    .await
    .expect("queue command");
    assert!(matches!(
        ui.panel.as_ref().map(|panel| panel.kind),
        Some(PanelKind::Queue)
    ));
    assert!(SLASH_COMMANDS.contains(&"/history"));
    assert!(SLASH_COMMANDS.contains(&"/activity"));
    assert!(SLASH_COMMANDS.contains(&"/queue"));
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

#[test]
fn decide_key_preserves_momentary_hold_press_and_release() {
    use std::collections::HashSet;
    let mut pressed = HashSet::new();
    let modifiers = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
    let press = KeyEvent::new_with_kind(KeyCode::Char('2'), modifiers, KeyEventKind::Press);
    let release = KeyEvent::new_with_kind(KeyCode::Char('2'), modifiers, KeyEventKind::Release);

    let down = decide_key(&mut pressed, &press).expect("hold press should survive filtering");
    assert_eq!(down.kind, KeyEventKind::Press);
    assert!(live_hold_toggle_action(&down, false, true));

    let up = decide_key(&mut pressed, &release).expect("hold release should survive filtering");
    assert_eq!(up.kind, KeyEventKind::Release);
    assert!(live_hold_release_action(&up, false));
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
/// busy 时 Enter 不提交,浮窗态 ↑↓选择、Tab补全、Enter提交,字符穿透,松键忽略。
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
    assert_eq!(
        input_action(
            &KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
            true,
            false
        ),
        InputAction::PushNow
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
        input_action(
            &KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
            true,
            false
        ),
        InputAction::ToggleAnswer
    );
    assert_eq!(
        input_action(
            &KeyEvent::new(KeyCode::Char('O'), KeyModifiers::CONTROL),
            false,
            false
        ),
        InputAction::ToggleDetails
    );
    assert_eq!(
        input_action(
            &KeyEvent::new(KeyCode::Char('R'), KeyModifiers::CONTROL),
            true,
            false
        ),
        InputAction::ToggleReasoning
    );
    assert_eq!(
        input_action(
            &KeyEvent::new(KeyCode::Char('T'), KeyModifiers::CONTROL),
            true,
            false
        ),
        InputAction::ToggleActivity
    );
    assert_eq!(
        input_action(
            &KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
            true,
            false
        ),
        InputAction::OpenLiveSearch
    );
    assert!(queue_panel_toggle_action(&KeyEvent::new(
        KeyCode::Char('q'),
        KeyModifiers::CONTROL
    )));
    assert!(!queue_panel_toggle_action(&press(KeyCode::Char('q'))));
    assert!(live_history_toggle_action(
        &KeyEvent::new(KeyCode::Char('i'), KeyModifiers::CONTROL),
        false,
        true
    ));
    assert!(live_history_toggle_action(
        &KeyEvent::new(KeyCode::Char('i'), KeyModifiers::ALT),
        false,
        true
    ));
    assert!(!live_history_toggle_action(
        &KeyEvent::new(KeyCode::Char('i'), KeyModifiers::CONTROL),
        true,
        true
    ));
    assert!(!live_history_toggle_action(
        &KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
        false,
        true
    ));
    assert_eq!(
        input_action(
            &KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
            true,
            false
        ),
        InputAction::PushNow
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
        semantic_focus_action(
            &KeyEvent::new(KeyCode::Left, KeyModifiers::ALT),
            false,
            true,
            true,
        ),
        Some(-1)
    );
    assert_eq!(
        semantic_focus_action(
            &KeyEvent::new(KeyCode::Right, KeyModifiers::ALT),
            false,
            true,
            true,
        ),
        Some(1)
    );
    assert_eq!(
        semantic_focus_action(
            &KeyEvent::new(KeyCode::Tab, KeyModifiers::ALT),
            false,
            true,
            true,
        ),
        None
    );
    assert_eq!(
        semantic_focus_action(
            &KeyEvent::new(KeyCode::Right, KeyModifiers::ALT),
            false,
            false,
            true,
        ),
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
    assert_eq!(
        live_scroll_action(
            &KeyEvent::new(KeyCode::PageUp, KeyModifiers::ALT),
            false,
            false,
            true
        ),
        Some(LiveScrollAction::Older)
    );
    assert_eq!(
        live_scroll_action(
            &KeyEvent::new(KeyCode::PageDown, KeyModifiers::ALT),
            false,
            false,
            true
        ),
        Some(LiveScrollAction::Newer)
    );
    assert_eq!(
        live_scroll_action(
            &KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
            false,
            false,
            true
        ),
        Some(LiveScrollAction::OlderPage)
    );
    assert_eq!(
        live_scroll_action(
            &KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
            false,
            false,
            true
        ),
        Some(LiveScrollAction::NewerPage)
    );
    assert!(live_hold_toggle_action(
        &KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL),
        false,
        true
    ));
    assert!(live_hold_toggle_action(
        &KeyEvent::new(
            KeyCode::Char('2'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        ),
        false,
        true
    ));
    assert!(live_hold_toggle_action(
        &KeyEvent::new(
            KeyCode::Char('@'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        ),
        false,
        true
    ));
    assert!(!live_hold_toggle_action(
        &KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL),
        true,
        true
    ));
    assert!(!live_hold_toggle_action(
        &KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL),
        false,
        false
    ));
    assert!(live_hold_release_action(
        &KeyEvent::new_with_kind(
            KeyCode::Char(' '),
            KeyModifiers::CONTROL,
            KeyEventKind::Release,
        ),
        false,
    ));
    assert!(!live_hold_release_action(
        &KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL),
        false,
    ));
    assert!(!live_hold_release_action(
        &KeyEvent::new_with_kind(
            KeyCode::Char(' '),
            KeyModifiers::CONTROL,
            KeyEventKind::Release,
        ),
        true,
    ));
    assert_eq!(
        live_scroll_action(
            &KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
            false,
            true,
            true
        ),
        Some(LiveScrollAction::OlderPage)
    );
    assert_eq!(
        live_scroll_action(
            &KeyEvent::new(KeyCode::End, KeyModifiers::ALT),
            false,
            false,
            true
        ),
        Some(LiveScrollAction::Follow)
    );
    assert_eq!(
        live_scroll_action(
            &KeyEvent::new(KeyCode::PageUp, KeyModifiers::ALT),
            false,
            true,
            true
        ),
        None
    );
    // 浮窗态:Tab 接受补全但不提交;Enter 接受补全并直接提交。
    assert_eq!(
        input_action(&press(KeyCode::Tab), false, true),
        InputAction::PopupAccept
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
        InputAction::PopupSubmit
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
            &KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
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
        input_action(&press(KeyCode::Esc), true, false),
        InputAction::Interrupt
    );
    assert_eq!(
        input_action(&press(KeyCode::Esc), false, false),
        InputAction::Ignore
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

#[test]
fn decide_key_filters_windows_release_without_losing_ime_characters() {
    let mut pressed = std::collections::HashSet::new();
    let press = KeyEvent::new_with_kind(KeyCode::Enter, KeyModifiers::NONE, KeyEventKind::Press);
    assert!(decide_key(&mut pressed, &press).is_some());
    assert!(decide_key(
        &mut pressed,
        &KeyEvent::new_with_kind(KeyCode::Enter, KeyModifiers::NONE, KeyEventKind::Release,)
    )
    .is_none());

    // Some IMEs inject a character only on Release; preserve it and normalize
    // its non-breaking space representation before downstream routing.
    let ime = decide_key(
        &mut pressed,
        &KeyEvent::new_with_kind(
            KeyCode::Char('\u{a0}'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ),
    )
    .expect("unpaired character release should remain usable");
    assert_eq!(ime.code, KeyCode::Char(' '));
    assert_eq!(ime.kind, KeyEventKind::Press);

    let hold = KeyEvent::new_with_kind(
        KeyCode::Char(' '),
        KeyModifiers::CONTROL,
        KeyEventKind::Press,
    );
    assert!(decide_key(&mut pressed, &hold).is_some());
    let release = KeyEvent::new_with_kind(
        KeyCode::Char(' '),
        KeyModifiers::CONTROL,
        KeyEventKind::Release,
    );
    assert_eq!(
        decide_key(&mut pressed, &release).map(|key| key.kind),
        Some(KeyEventKind::Release),
        "Ctrl+Space release must reach momentary audit"
    );
}

#[test]
fn panel_attention_shortcuts_remain_global_while_browsing() {
    let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
    assert_eq!(
        panel_attention_action(&key('a'), true, false),
        Some(InputAction::ToggleAnswer)
    );
    assert_eq!(
        panel_attention_action(&key('r'), true, false),
        Some(InputAction::ToggleReasoning)
    );
    assert_eq!(
        panel_attention_action(&key('o'), true, false),
        Some(InputAction::ToggleDetails)
    );
    assert_eq!(
        panel_attention_action(&key('t'), true, false),
        Some(InputAction::ToggleActivity)
    );
}

#[test]
fn panel_attention_shortcuts_ignore_editor_and_popup() {
    let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
    for c in ['a', 'r', 'o', 't'] {
        assert_eq!(panel_attention_action(&key(c), false, false), None);
        assert_eq!(panel_attention_action(&key(c), true, true), None);
    }
}

#[test]
fn wide_audit_panel_hint_exposes_global_attention_switches() {
    let panel = Panel::new(PanelKind::Activity, "Activity".into(), Vec::new());
    let hint = panel_hint(&panel, 96);
    assert!(
        hint.contains("^R think"),
        "missing reasoning affordance: {hint}"
    );
    assert!(
        hint.contains("^A answers"),
        "missing answer affordance: {hint}"
    );
    assert!(hint.contains("^O tools"), "missing tool affordance: {hint}");
    assert!(
        hint.contains("^T activity"),
        "missing activity affordance: {hint}"
    );
    assert!(str_cells(&hint) <= 96, "hint overflow: {hint}");

    let compact = panel_hint(&panel, 72);
    assert!(
        compact.contains("^A/^R/^O/^T audit"),
        "missing compact affordance: {compact}"
    );
    assert!(
        str_cells(&compact) <= 72,
        "compact hint overflow: {compact}"
    );

    let mut editing = Panel::new(PanelKind::Activity, "Activity".into(), Vec::new());
    editing.editing = Some("query".into());
    assert!(!panel_hint(&editing, 96).contains("^R think"));
}

#[test]
fn audit_panel_titles_keep_answer_and_reasoning_roles_distinct() {
    assert_eq!(panel_title_role(PanelKind::AnswerHistory), Role::Primary);
    assert_eq!(panel_title_role(PanelKind::ReasoningHistory), Role::Info);
    assert_eq!(panel_title_role(PanelKind::ToolHistory), Role::Info);
    assert_eq!(panel_title_role(PanelKind::Activity), Role::Info);
}

#[test]
fn physical_enter_spellings_share_queue_and_front_queue_routing() {
    for code in [KeyCode::Char('\r'), KeyCode::Char('\n'), KeyCode::Char('m')] {
        let ctrl = KeyEvent::new(code, KeyModifiers::CONTROL);
        assert_eq!(
            input_action(&ctrl, true, false),
            InputAction::PushNow,
            "{code:?} with Ctrl must front-queue while busy"
        );
    }
    for code in [KeyCode::Char('\r'), KeyCode::Char('\n')] {
        let plain = KeyEvent::new(code, KeyModifiers::NONE);
        assert_eq!(
            input_action(&plain, true, false),
            InputAction::Queue,
            "{code:?} must queue while busy"
        );
    }

    let mut pressed = std::collections::HashSet::new();
    let normalized = decide_key(
        &mut pressed,
        &KeyEvent::new(KeyCode::Char('\n'), KeyModifiers::CONTROL),
    )
    .expect("LF press should remain an input event");
    assert_eq!(normalized.code, KeyCode::Enter);
    assert!(decide_key(
        &mut pressed,
        &KeyEvent::new_with_kind(
            KeyCode::Char('\n'),
            KeyModifiers::CONTROL,
            KeyEventKind::Release,
        )
    )
    .is_none());
}

#[test]
fn pending_queue_preview_wraps_and_remains_bounded() {
    let queue = std::collections::VecDeque::from([
        "first pending message with enough text to wrap".to_owned(),
        "second pending message".to_owned(),
        "third pending message".to_owned(),
        "fourth pending message".to_owned(),
    ]);
    let lines = pending_queue_lines(&queue, 24);
    assert!(lines.len() <= MAX_PENDING_PREVIEW_ROWS);
    assert_eq!(lines[0].spans[0].style.fg, Some(role_color(Role::Primary)));
    let text = |line: &Line<'static>| {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    };
    assert!(text(&lines[0]).contains("next"));
    assert!(lines.iter().any(|line| text(line).contains("first")));
    assert!(lines.iter().all(|line| str_cells(&text(line)) <= 24));
}

#[test]
fn pending_queue_preview_bounds_large_pasted_message() {
    let queue = std::collections::VecDeque::from([format!(
        "head of pending paste {}",
        "x".repeat(MAX_PENDING_PREVIEW_CHARS * 8)
    )]);
    let lines = pending_queue_lines(&queue, 24);
    let text = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert!(lines.len() <= MAX_PENDING_PREVIEW_ROWS);
    assert!(text.iter().any(|line| line.contains("head of pending")));
    assert!(text.iter().any(|line| line.contains("more queued text")));
    assert!(text.iter().all(|line| str_cells(line) <= 24));
}

#[test]
fn pending_queue_stays_visible_in_short_live_frame() {
    let mut ui = Ui {
        busy: true,
        activity: "reasoning".into(),
        ..Ui::default()
    };
    ui.queued
        .push_back("keep this pending intent visible".into());
    let meta = ReplMeta {
        tools: Vec::new(),
        provider: "test".into(),
        provider_label: "test".into(),
        model: "model".into(),
        base_url: String::new(),
        status_bar: "{provider} · {model}".into(),
        ctx_window: 200_000,
    };
    let vitals = Vitals {
        step: 1,
        elapsed_s: 2,
        task_tokens: 3,
        rate: 1,
        ctx_used: 4,
        queued: 1,
    };
    let mut terminal =
        Terminal::new(ratatui::backend::TestBackend::new(40, 7)).expect("short queue terminal");
    terminal
        .draw(|frame| draw(frame, &ui, &meta, 0, &vitals, None))
        .expect("short queue draw");
    let symbols = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        symbols.contains("keep this pending") || symbols.contains("keep this"),
        "queued intent hidden in short frame: {symbols}"
    );
}

#[test]
fn pending_queue_stays_above_wrapped_draft_and_cursor_tail() {
    let mut ui = Ui {
        busy: true,
        activity: "reasoning".into(),
        ..Ui::default()
    };
    ui.queued
        .push_back("keep this pending intent visible".into());
    ui.input
        .insert_str(&format!("{}\nvisible draft tail", "x".repeat(160)));
    let meta = ReplMeta {
        tools: Vec::new(),
        provider: "test".into(),
        provider_label: "test".into(),
        model: "model".into(),
        base_url: String::new(),
        status_bar: "status".into(),
        ctx_window: 200_000,
    };
    let vitals = Vitals {
        step: 1,
        elapsed_s: 2,
        task_tokens: 3,
        rate: 1,
        ctx_used: 4,
        queued: 1,
    };
    let mut terminal =
        Terminal::new(ratatui::backend::TestBackend::new(40, 8)).expect("queued draft terminal");
    terminal
        .draw(|frame| draw(frame, &ui, &meta, 0, &vitals, None))
        .expect("queued draft draw");
    let symbols = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        symbols.contains("keep this pending"),
        "queued intent hidden behind wrapped draft: {symbols}"
    );
    assert!(
        symbols.contains("visible draft tail"),
        "cursor-near input tail hidden by queue preview: {symbols}"
    );
}

#[test]
fn ctrl_c_requires_second_press_within_two_seconds() {
    let now = std::time::Instant::now();
    assert!(!is_second_ctrl_c(None, now));
    assert!(is_second_ctrl_c(
        Some(now),
        now + std::time::Duration::from_secs(1)
    ));
    assert!(!is_second_ctrl_c(
        Some(now),
        now + std::time::Duration::from_secs(3)
    ));
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
    let tail = tail_display_cells(&"x".repeat(100), 4, 2);
    assert_eq!(str_cells(&tail), 8);
    assert!(tail.starts_with('…'));
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

#[test]
fn input_history_switches_from_global_to_session_scope() {
    let mut s = InputState::default();
    s.set_history(vec!["global command".into()], false);
    s.insert_str("session task");
    assert_eq!(s.take(), "session task");
    s.drop_last_history_if("session task");
    s.begin_session();
    s.push_history("session task");
    s.insert_str("draft");
    assert!(!s.move_up());
    s.recall_prev();
    assert_eq!(s.buffer, "session task");
    s.recall_next();
    assert_eq!(s.buffer, "draft");
    assert!(s.session_mode);
    assert!(!s.history.iter().any(|item| item == "global command"));
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
    q.insert_str("/res");
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
        if height >= 5 {
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
        provider_label: "test".into(),
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
        assert!(
            symbols.contains("draft"),
            "editable draft lost at {height}: {symbols}"
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
fn responsive_live_layout_handles_ultra_low_height_editor_fallback() {
    let meta = ReplMeta {
        tools: Vec::new(),
        provider: "test".into(),
        provider_label: "test".into(),
        model: "model".into(),
        base_url: String::new(),
        status_bar: "status".into(),
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
    for height in [2, 3] {
        let mut ui = Ui::default();
        ui.push_chunk(provider::StreamChunk::Answer("answer survives".into()));
        ui.input.insert_str("draft survives");
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(24, height))
            .expect("ultra-low terminal");
        terminal
            .draw(|frame| draw(frame, &ui, &meta, 1, &vitals, None))
            .expect("ultra-low draw");
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
            symbols.contains("draft survives"),
            "draft lost at {height}: {symbols}"
        );
    }

    let mut terminal =
        Terminal::new(ratatui::backend::TestBackend::new(24, 1)).expect("single-row terminal");
    terminal
        .draw(|frame| {
            let ui = Ui::default();
            draw(frame, &ui, &meta, 0, &vitals, None);
        })
        .expect("single-row draw");
}

#[test]
fn ultra_low_height_pending_queue_stays_visible_above_or_with_draft() {
    let meta = ReplMeta {
        tools: Vec::new(),
        provider: "test".into(),
        provider_label: "test".into(),
        model: "model".into(),
        base_url: String::new(),
        status_bar: "status".into(),
        ctx_window: 200_000,
    };
    let vitals = Vitals {
        step: 1,
        elapsed_s: 1,
        task_tokens: 1,
        rate: 1,
        ctx_used: 1,
        queued: 1,
    };
    for height in [2, 3] {
        let mut ui = Ui::default();
        ui.queued.push_back("queued intent".into());
        ui.input.insert_str("draft survives");
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(24, height))
            .expect("ultra-low queue terminal");
        terminal
            .draw(|frame| draw(frame, &ui, &meta, 1, &vitals, None))
            .expect("ultra-low queue draw");
        let symbols = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            symbols.contains("draft survives"),
            "draft lost at {height}: {symbols}"
        );
        if height == 2 {
            assert!(
                symbols.contains("1 pending"),
                "queue count lost at {height}: {symbols}"
            );
        } else {
            assert!(
                symbols.contains("queued intent"),
                "queue preview lost at {height}: {symbols}"
            );
        }
    }
}

#[test]
fn tiny_frames_keep_input_slot_visible() {
    let meta = ReplMeta {
        tools: Vec::new(),
        provider: "test".into(),
        provider_label: "test".into(),
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
            symbols.contains("Input") || symbols.contains("In"),
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

#[test]
fn live_page_rows_reserves_chrome_and_keeps_one_output_row() {
    assert_eq!(live_page_rows(24), 19);
    assert_eq!(live_page_rows(5), 1);
    assert_eq!(live_page_rows(0), 1);
}

#[test]
fn live_frame_plan_keeps_queue_status_and_slots_in_one_projection() {
    let mut ui = Ui {
        busy: true,
        input_tokens: 12,
        output_tokens: 7,
        effort: Some("high".into()),
        ..Ui::default()
    };
    ui.queued.push_back("/next".into());
    let meta = ReplMeta {
        tools: Vec::new(),
        provider: "test".into(),
        provider_label: "Test".into(),
        model: "model".into(),
        base_url: String::new(),
        status_bar: "{provider} · {model} · {ctx} · {tokens}".into(),
        ctx_window: 100,
    };
    let vitals = Vitals {
        step: 2,
        elapsed_s: 3,
        task_tokens: 19,
        rate: 4,
        ctx_used: 20,
        queued: 1,
    };
    let plan = LiveFramePlan::build(
        Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 12,
        },
        &ui,
        &meta,
        19,
        &vitals,
    );
    let slots = plan.slots();
    assert_eq!(slots.iter().map(|slot| slot.height).sum::<u16>(), 12);
    assert!(slots[0].height >= 1, "live output must retain one row");
    assert!(plan.status_text().contains("I12"));
    assert!(plan.status_text().contains("O7"));
    assert!(plan.status_text().contains("Ehi"));
}

/// iter-26:流式尾巴 —— 少于 K 全量,多于 K 取尾。
#[test]
fn stream_tail_takes_last_k_lines() {
    assert_eq!(stream_tail("a\nb\nc", 5), vec!["a", "b", "c"]);
    assert_eq!(stream_tail("a\nb\nc\nd\ne\nf", 3), vec!["d", "e", "f"]);
    assert!(stream_tail("", 3).is_empty());
}

#[test]
fn live_output_inspection_pauses_and_returns_to_follow() {
    let mut transcript = LiveTranscript::default();
    transcript.push_reasoning("r0\nr1\nr2\nr3\nr4\nr5\nr6\nr7\nr8\nr9");
    transcript.push_answer("a0\na1\na2\na3\na4");

    assert!(!transcript.is_inspecting());
    assert_eq!(
        transcript.visible_lines(4).last().map(|line| line.text),
        Some("a4")
    );
    assert!(transcript.scroll_live(1));
    assert!(transcript.is_inspecting());
    let older = transcript.visible_lines(4);
    assert_ne!(older.last().map(|line| line.text), Some("a4"));

    transcript.push_answer("\na5");
    assert_ne!(
        transcript.visible_lines(4).last().map(|line| line.text),
        Some("a5")
    );
    assert!(transcript.follow_live());
    assert!(!transcript.is_inspecting());
    assert_eq!(
        transcript.visible_lines(4).last().map(|line| line.text),
        Some("a5")
    );

    transcript.scroll_live(1);
    transcript.clear_streams();
    assert!(!transcript.is_inspecting());
}

#[test]
fn explicit_live_hold_is_visible_even_with_short_output() {
    let mut transcript = LiveTranscript::default();
    transcript.push_answer("one line");
    assert!(!transcript.is_inspecting());
    assert!(transcript.hold_live());
    assert!(transcript.is_inspecting());
    transcript.push_answer("\ntwo");
    assert!(transcript.is_inspecting());
    assert!(transcript.follow_live());
    assert!(!transcript.is_inspecting());
    assert_eq!(
        transcript.visible_lines(2).last().map(|line| line.text),
        Some("two")
    );
}

#[test]
fn live_hold_keeps_the_same_logical_rows_as_stream_grows() {
    let mut transcript = LiveTranscript::default();
    transcript.push_reasoning("line 0\nline 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7");
    assert!(transcript.hold_live());
    let before = transcript
        .visible_lines(3)
        .into_iter()
        .map(|line| line.text.to_owned())
        .collect::<Vec<_>>();
    assert_eq!(before, ["line 1", "line 2", "line 3"]);

    transcript.push_reasoning("\nline 8");
    let after = transcript
        .visible_lines(3)
        .into_iter()
        .map(|line| line.text.to_owned())
        .collect::<Vec<_>>();
    assert_eq!(after, before);
    assert!(transcript.is_inspecting());
}

#[test]
fn held_cache_reflow_preserves_synthetic_continuation_anchor() {
    let mut transcript = LiveTranscript::default();
    transcript.push_answer(
        &(0..14)
            .map(|index| {
                if index == 5 {
                    format!("line {index} long-body-abcdefghijklmnopqrstuvwx")
                } else {
                    format!("line {index} unique")
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
    );
    assert!(transcript.hold_live());

    let mut cache = LiveOutputCache::default();
    let vitals = Vitals {
        step: 0,
        elapsed_s: 0,
        task_tokens: 0,
        rate: 0,
        ctx_used: 0,
        queued: 0,
    };
    let before = cache.lines(&transcript, 12, 3, false, &vitals);
    let after = cache.lines(&transcript, 24, 3, false, &vitals);
    let text = |lines: &[Line<'static>]| {
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>()
    };
    let before_text = text(&before);
    let after_text = text(&after);
    assert!(
        before_text.contains("continues"),
        "before resize: {before_text}"
    );
    assert!(
        after_text.contains("continues"),
        "after resize: {after_text}"
    );
    assert!(
        !after_text.contains("line 0"),
        "resize must not jump back to the pinned answer header: {after_text}"
    );
}

#[test]
fn live_page_scroll_moves_by_viewport_and_returns_to_follow() {
    let mut transcript = LiveTranscript::default();
    transcript.push_answer(
        &(0..40)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );

    assert!(transcript.scroll_live_page(1, 8));
    let older = transcript.visible_lines(8);
    assert_ne!(older.last().map(|line| line.text), Some("line39"));
    assert!(transcript.scroll_live_page(-1, 8));
    assert_eq!(
        transcript.visible_lines(8).last().map(|line| line.text),
        Some("line39")
    );
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
    assert_eq!(role_color(Role::Reasoning), Color::LightBlue);
    assert_eq!(role_color(Role::Border), Color::DarkGray);
    assert_eq!(role_color(Role::Command), Color::White);
    assert_eq!(role_color(Role::Answer), Color::White);
    assert_eq!(role_color(Role::Info), Color::Gray);
    assert_eq!(role_color(Role::Muted), Color::DarkGray);
    assert_eq!(role_color(Role::Metric), Color::White);
    assert_eq!(role_color(Role::Label), Color::DarkGray);
}

#[test]
fn telemetry_surface_keeps_muted_status_text_readable() {
    let style = telemetry_surface().fg(role_color(Role::Muted));
    assert_eq!(style.bg, Some(Color::Reset));
    assert_ne!(style.fg, style.bg);
}

#[test]
fn markdown_alerts_render_semantic_rails_without_leaking_syntax() {
    let (warning, next) = md_line_spans("> [!WARNING] **Protect** the boundary", false);
    assert!(!next);
    assert_eq!(
        warning
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>(),
        "│ WARNING ┃ Protect the boundary"
    );
    assert_eq!(warning[0].style.fg, Some(role_color(Role::Warn)));
    assert_eq!(warning[1].style.fg, Some(role_color(Role::Warn)));
    assert!(warning[1].style.add_modifier.contains(Modifier::BOLD));

    let (tip, _) = md_line_spans("> [!TIP] Use the fast path", false);
    assert_eq!(tip[0].style.fg, Some(role_color(Role::Success)));
    assert!(tip.iter().all(|span| !span.content.contains("[!TIP]")));

    let (quote, _) = md_line_spans("> ordinary quote", false);
    assert_eq!(
        quote
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>(),
        "│ ordinary quote"
    );
}

#[test]
fn markdown_alert_continuation_keeps_semantic_rail() {
    let lines = markdown_lines(
        "🤖 > [!WARNING] Protect the boundary\n> Continue **this** conclusion\nplain",
    );
    let continuation = lines[1]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert_eq!(continuation, "└ Continue this conclusion");
    assert_eq!(lines[1].spans[0].style.fg, Some(role_color(Role::Warn)));

    let mut in_code = false;
    let mut alert_role = None;
    let _ = live_markdown_spans_with_alert(
        "> [!WARNING] head",
        &mut in_code,
        Color::White,
        Modifier::empty(),
        &mut alert_role,
    );
    let live_continuation = live_markdown_spans_with_alert(
        "> live conclusion",
        &mut in_code,
        Color::White,
        Modifier::empty(),
        &mut alert_role,
    );
    assert_eq!(live_continuation[0].style.fg, Some(role_color(Role::Warn)));
    assert_eq!(
        live_continuation
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>(),
        "│ live conclusion"
    );
}

#[test]
fn markdown_alert_edges_form_a_bounded_static_container() {
    let lines = markdown_lines(
        "🤖 > [!WARNING] Protect the boundary\n> Continue **this** conclusion\nplain",
    );
    let rendered = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert_eq!(rendered[0], "🤖 ┌ WARNING ┃ Protect the boundary");
    assert_eq!(rendered[1], "└ Continue this conclusion");
    assert_eq!(rendered[2], "plain");
    assert_eq!(lines[0].spans[1].style.fg, Some(role_color(Role::Warn)));
    assert_eq!(lines[1].spans[0].style.fg, Some(role_color(Role::Warn)));
}

#[test]
fn selection_style_is_quiet_focus() {
    let style = selection_style();
    assert_eq!(style.fg, Some(role_color(Role::Primary)));
    assert_eq!(style.bg, Some(Color::DarkGray));
    assert!(style.add_modifier.contains(Modifier::BOLD));
}

/// iter-28:md 轻渲染 —— 围栏切态、bounded code roles、标题粗、行内 code、未闭合按字面。
#[test]
fn md_line_rendering() {
    let (spans, state) = md_line_spans("```rust", false);
    assert!(state);
    assert_eq!(spans.len(), 2);
    assert_eq!(spans[0].content, "```");
    assert_eq!(spans[1].content, "rust");
    let (_, state2) = md_line_spans("```", true);
    assert!(!state2);
    let (s, st) = md_line_spans("let x = 1;", true);
    assert!(st);
    assert_eq!(
        s.iter()
            .find(|span| span.content.as_ref() == "let")
            .expect("keyword span")
            .style
            .fg,
        Some(role_color(Role::Primary))
    );
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
    assert_eq!(lines[2].spans[0].style.fg, Some(role_color(Role::Primary)));
    assert_eq!(lines[3].spans[0].style.fg, Some(role_color(Role::Border)));
    let visible = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(!visible.contains('\x1b'));
}

#[test]
fn committed_answer_keeps_a_semantic_rail_after_leaving_live_view() {
    let lines = answer_commit_lines("🤖 **answer**\nnext line");
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].spans[0].content.as_ref(), "╭ ANSWER ");
    assert_eq!(lines[1].spans[0].content.as_ref(), "╰ ");
    assert!(lines[0]
        .spans
        .iter()
        .any(|span| span.content.as_ref().contains("Ctrl+A answers")));
    assert_eq!(lines[0].spans[0].style.fg, Some(role_color(Role::Primary)));
    assert_eq!(
        answer_commit_measure("first\nsecond"),
        "╭ ANSWER first  [Ctrl+A answers]\n╰ second"
    );
}

#[test]
fn reasoning_scrollback_closes_multiline_block() {
    let lines = reasoning_commit_lines("first thought\nsecond thought", 2, 3, 5, 80);
    assert_eq!(lines[1].spans[0].content.as_ref(), "┊ ");
    assert_eq!(lines[2].spans[0].content.as_ref(), "└ ");
    assert!(lines.iter().all(|line| {
        line.spans
            .iter()
            .map(|span| str_cells(span.content.as_ref()))
            .sum::<usize>()
            <= 80
    }));
}

#[test]
fn answer_scrollback_closes_multiline_block() {
    let lines = answer_commit_lines("🤖 first answer\nsecond answer");
    assert_eq!(lines[0].spans[0].content.as_ref(), "╭ ANSWER ");
    assert_eq!(lines[1].spans[0].content.as_ref(), "╰ ");
}

#[test]
fn activity_scrollback_closes_multiline_block() {
    let lines = activity_commit_lines(3, ActivityKind::Conclusion, "settling\nresult ready", 80);
    assert_eq!(lines[1].spans[0].content.as_ref(), "⟦SUM #3⟧ ");
    assert_eq!(lines[2].spans[0].content.as_ref(), "└ ");
}

#[test]
fn committed_answer_exposes_observed_context_alongside_its_rail() {
    let lines = answer_commit_lines_with_status_and_metrics(
        "🤖 answer",
        false,
        Some(PresentationMetrics {
            step: 2,
            elapsed_s: 3,
            tokens: 17,
            chars: 11,
        }),
    );
    let first = lines[0]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(first.starts_with("╭ ANSWER "), "{first}");
    assert!(first.contains("[step 2 · +3s · 17 task tok]"), "{first}");
    assert!(first.contains("Ctrl+A answers"), "{first}");
    assert!(
        lines[0].spans.iter().any(|span| {
            span.content.as_ref().contains("step 2")
                && span.style.fg == Some(role_color(Role::Label))
        }),
        "answer metadata should be visually subordinate"
    );
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
fn live_alerts_keep_semantic_role_through_markdown_projection() {
    let mut in_code = false;
    let spans = live_markdown_line(
        "> [!CAUTION] stop here",
        32,
        &mut in_code,
        Color::White,
        Modifier::BOLD,
    );
    assert_eq!(
        spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>(),
        "│ CAUTION ┃ stop here"
    );
    assert_eq!(spans[0].style.fg, Some(role_color(Role::Error)));
    assert_eq!(spans[1].style.fg, Some(role_color(Role::Error)));
    assert!(spans[1].style.add_modifier.contains(Modifier::BOLD));
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
        [CommitBlock::Markdown { text, .. }] if text == "🤖 **answer**"
    ));
}

#[test]
fn interrupted_live_answer_is_retained_without_faking_completion() {
    let mut ui = Ui::default();
    ui.push_chunk(provider::StreamChunk::Answer("partial\nanswer".into()));

    ui.commit_live_answers("interrupted before final response", 3, 7);

    assert_eq!(ui.answer_history.len(), 1);
    assert!(ui
        .answer_history
        .back()
        .is_some_and(|answer| answer.partial));
    let panel = answer_history_panel(&ui.answer_history);
    assert!(panel.rows[0].key.contains("PARTIAL"));
    assert_eq!(panel.rows[0].value, "partial\nanswer");
    let blocks = ui.drain_commit_blocks();
    assert!(matches!(
        blocks.as_slice(),
        [
            CommitBlock::Text { text, .. },
            CommitBlock::Markdown { text: body, .. }
        ] if text.contains("partial answer retained") && body == "partial\nanswer"
    ));
    assert!(!ui.transcript.has_inspectable_output());
}

#[test]
fn partial_answer_scrollback_keeps_answer_channel_and_marks_partial() {
    let mut terminal = Terminal::with_options(
        ratatui::backend::TestBackend::new(64, 8),
        TerminalOptions {
            viewport: Viewport::Inline(4),
        },
    )
    .expect("terminal");
    let mut ui = Ui::default();
    ui.push_chunk(provider::StreamChunk::Answer("partial answer".into()));
    ui.commit_live_answers("interrupted before final response", 2, 3);

    flush_commits(&mut terminal, &mut ui).expect("partial answer scrollback");
    let symbols = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(symbols.contains("ANSWER"));
    assert!(symbols.contains("step 2"), "{symbols}");
    assert!(symbols.contains("task tok"), "{symbols}");
    assert!(symbols.contains("PARTIAL"));
}

#[test]
fn partial_answer_marker_wraps_cjk_markdown_at_supported_widths() {
    let body = "🤖 # 结论\n> [!NOTE] 你你\n```rust\n你你\n```\nanswer tail";
    for width in [32, 40, 80, 96] {
        let lines = wrap_commit_lines(answer_commit_lines_with_status(body, true), width);
        assert!(!lines.is_empty(), "width={width}");
        assert!(
            lines.iter().all(|line| {
                line.spans
                    .iter()
                    .map(|span| str_cells(span.content.as_ref()))
                    .sum::<usize>()
                    <= width as usize
            }),
            "width={width} lines={lines:?}"
        );
        let rendered = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(rendered.contains("ANSWER"), "width={width} {rendered}");
        assert!(rendered.contains("PARTIAL"), "width={width} {rendered}");
    }
}

#[test]
fn partial_answer_archive_bounds_and_expands_long_detail() {
    let mut ui = Ui::default();
    let body = format!(
        "PARTIAL HEAD {}\n{}\nPARTIAL TAIL",
        "h".repeat(MAX_ANSWER_HISTORY_CHARS / 2),
        "middle ".repeat(600)
    );
    ui.push_chunk(provider::StreamChunk::Answer(body));
    ui.commit_live_answers("run ended before final response", 4, 12);

    let stored = &ui.answer_history.back().expect("partial archive").text;
    assert!(stored.contains("PARTIAL HEAD"));
    assert!(stored.contains("middle omitted"));
    assert!(stored.contains("PARTIAL TAIL"));
    assert!(ui.open_answer_history());
    let panel = ui.panel.as_mut().expect("answer archive panel");
    assert!(panel
        .selected()
        .is_some_and(|row| row.key.contains("PARTIAL")));
    assert!(panel.toggle_detail());
    assert!(panel
        .selected()
        .is_some_and(|row| row.value.contains("PARTIAL TAIL")));
}

#[test]
fn answer_archive_evicts_partial_before_completed_conclusions() {
    let mut ui = Ui::default();
    ui.note_markdown("completed investigation conclusion");
    for index in 0..MAX_ANSWER_HISTORY {
        ui.push_chunk(provider::StreamChunk::Answer(format!("partial {index}")));
        ui.commit_live_answers("interrupted before final response", index, index as u64);
    }

    assert_eq!(ui.answer_history.len(), MAX_ANSWER_HISTORY);
    assert!(ui
        .answer_history
        .iter()
        .any(|entry| !entry.partial && entry.text == "completed investigation conclusion"));
    assert_eq!(
        ui.answer_history
            .iter()
            .filter(|entry| entry.partial)
            .count(),
        MAX_ANSWER_HISTORY - 1
    );
}

/// iter-52:TestBackend 复现窄终端渲染，证明宽字符折行不注入 ANSI 残留且不 panic。
#[test]
fn markdown_render_survives_narrow_test_backend() {
    let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(12, 8)).expect("terminal");
    let lines = markdown_lines("🤖 # 标题\n> [!WARNING] 你你\n```rust\n你你\n```\nplain");
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
        provider_label: "test".into(),
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
    assert_eq!(idle_rail.fg, role_color(Role::Reasoning));

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

    let mut wrapped_ui = Ui::default();
    wrapped_ui.push_chunk(provider::StreamChunk::Answer(
        "a very long live answer that must stay within the viewport".into(),
    ));
    let mut terminal =
        Terminal::new(ratatui::backend::TestBackend::new(18, 8)).expect("wrapped terminal");
    terminal
        .draw(|frame| draw(frame, &wrapped_ui, &meta, 8, &vitals, None))
        .expect("wrapped draw");
    let symbols = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        symbols.contains("stay"),
        "wrapped live answer disappeared: {symbols}"
    );
    assert!(
        !symbols.contains('…'),
        "wrapped live answer was clipped: {symbols}"
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

    let mut reasoning = Panel::new(
        PanelKind::ReasoningHistory,
        "Reasoning history · Enter expand · Esc close".into(),
        vec![PanelRow {
            key: "#1 step 3 · 8 tok".into(),
            value: "inspect state and compare observations".into(),
            ctx: None,
        }],
    );
    reasoning.query = "state".into();
    reasoning.retype();
    reasoning.detail_open = true;

    let mut live = Panel::new(
        PanelKind::LiveHistory,
        "Live blocks · Enter expand · Esc close".into(),
        vec![PanelRow {
            key: "#1 🤖 Answer · 12 chars".into(),
            value: "answer detail".into(),
            ctx: None,
        }],
    );
    live.query = "answer".into();
    live.retype();
    live.detail_open = true;

    for (name, panel) in [
        ("tools", tools),
        ("history", history),
        ("reasoning", reasoning),
        ("live", live),
    ] {
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
            if width < 14 {
                assert!(
                    symbols.contains("↕"),
                    "narrow action legend disappeared: {name} {symbols}"
                );
            }
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
fn live_inspector_panel_exposes_hold_follow_controls() {
    let mut live = Panel::new(
        PanelKind::LiveHistory,
        "Live blocks · Enter expand · Esc close".into(),
        vec![PanelRow {
            key: "#1 🤖 Answer · 12 chars".into(),
            value: "answer detail".into(),
            ctx: None,
        }],
    );
    live.detail_open = true;

    for width in [24, 32, 40, 80] {
        let mut terminal =
            Terminal::new(ratatui::backend::TestBackend::new(width, 10)).expect("live panel");
        terminal
            .draw(|frame| draw_panel(frame, frame.area(), &live))
            .expect("live panel draw");
        let symbols = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        let follow = if width >= 40 { "Ctrl+Space" } else { "^Space" };
        assert!(
            symbols.contains(follow),
            "Live Inspector follow control disappeared at {width}: {symbols}"
        );
        assert!(!symbols.contains('\x1b'));
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
        provider_label: "test".into(),
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
        provider_label: "test".into(),
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
        provider_label: "test".into(),
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
        symbols.contains('┃') && symbols.contains('╰'),
        "answer semantic rails: {symbols}"
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
fn static_fenced_code_reuses_bounded_token_roles() {
    let lines = markdown_lines("🤖 ```rust\nlet value: usize = 42; \"ok\" // note\n```");
    let code = &lines[1];
    let span = |text: &str| {
        code.spans
            .iter()
            .find(|span| span.content.as_ref() == text)
            .unwrap_or_else(|| panic!("missing code span: {text}"))
    };

    assert_eq!(span("let").style.fg, Some(role_color(Role::Primary)));
    assert_eq!(span("usize").style.fg, Some(role_color(Role::Info)));
    assert_eq!(span("42").style.fg, Some(role_color(Role::Warn)));
    assert_eq!(span("\"ok\"").style.fg, Some(role_color(Role::Success)));
    assert_eq!(span("// note").style.fg, Some(role_color(Role::Muted)));
}

#[test]
fn fenced_language_token_has_display_only_semantic_style() {
    let lines = markdown_lines("  ```rust extra\nlet value = 42;\n```");
    let opener = &lines[0];
    let rendered = opener
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert_eq!(rendered, "  ```rust extra");
    assert_eq!(str_cells(&rendered), str_cells("  ```rust extra"));

    let language = opener
        .spans
        .iter()
        .find(|span| span.content.as_ref() == "rust")
        .expect("language span");
    assert_eq!(language.style.fg, Some(role_color(Role::Info)));
    assert!(language.style.add_modifier.contains(Modifier::BOLD));

    let bare = markdown_lines("```");
    assert_eq!(bare[0].spans.len(), 1);
    assert_eq!(bare[0].spans[0].style.fg, Some(role_color(Role::Border)));
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
fn long_static_answer_keeps_head_tail_archive_and_bounded_commit_work() {
    let mut terminal = Terminal::with_options(
        ratatui::backend::TestBackend::new(40, 12),
        TerminalOptions {
            viewport: Viewport::Inline(6),
        },
    )
    .expect("terminal");
    let body = format!(
        "STRESS_ANSWER_BEGIN\n{}\nSTRESS_ANSWER_END",
        "中文 CJK markdown stream with `token` and **emphasis**. ".repeat(2_000)
    );
    let mut ui = Ui::default();
    ui.note_markdown(body);

    flush_commits(&mut terminal, &mut ui).expect("bounded static answer");

    let archived = &ui.answer_history.back().expect("answer archive").text;
    assert!(archived.starts_with("STRESS_ANSWER_BEGIN"));
    assert!(archived.contains("middle omitted"));
    assert!(archived.ends_with("STRESS_ANSWER_END"));
    assert!(ui.commits.is_empty());
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
    assert!(symbols.contains("T tool summary"));
    assert!(symbols.contains("┆"));
}

#[test]
fn tool_history_is_collapsed_and_expandable_after_static_commit() {
    let mut ui = Ui::default();
    ui.push_tool(
        ToolBlock::from_lines_with_phase(
            vec![
                ("tool summary".into(), Color::Cyan),
                ("detail one".into(), Color::Gray),
            ],
            ToolPhase::Observation,
            Some("read_file".into()),
        )
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
    assert!(static_symbols.contains("O tool summary"));
    assert!(static_symbols.contains("folded"));
    assert!(static_symbols.contains("Ctrl+O"));
    assert!(static_symbols.contains("details"));
    assert!(static_symbols.contains("1 rows"));
    assert!(!static_symbols.contains("detail one"));
    assert!(ui.toggle_details_or_history());

    let mut meta = ReplMeta {
        tools: Vec::new(),
        provider: "test".into(),
        provider_label: "test".into(),
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
            ..
        } if text == "inspect actual state"
    )));
    assert!(ui
        .transcript
        .visible_lines(4)
        .iter()
        .any(|line| { line.kind == LiveLineKind::Answer && line.text == "final answer" }));
    ui.clear_streams();
    assert!(ui.transcript.visible_lines(4).is_empty());
    assert_eq!(ui.splash, SPLASH_TICKS);
    assert!(ui.drain_commits().iter().any(|(text, color)| text
        == "┊ THK[step 3 · t+12s · 8 task tok] inspect actual state  [Ctrl+R history]"
        && *color == role_color(Role::Reasoning)));
}

#[test]
fn static_tool_projection_preserves_call_and_output_phase() {
    let call = ToolBlock::from_lines_with_phase(
        vec![("read_file: src/lib.rs".into(), Color::Cyan)],
        ToolPhase::Call,
        Some("read_file".into()),
    )
    .expect("call tool");
    let output = ToolBlock::from_lines_with_phase(
        vec![("read_file: 12 lines".into(), Color::Green)],
        ToolPhase::Observation,
        Some("read_file".into()),
    )
    .expect("output tool");

    assert_eq!(call.phase_label(), "CALL");
    assert_eq!(output.phase_label(), "OUT");
}

#[test]
fn reasoning_commit_renders_in_inline_scrollback() {
    let mut ui = Ui::default();
    ui.push_chunk(provider::StreamChunk::Reasoning(
        "actual plan\nsecond thought".into(),
    ));
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
    let body = symbols.replace("│ ", "");
    assert!(body.contains("actual plan"));
    assert!(symbols.contains("t+12s"));
    assert!(symbols.contains("task tok"));
    assert!(symbols.contains("THK["));
    assert!(symbols.contains("Ctrl+R"));
    assert!(symbols.contains("┊"));
    assert!(symbols.contains("│"));
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

#[test]
fn reasoning_scrollback_preserves_markdown_roles() {
    let lines = reasoning_commit_lines(
        "# plan\n```rust\nfn main() { let count: usize = 42; }\n```\n> [!WARNING] caution",
        2,
        12,
        9,
        80,
    );
    let body = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(body.contains("┊ "));
    assert!(body.contains("THK[step 2"));
    assert!(body.contains("t+12s"));
    assert!(body.contains("9 task tok] "));
    assert!(body.contains("[Ctrl+R history]"));

    let heading = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.as_ref() == "#")
        .expect("heading span");
    assert_eq!(heading.style.fg, Some(role_color(Role::Primary)));
    assert!(heading.style.add_modifier.contains(Modifier::BOLD));

    let code_type = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.as_ref() == "u")
        .expect("code type span");
    assert_eq!(code_type.style.fg, Some(role_color(Role::Info)));
    assert!(code_type.style.add_modifier.contains(Modifier::DIM));
    assert!(code_type.style.add_modifier.contains(Modifier::ITALIC));

    assert!(lines.iter().all(|line| {
        line.spans
            .iter()
            .map(|span| str_cells(span.content.as_ref()))
            .sum::<usize>()
            <= 80
    }));
}

#[test]
fn reasoning_scrollback_preserves_markdown_alert_edges() {
    let lines = reasoning_commit_lines(
        "intro\n> [!WARNING] protect the boundary\n> continue **this** conclusion\nplain",
        2,
        12,
        9,
        96,
    );
    let rendered = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert!(
        rendered.iter().any(|line| line.contains("┌ WARNING")),
        "alert top edge missing: {rendered:?}"
    );
    assert!(
        rendered.iter().any(|line| line.contains("└ continue")),
        "alert bottom edge missing: {rendered:?}"
    );
    assert!(rendered.iter().all(|line| str_cells(line) <= 96));
}

#[test]
fn semantic_reasoning_rail_prefers_word_boundaries_on_narrow_reflow() {
    let text = "┊ THK[t+3s · 19 task tok] fixture reasoning: waiting without network; queue and takeover remain available  [Ctrl+R history]";
    let lines = wrap_commit_lines(vec![Line::from(Span::raw(text))], 40);
    let rendered = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();

    assert!(rendered.iter().any(|line| line.starts_with("│ ")));
    assert!(
        rendered.iter().all(|line| str_cells(line) <= 40),
        "{rendered:?}"
    );
    for word in [
        "reasoning",
        "network",
        "queue",
        "takeover",
        "available",
        "history",
    ] {
        assert!(
            rendered.iter().any(|line| line.contains(word)),
            "{word} split across semantic rail: {rendered:?}"
        );
    }
}

#[test]
fn reasoning_history_is_bounded_searchable_and_expandable() {
    let mut ui = Ui::default();
    for step in 0..=MAX_REASONING_HISTORY {
        ui.push_chunk(provider::StreamChunk::Reasoning(format!(
            "thought {step}\nsecond line"
        )));
        ui.commit_live_reasoning(step, step as u64);
    }

    assert_eq!(ui.reasoning_history.len(), MAX_REASONING_HISTORY);
    assert!(!ui
        .reasoning_history
        .iter()
        .any(|entry| entry.text.starts_with("thought 0")));
    assert!(ui
        .reasoning_history
        .back()
        .is_some_and(|entry| entry.text.starts_with("thought 8")));
    assert!(ui.toggle_reasoning_or_history());

    let panel = ui.panel.as_mut().expect("reasoning history panel");
    assert_eq!(panel.kind, PanelKind::ReasoningHistory);
    assert!(panel
        .selected()
        .is_some_and(|row| row.key.contains("step 8")));
    assert!(panel.toggle_detail());
    assert!(panel.detail_open);
    assert!(panel
        .selected()
        .is_some_and(|row| row.value.contains("second line")));

    panel.query = "thought 4".into();
    panel.retype();
    assert_eq!(panel.view.len(), 1);
    assert!(panel
        .selected()
        .is_some_and(|row| row.key.contains("step 4")));
}

#[test]
fn matching_audit_history_shortcut_closes_the_current_panel() {
    let mut reasoning = Ui::default();
    reasoning.push_chunk(provider::StreamChunk::Reasoning("saved thought".into()));
    reasoning.commit_live_reasoning(1, 1);
    assert!(reasoning.open_reasoning_history());
    assert!(reasoning.toggle_reasoning_or_history());
    assert!(reasoning.panel.is_none());

    let mut tools = Ui::default();
    tools.push_tool(ToolBlock::from_lines(vec![("saved tool".into(), Color::Cyan)]).expect("tool"));
    tools.commit_live_tools();
    assert!(tools.open_tool_history());
    assert!(tools.toggle_details_or_history());
    assert!(tools.panel.is_none());
}

#[test]
fn answer_history_is_bounded_searchable_and_expandable() {
    let mut ui = Ui::default();
    for index in 0..=MAX_ANSWER_HISTORY {
        ui.note_markdown(format!(
            "🤖 answer {index}\nfull conclusion line 1\nfull conclusion line 2"
        ));
    }

    assert_eq!(ui.answer_history.len(), MAX_ANSWER_HISTORY);
    assert!(!ui
        .answer_history
        .iter()
        .any(|entry| entry.text.starts_with("🤖 answer 0")));
    assert!(ui
        .answer_history
        .back()
        .is_some_and(|entry| entry.text.starts_with("🤖 answer 8")));
    assert!(ui.open_answer_history());

    let panel = ui.panel.as_mut().expect("answer history panel");
    assert_eq!(panel.kind, PanelKind::AnswerHistory);
    assert!(panel
        .selected()
        .is_some_and(|row| row.key.contains("ANSWER") && row.key.contains("#1")));
    assert!(panel.toggle_detail());
    assert!(panel
        .selected()
        .is_some_and(|row| row.value.contains("full conclusion line 2")));

    panel.query = "answer 4".into();
    panel.retype();
    assert_eq!(panel.view.len(), 1);
    assert!(panel
        .selected()
        .is_some_and(|row| row.value.starts_with("🤖 answer 4")));
}

#[test]
fn answer_history_shortcut_opens_and_closes_without_mutating_input() {
    let mut ui = Ui::default();
    ui.input.insert_str("draft intervention");
    ui.note_markdown("final conclusion");

    assert_eq!(
        input_action(
            &KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
            false,
            false
        ),
        InputAction::ToggleAnswer
    );
    assert!(ui.toggle_answer_or_history());
    assert_eq!(ui.input.buffer, "draft intervention");
    assert_eq!(
        ui.panel.as_ref().map(|panel| panel.kind),
        Some(PanelKind::AnswerHistory)
    );
    assert!(ui.toggle_answer_or_history());
    assert!(ui.panel.is_none());
}

#[test]
fn live_answer_shortcut_focuses_answer_then_returns_to_follow() {
    let mut ui = Ui {
        busy: true,
        ..Ui::default()
    };
    ui.push_chunk(provider::StreamChunk::Answer("streaming answer".into()));

    assert!(ui.toggle_answer_or_history());
    assert!(ui.panel.is_none());
    assert!(ui.transcript.is_inspecting());
    assert!(matches!(
        ui.transcript.focused_block(),
        Some(LiveBlockFocus::Answer(_))
    ));

    assert!(ui.toggle_answer_or_history());
    assert!(!ui.transcript.is_inspecting());
    assert!(ui.transcript.focused_block().is_none());
}

#[test]
fn answer_history_rows_expose_step_elapsed_and_tokens() {
    let mut ui = Ui::default();
    ui.note_markdown_with_meta("first conclusion", 2, 4, 11);
    ui.note_markdown_with_meta("second conclusion", 7, 18, 33);
    assert!(ui.open_answer_history());

    let panel = ui.panel.as_mut().expect("answer history panel");
    let newest = panel.selected().expect("newest answer").key.clone();
    assert!(newest.contains("#1 ANSWER"));
    assert!(newest.contains("step 7"));
    assert!(newest.contains("33 tok"));
    assert!(newest.contains("+18s"));

    panel.move_down();
    let older = panel.selected().expect("older answer").key.clone();
    assert!(older.contains("#2 ANSWER"));
    assert!(older.contains("step 2"));
    assert!(older.contains("11 tok"));
    assert!(older.contains("+4s"));
}

#[test]
fn answer_history_bounds_large_detail_without_changing_scrollback_commit() {
    let mut ui = Ui::default();
    let body = format!(
        "🤖 HEAD {}\n{}\nTAIL conclusion",
        "h".repeat(MAX_ANSWER_HISTORY_CHARS),
        "middle ".repeat(MAX_ANSWER_HISTORY_CHARS)
    );
    ui.note_markdown(body.clone());

    let stored = &ui.answer_history.back().expect("answer history").text;
    assert!(stored.contains("HEAD"));
    assert!(stored.contains("middle omitted"));
    assert!(stored.contains("TAIL conclusion"));
    assert!(stored.chars().count() < body.chars().count());
    assert!(matches!(
        ui.commits.as_slice(),
        [CommitBlock::Markdown { text, .. }] if text == &body
    ));
}

#[test]
fn reasoning_history_bounds_large_detail_without_changing_scrollback_commit() {
    let mut ui = Ui::default();
    let body = format!(
        "THINK HEAD {}\n{}\nTAIL reasoning conclusion",
        "h".repeat(MAX_REASONING_HISTORY_CHARS + 100),
        "middle ".repeat(1_000)
    );
    ui.push_chunk(provider::StreamChunk::Reasoning(body.clone()));
    ui.commit_live_reasoning(7, 9);

    let stored = &ui.reasoning_history.back().expect("reasoning history").text;
    assert!(stored.contains("THINK HEAD"));
    assert!(stored.contains("middle omitted"));
    assert!(stored.contains("TAIL reasoning conclusion"));
    assert!(stored.chars().count() < body.chars().count());
    assert!(matches!(
        ui.commits.as_slice(),
        [CommitBlock::Reasoning { text, .. }] if text == &body
    ));
}

#[test]
fn live_block_inspector_tracks_mixed_stream_and_expands_selected_block() {
    let mut ui = Ui::default();
    ui.push_chunk(provider::StreamChunk::Reasoning("plan first".into()));
    ui.push_tool(
        ToolBlock::from_lines(vec![
            ("read_file".into(), Color::Cyan),
            ("file contents".into(), Color::Gray),
        ])
        .expect("tool"),
    );
    ui.push_chunk(provider::StreamChunk::Answer("answer now".into()));

    assert!(ui.open_live_history());
    let panel = ui.panel.as_mut().expect("live inspector");
    assert_eq!(panel.kind, PanelKind::LiveHistory);
    assert!(panel
        .selected()
        .is_some_and(|row| row.key.contains("Answer")));
    assert!(panel.toggle_detail());
    assert!(panel
        .selected()
        .is_some_and(|row| row.value == "answer now"));

    panel.move_down();
    assert!(panel
        .selected()
        .is_some_and(|row| row.key.contains("read_file")));
    assert!(panel.detail_open);
    assert!(panel
        .selected()
        .is_some_and(|row| row.value == "file contents"));

    // Appending a new block refreshes the open panel without closing it.
    ui.push_chunk(provider::StreamChunk::Reasoning("follow-up".into()));
    assert!(ui
        .panel
        .as_ref()
        .is_some_and(|panel| panel.kind == PanelKind::LiveHistory));
    assert!(ui
        .panel
        .as_ref()
        .is_some_and(|panel| panel.rows.iter().any(|row| row.value == "follow-up")));
}

#[test]
fn live_inspector_selection_focuses_historical_tool_without_interrupting() {
    let mut ui = Ui::default();
    ui.push_tool(
        ToolBlock::from_lines(vec![
            ("read_file".into(), Color::Cyan),
            ("file contents".into(), Color::Gray),
        ])
        .expect("tool"),
    );
    ui.push_chunk(provider::StreamChunk::Answer("answer".into()));
    ui.busy = true;

    assert!(ui.open_live_history());
    {
        let panel = ui.panel.as_mut().expect("live inspector");
        assert!(panel
            .selected()
            .is_some_and(|row| row.key.contains("Answer")));
        panel.move_down();
        assert_eq!(
            panel.selected_action(),
            PanelRowAction::FocusLiveBlock(LiveBlockFocus::Tool(0))
        );
    }
    ui.sync_live_panel_focus();
    assert_eq!(ui.transcript.focused_tool_summary(), Some("read_file"));
    assert!(ui.toggle_live_panel_detail());
    assert!(ui
        .transcript
        .visible_lines(5)
        .iter()
        .any(|line| line.text == "file contents"));
    assert!(ui.busy, "Inspector focus must not interrupt the model task");
}

#[test]
fn live_inspector_search_expands_matching_folded_tool() {
    let mut ui = Ui::default();
    ui.push_tool(
        ToolBlock::from_lines(vec![
            ("read_file".into(), Color::Cyan),
            ("needle hidden in folded output".into(), Color::Gray),
        ])
        .expect("tool"),
    );

    assert!(ui.open_live_history());
    {
        let panel = ui.panel.as_mut().expect("live inspector");
        panel.query = "needle".into();
        panel.retype();
        assert!(panel.detail_open, "detail search should open its preview");
        assert_eq!(
            panel.selected_action(),
            PanelRowAction::FocusLiveBlock(LiveBlockFocus::Tool(0))
        );
    }

    // Searching a folded detail must make the same match visible in the live
    // projection; users should not have to close the Inspector and expand it
    // again by hand.
    ui.sync_live_panel_focus();
    assert_eq!(ui.transcript.focused_block(), Some(LiveBlockFocus::Tool(0)));
    assert!(ui
        .transcript
        .visible_lines(8)
        .iter()
        .any(|line| line.text == "needle hidden in folded output"));
}

#[test]
fn ctrl_f_live_search_is_non_blocking_and_penetrates_folded_detail() {
    let mut ui = Ui {
        busy: true,
        ..Ui::default()
    };
    ui.push_tool(
        ToolBlock::from_lines(vec![
            ("read_file".into(), Color::Cyan),
            ("needle from folded output".into(), Color::Gray),
        ])
        .expect("tool"),
    );
    ui.input.insert_str("keep this draft");

    assert!(ui.open_live_search("needle"));
    let panel = ui.panel.as_ref().expect("live search panel");
    assert_eq!(panel.kind, PanelKind::LiveHistory);
    assert_eq!(panel.query, "needle");
    assert!(panel.detail_open);
    assert_eq!(ui.input.buffer, "keep this draft");
    assert!(ui.busy, "live search must not interrupt the running task");
    assert!(ui
        .transcript
        .visible_lines(8)
        .iter()
        .any(|line| line.text == "needle from folded output"));
}

#[test]
fn live_inspector_exposes_pending_fifo_and_switches_attention_surfaces() {
    let mut ui = Ui::default();
    ui.push_chunk(provider::StreamChunk::Answer("answer".into()));
    ui.queued.push_back("first pending".into());
    ui.queued.push_back("second pending".into());

    assert!(ui.open_live_history());
    let panel = ui.panel.as_mut().expect("live inspector");
    assert!(panel
        .rows
        .iter()
        .any(|row| row.key.contains("pending · next")));
    panel.query = "second pending".into();
    panel.retype();
    assert_eq!(panel.selected_action(), PanelRowAction::RemoveQueued(1));

    assert_eq!(ui.remove_queued(1).as_deref(), Some("second pending"));
    ui.refresh_live_history_panel();
    assert!(!ui
        .panel
        .as_ref()
        .expect("live inspector after removal")
        .rows
        .iter()
        .any(|row| row.value == "second pending"));

    assert!(ui.toggle_queue_panel());
    assert_eq!(
        ui.panel.as_ref().map(|panel| panel.kind),
        Some(PanelKind::Queue)
    );
    assert!(ui.toggle_live_history());
    assert_eq!(
        ui.panel.as_ref().map(|panel| panel.kind),
        Some(PanelKind::LiveHistory)
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
        reasoning_history_panel(&std::collections::VecDeque::new()).title,
        answer_history_panel(&std::collections::VecDeque::new()).title,
        live_history_panel_with_queue(
            &LiveTranscript::default(),
            &std::collections::VecDeque::new(),
        )
        .title,
        models_panel_with_effort(&[], "", "", "medium").title,
        agent_panel(&[]).title,
    ];
    for t in &titles {
        assert!(!has_cjk(t), "panel 标题应为英文: {t}");
    }
}

#[test]
fn halt_labels_and_task_guards_are_deterministic() {
    let cases = [
        (HaltReason::Approved, "approved", "result verified"),
        (
            HaltReason::Budget,
            "budget limit",
            "inspect activity before starting a smaller task",
        ),
        (
            HaltReason::Stall,
            "no verified progress",
            "inspect reasoning/tools, then start the next task",
        ),
        (
            HaltReason::StepCap,
            "step limit",
            "inspect activity, then narrow the next task",
        ),
        (
            HaltReason::ConstraintBreach,
            "safety constraint",
            "inspect the blocked action and revise the request",
        ),
        (
            HaltReason::ContextRot,
            "context limit",
            "start the next task with a smaller context",
        ),
        (
            HaltReason::CircuitBroken,
            "repeated tool errors",
            "inspect the last tool error before retrying",
        ),
        (
            HaltReason::Unverified,
            "not verified",
            "inspect the activity log before retrying",
        ),
    ];
    for (reason, label, guidance) in cases {
        assert_eq!(halt_reason_display(reason), label);
        assert_eq!(halt_reason_guidance(reason), guidance);
    }
    assert_eq!(inline_height_cap(), 14);
    assert!(superstep_is_busy(&["reason".into()]));
    assert!(!superstep_is_busy(&[]));
    assert!(can_start_task(false, false));
    assert!(!can_start_task(true, false));
    assert!(!can_start_task(false, true));
}

#[test]
fn unfinished_answers_and_attention_fallbacks_remain_visible() {
    let mut unfinished = AgentState::new("task");
    assert_eq!(
        unfinished_answer_reason(&Ok(unfinished.clone())),
        Some("run stopped before final response")
    );
    assert_eq!(
        unfinished_answer_reason(&Err("failed".into())),
        Some("run ended before final response")
    );
    unfinished.messages.push("(final) done".into());
    assert_eq!(unfinished_answer_reason(&Ok(unfinished)), None);

    let mut ui = Ui::default();
    mark_takeover_requested(&mut ui);
    apply_attention_action(&mut ui, InputAction::ToggleDetails);
    assert!(!ui.commits.is_empty());
    apply_attention_action(&mut ui, InputAction::ToggleActivity);
    assert!(!ui.activity_history.is_empty());
}

#[test]
fn live_history_is_a_full_frame_transcript_audit_surface() {
    let area = Rect {
        x: 2,
        y: 3,
        width: 96,
        height: 24,
    };
    assert_eq!(panel_rect_for_kind(area, PanelKind::LiveHistory), area);
    let modal = panel_rect_for_kind(area, PanelKind::Activity);
    assert!(modal.width < area.width);
    assert!(modal.height < area.height);
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

#[test]
fn long_live_frames_are_bounded_and_profiled() {
    let mut ui = Ui::default();
    for index in 0..64 {
        ui.push_tool(
            ToolBlock::from_lines(vec![
                (format!("tool-{index}"), Color::Cyan),
                ("bounded detail".into(), Color::Gray),
            ])
            .expect("tool block"),
        );
    }
    let answer = (0..500)
        .map(|index| format!("answer line {index} · let value = {index};"))
        .collect::<Vec<_>>()
        .join("\n");
    ui.push_chunk(provider::StreamChunk::Answer(answer));
    let meta = ReplMeta {
        tools: Vec::new(),
        provider: "test".into(),
        provider_label: "test".into(),
        model: "model".into(),
        base_url: String::new(),
        status_bar: "{provider} · {model} · {tokens}".into(),
        ctx_window: 200_000,
    };
    let vitals = Vitals {
        step: 3,
        elapsed_s: 4,
        task_tokens: 500,
        rate: 120,
        ctx_used: 2_000,
        queued: 2,
    };

    let mut cache = LiveOutputCache::default();
    for width in [18, 40, 80] {
        let mut terminal =
            Terminal::new(ratatui::backend::TestBackend::new(width, 24)).expect("profile terminal");
        let started = std::time::Instant::now();
        for _ in 0..100 {
            terminal
                .draw(|frame| draw_with_cache(frame, &ui, &meta, 500, &vitals, None, &mut cache))
                .expect("profile draw");
        }
        let elapsed = started.elapsed();
        eprintln!(
            "long_live_frames width={width} draws=100 elapsed_ms={}",
            elapsed.as_millis()
        );
        assert!(
            ui.transcript.visible_lines(8).len() <= 8,
            "visible live tail must remain bounded at width {width}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "bounded draw profile regressed at width {width}: {elapsed:?}"
        );
    }
    assert_eq!(cache.rebuilds(), 3, "each viewport width should build once");

    let mut long_line_ui = Ui::default();
    long_line_ui.push_chunk(provider::StreamChunk::Answer("x".repeat(32_768)));
    let mut long_line_terminal =
        Terminal::new(ratatui::backend::TestBackend::new(40, 24)).expect("long line terminal");
    let mut long_line_cache = LiveOutputCache::default();
    let started = std::time::Instant::now();
    for _ in 0..100 {
        long_line_terminal
            .draw(|frame| {
                draw_with_cache(
                    frame,
                    &long_line_ui,
                    &meta,
                    500,
                    &vitals,
                    None,
                    &mut long_line_cache,
                )
            })
            .expect("long line draw");
    }
    let elapsed = started.elapsed();
    let symbols = long_line_terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        symbols.contains('…'),
        "long live line should expose tail marker"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "unbroken long-line draw regressed: {elapsed:?}"
    );
    assert_eq!(long_line_cache.rebuilds(), 1);
}

#[test]
fn presentation_anchor_survives_live_history_and_static_projection() {
    let mut ui = Ui::default();
    ui.push_chunk(provider::StreamChunk::Reasoning("plan".into()));
    let reasoning_id = match ui.transcript.inspector_rows()[0].focus {
        LiveBlockFocus::Reasoning(id) => id,
        focus => panic!("unexpected focus: {focus:?}"),
    };
    ui.commit_live_reasoning(3, 2);

    assert_eq!(
        ui.reasoning_history.back().expect("reasoning archive").id,
        reasoning_id
    );
    assert!(ui.presentation.records().iter().any(|record| {
        record.id == reasoning_id
            && record.channel == PresentationChannel::Reasoning
            && record.status == PresentationStatus::Committed
    }));
    assert!(ui.commits.iter().any(|block| {
        matches!(
            block,
            CommitBlock::Reasoning { id, .. } if *id == reasoning_id
        )
    }));

    ui.push_tool(ToolBlock::from_lines(vec![("read_file".into(), Color::Cyan)]).expect("tool"));
    let tool_id = match ui.transcript.inspector_rows()[0].focus {
        LiveBlockFocus::Tool(id) => id,
        focus => panic!("unexpected focus: {focus:?}"),
    };
    ui.commit_live_tools();
    assert_eq!(
        ui.tool_history
            .back()
            .expect("tool archive")
            .presentation_id(),
        tool_id
    );
    assert!(ui.presentation.records().iter().any(|record| {
        record.id == tool_id
            && record.channel == PresentationChannel::Tool
            && record.status == PresentationStatus::Committed
    }));

    ui.note_markdown_with_meta("final answer", 4, 3, 9);
    let answer_id = ui.answer_history.back().expect("answer archive").id;
    assert!(ui
        .commits
        .iter()
        .any(|block| { matches!(block, CommitBlock::Markdown { id, .. } if *id == answer_id) }));
    assert!(ui.presentation.records().iter().any(|record| {
        record.id == answer_id
            && record.channel == PresentationChannel::Answer
            && record.status == PresentationStatus::Committed
    }));
}

#[test]
fn presentation_ledger_is_bounded_and_keeps_channel_identity() {
    let mut ui = Ui::default();
    for index in 0..(MAX_PRESENTATION_RECORDS + 8) {
        ui.note_markdown(format!("answer {index}"));
    }
    assert_eq!(ui.presentation.records().len(), MAX_PRESENTATION_RECORDS);
    assert!(ui
        .presentation
        .records()
        .iter()
        .all(|record| record.channel == PresentationChannel::Answer));
    assert!(ui
        .presentation
        .records()
        .iter()
        .all(|record| record.status == PresentationStatus::Committed));
}

#[test]
fn activity_anchor_preserves_semantic_continuation_rail_when_wrapped() {
    let width = 40;
    let mut ui = Ui::default();
    ui.set_activity("waiting for the background model response while keeping takeover visible");
    let mut terminal = Terminal::with_options(
        ratatui::backend::TestBackend::new(width, 30),
        TerminalOptions {
            viewport: Viewport::Inline(20),
        },
    )
    .expect("activity wrapping terminal");
    flush_commits(&mut terminal, &mut ui).expect("activity wrapping scrollback");
    let rows = terminal
        .backend()
        .buffer()
        .content()
        .chunks(width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .filter(|row| !row.is_empty())
        .collect::<Vec<_>>();

    assert!(rows.len() > 1, "activity should wrap at {width}: {rows:?}");
    assert!(
        rows[0].starts_with("⟦WAIT #1⟧"),
        "missing activity tag: {rows:?}"
    );
    assert!(
        rows.iter().all(|row| str_cells(row) <= width as usize),
        "activity row exceeded width: {rows:?}"
    );
    assert!(
        rows.iter().skip(1).all(|row| row.starts_with("│ ")),
        "wrapped activity rows lost continuation rail: {rows:?}"
    );

    assert!(
        rows.iter().any(|row| row.contains("model response")),
        "activity split an ordinary word: {rows:?}"
    );
}

#[test]
fn activity_anchor_keeps_rail_across_explicit_detail_lines() {
    let mut ui = Ui::default();
    ui.set_activity("waiting for the model\nsecond detail remains actionable");
    let mut terminal = Terminal::with_options(
        ratatui::backend::TestBackend::new(40, 30),
        TerminalOptions {
            viewport: Viewport::Inline(20),
        },
    )
    .expect("multiline activity terminal");
    flush_commits(&mut terminal, &mut ui).expect("multiline activity scrollback");
    let rows = terminal
        .backend()
        .buffer()
        .content()
        .chunks(40)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .filter(|row| !row.is_empty())
        .collect::<Vec<_>>();
    let detail = rows
        .iter()
        .find(|row| row.contains("second detail"))
        .expect("explicit activity detail");
    assert!(
        detail.starts_with("└ "),
        "explicit activity detail lost closing rail: {rows:?}"
    );
}
use std::collections::VecDeque;
use std::sync::Arc;

use agent::{est_tokens, AgentState, Config, HaltReason, Todo, PROVIDER_PRESETS};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Paragraph, Widget, Wrap},
    Terminal, TerminalOptions, Viewport,
};

use super::{
    active_reasoning_tail_role, activity_commit_lines, activity_panel, agent_panel,
    answer_commit_lines, answer_commit_lines_with_status,
    answer_commit_lines_with_status_and_metrics, answer_commit_measure, answer_history_panel,
    apply_attention_action, apply_completion, apply_paste, apply_scroll, approval_action,
    build_popup, can_start_task, char_cells, clip_display_cells, commit_height,
    compact_activity_item, compact_status_line, config_panel, context_pressure_role, ctx_percent,
    current_word, decide_key, detail_match_scroll, detail_scroll_position, draw, draw_panel,
    draw_with_cache, event_color, fence_language, fence_without_language, filter_prefix,
    flush_commits, fmt_busy_bar, fmt_busy_phase, fmt_busy_signal, fmt_ctx, fmt_progress_diagnostic,
    fmt_reasoning_meta, fold_lines, format_event_plain, halt_reason_display, halt_reason_guidance,
    inline_height_cap, input_action, input_chrome, input_height, is_final_event, is_second_ctrl_c,
    live_code_rail, live_empty_state_for_test, live_history_panel_with_queue,
    live_history_toggle_action, live_hold_release_action, live_hold_toggle_action,
    live_markdown_line, live_markdown_spans_with_alert, live_page_rows, live_phase_anchor,
    live_phase_marker, live_rail, live_scroll_action, live_semantic_toggle_action,
    live_surface_title, live_tool_rail_role, login_panel, mark_takeover_requested, markdown_lines,
    md_line_spans, models_panel_with_effort, multiline_shortcut_label, named_profile_name,
    panel_action, panel_attention_action, panel_enter, panel_filter, panel_hint,
    panel_rect_for_kind, panel_title_role, panel_viewport_range, pending_queue_lines, preset_by_id,
    preview_lines, provider_panel, queue_panel_toggle_action, reasoning_commit_lines,
    reasoning_history_panel, render_status_template, render_todo_block, responsive_live_layout,
    role_color, run_command, sanitize_display_text, sanitize_paste, selection_style,
    semantic_focus_action, should_draw, splash_block, splash_frame, status_line_projection,
    str_cells, stream_channel_badge, stream_tail, summarize_event, superstep_is_busy,
    tail_display_cells, telemetry_surface, terminal_event_action, todo_progress, token_rate,
    tool_detail_scroll_action, tool_focus_action, tool_history_panel, tool_preview, tools_panel,
    top_chrome, unfinished_answer_reason, up_fallback_is_home, wrap_commit_lines, wrap_input,
    wrap_live_spans, wrap_live_spans_tail, wrapped_rows, ActivityKind, ApprovalAction,
    ApprovalRequest, CommandCatalog, CommandStats, CommitBlock, DetailLayoutCache, InputAction,
    InputChromeArgs, InputState, LiveBlockFocus, LiveChannel, LiveFramePlan, LiveLineKind,
    LiveOutputCache, LiveScrollAction, LiveTranscript, Panel, PanelAction, PanelItemsCache,
    PanelKind, PanelRow, PanelRowAction, Popup, PresentationChannel, PresentationMetrics,
    PresentationStatus, Role, StatusVars, TerminalEventAction, ToolBlock, ToolPhase, Ui, Vitals,
    CHATGPT_MODEL_GROUP, CLAUDE_OAUTH_ROW, CODEX_OAUTH_ROW, MAX_ACTIVITY_HISTORY,
    MAX_ANSWER_HISTORY, MAX_ANSWER_HISTORY_CHARS, MAX_PENDING_PREVIEW_CHARS,
    MAX_PENDING_PREVIEW_ROWS, MAX_PRESENTATION_RECORDS, MAX_REASONING_HISTORY,
    MAX_REASONING_HISTORY_CHARS, MAX_TOOL_HISTORY, SLASH_COMMANDS, SPLASH, SPLASH_TICKS,
};
