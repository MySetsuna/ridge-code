//! RidgeCode 的交互式终端界面 —— **主屏内联 REPL**(iter-26)。
//! 不再霸占备用屏:历史内容经 `Terminal::insert_before` 静态提交进终端原生 scrollback
//! (原生滚动/选取/搜索全保留),ratatui 只渲染底部一小块 Live 视口(状态行 + 流式尾巴 + 输入框)。
//! 执行图跑在后台 Tokio task,token 流、工具事件和权限门都不会卡住界面(iter-23 事件驱动主环)。

use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::{
    event::{
        self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    backend::CrosstermBackend, layout::Rect, style::Color, Terminal, TerminalOptions, Viewport,
};

use agent::{
    active_run_dir_for, agent_run_config, build_llm_agent_full, est_tokens, expand_mentions,
    halt_reason, invoke_durable, mark_durable_cancelled, null_token_bus, preset_by_id, AgentState,
    Approver, AutoApprove, HaltReason, McpTools, Skill, TokenBus,
};
use langgraph::StreamEvent;
use provider::Message;

use crate::{
    load_global_input_history, load_session_input_history, node_label, save_global_input_history,
    save_session, save_session_input_history, session_path, DeviceOAuthEvent, ReplMeta,
};
use provider::SwapProvider;

type Term = Terminal<CrosstermBackend<io::Stdout>>;

pub(crate) type ModelCatalog = Vec<(String, Vec<provider::models::ModelInfo>)>;

/// Live 视口总高:状态行 1 + 流式尾巴 ≥5 + 输入框 3..=8。内联模式下 ratatui 只管这块,
/// 高度恒小于终端 —— 从根上杜绝「动态高度超视口触发全屏清屏」的闪烁根因。
const LIVE_HEIGHT: u16 = 14;
/// 单次 token 唤醒最多合并的 chunk；留出下一轮 select 处理键盘，保证 Ctrl-C 可抢占。
const MAX_STREAM_CHUNKS_PER_WAKE: usize = 256;

fn inline_height_cap() -> u16 {
    // Keep the viewport cap stable; ratatui clamps it to current terminal height
    // and can grow back to the cap after a small terminal is enlarged.
    LIVE_HEIGHT
}

/// Convert an execution halt into a short, user-facing presentation label.
/// The execution enum remains the source of truth; this never changes routing.
pub(crate) fn halt_reason_display(reason: HaltReason) -> &'static str {
    match reason {
        HaltReason::Approved => "approved",
        HaltReason::Budget => "budget limit",
        HaltReason::Stall => "no verified progress",
        HaltReason::StepCap => "step limit",
        HaltReason::ConstraintBreach => "safety constraint",
        HaltReason::ContextRot => "context limit",
        HaltReason::CircuitBroken => "repeated tool errors",
        HaltReason::Unverified => "not verified",
    }
}

/// Keep the recovery advice deterministic and bounded; do not infer hidden
/// reasoning or add a second execution state machine to the TUI.
pub(crate) fn halt_reason_guidance(reason: HaltReason) -> &'static str {
    match reason {
        HaltReason::Approved => "result verified",
        HaltReason::Budget => "inspect activity before starting a smaller task",
        HaltReason::Stall => "inspect reasoning/tools, then start the next task",
        HaltReason::StepCap => "inspect activity, then narrow the next task",
        HaltReason::ConstraintBreach => "inspect the blocked action and revise the request",
        HaltReason::ContextRot => "start the next task with a smaller context",
        HaltReason::CircuitBroken => "inspect the last tool error before retrying",
        HaltReason::Unverified => "inspect the activity log before retrying",
    }
}

/// Superstep 后仍有 frontier 即任务仍运行；空 frontier 才允许输入启动下一任务。
fn superstep_is_busy(active: &[String]) -> bool {
    !active.is_empty()
}

/// 仅在 UI 空闲且旧 task 已由 done 分支收走时启动新任务。
fn can_start_task(busy: bool, task_running: bool) -> bool {
    !busy && !task_running
}

fn mark_submit_dirty(had_pending_submit: bool, dirty: &mut bool) {
    if had_pending_submit {
        *dirty = true;
    }
}

/// Opt-in lifecycle trace for isolated terminal harnesses; normal TUI does no file I/O.
fn tui_trace(stage: &str) {
    let Some(path) = std::env::var_os("RIDGE_TUI_TRACE") else {
        return;
    };
    if let Some(parent) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        let _ = writeln!(file, "{stage}");
    }
}

/// Record the hand-off boundary before aborting the task.  The task is still
/// cancelled immediately; this extra presentation event lets Activity/trace
/// consumers distinguish a user takeover from a provider failure.
fn mark_takeover_requested(ui: &mut Ui) {
    ui.record_activity(
        ActivityKind::Takeover,
        "interrupting · cancelling current turn",
    );
}

/// Retain streamed answer text whenever the graph finishes without emitting
/// its explicit `(final)` event.  A non-error stop can still be unverified or
/// step-capped; clearing the live viewport in that case must not erase text
/// the user already saw.
fn unfinished_answer_reason(result: &Result<AgentState, String>) -> Option<&'static str> {
    match result {
        Err(_) => Some("run ended before final response"),
        Ok(out) if !out.messages.iter().any(|message| is_final_event(message)) => {
            Some("run stopped before final response")
        }
        Ok(_) => None,
    }
}

struct TerminalGuard {
    keyboard_enhancement_pushed: bool,
    mouse_capture_enabled: bool,
    base_mouse_capture_enabled: bool,
}

pub(crate) fn mouse_capture_requested(value: Option<&str>) -> bool {
    value == Some("1")
}

impl TerminalGuard {
    fn enter() -> anyhow::Result<(Self, Term)> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        // BPM best-effort(iter-24):旧 Windows conhost 不支持则静默退化为逐字粘贴,绝不阻 TUI 启动。
        let _ = execute!(stdout, event::EnableBracketedPaste);
        // Keep the terminal's native scrollback and text selection authoritative.
        // TUI mouse capture is opt-in for panel-only experiments; the default
        // must leave wheel/drag events to Windows Terminal/conhost/PTY.
        let mouse_capture_enabled =
            mouse_capture_requested(std::env::var("RIDGE_TUI_MOUSE_CAPTURE").ok().as_deref())
                && execute!(stdout, event::EnableMouseCapture).is_ok();
        // CSI u best-effort(iter-27):现代终端(Ghostty/WezTerm/iTerm2/kitty)得 Shift+Enter
        // 精确修饰键;不支持则静默降级(Alt+Enter / Ctrl+J 仍可换行)。同时请求
        // REPORT_EVENT_TYPES，让 Ctrl+Space 可实现按住审计、松开跟随；decide_key
        // 只放行这一语义键的 Release，其余 press/release 噪声仍去重。
        // ⚠ **仅非 Windows 推**:Windows Terminal 的 Kitty 键盘协议实现有缺陷 —— 开了它,**逐字打的空格键
        // 会被吞**(粘贴走 BracketedPaste 不受影响,故长任务粘贴照常);Windows 回落普通 WinAPI 键事件,
        // 空格正常,仅失 Shift+Enter 精确换行(Alt+Enter / Ctrl+J 仍可换行,损失可接受)。
        // Windows stays on the legacy WinAPI path by default; the opt-in
        // fixture flag lets a raw ConPTY harness send CSI-u Ctrl+Enter without
        // changing normal Windows Terminal compatibility.
        let keyboard_enhancement_pushed =
            if !cfg!(windows) || std::env::var("RIDGE_TUI_KITTY").ok().as_deref() == Some("1") {
                execute!(
                    stdout,
                    PushKeyboardEnhancementFlags(
                        KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                            | KeyboardEnhancementFlags::REPORT_EVENT_TYPES,
                    )
                )
                .is_ok()
            } else {
                false
            };
        set_precise_multiline_input_enabled(keyboard_enhancement_pushed);
        // 主屏内联视口(iter-26):不进备用屏,终端原生历史/选取/搜索神圣不可侵犯。
        let term = Terminal::with_options(
            CrosstermBackend::new(stdout),
            TerminalOptions {
                viewport: Viewport::Inline(inline_height_cap()),
            },
        )?;
        Ok((
            Self {
                keyboard_enhancement_pushed,
                mouse_capture_enabled,
                base_mouse_capture_enabled: mouse_capture_enabled,
            },
            term,
        ))
    }

    fn set_editor_mouse_capture(&mut self, enabled: bool) {
        if enabled {
            if !self.mouse_capture_enabled {
                self.mouse_capture_enabled =
                    execute!(io::stdout(), event::EnableMouseCapture).is_ok();
            }
        } else {
            if self.mouse_capture_enabled && !self.base_mouse_capture_enabled {
                let _ = execute!(io::stdout(), event::DisableMouseCapture);
                self.mouse_capture_enabled = false;
            }
        }
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // 与 enter 对称:仅在 KKP 命令确实写出后还原,避免误发 pop。
        if self.keyboard_enhancement_pushed {
            let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
        }
        if self.mouse_capture_enabled {
            let _ = execute!(io::stdout(), event::DisableMouseCapture);
        }
        let _ = execute!(io::stdout(), event::DisableBracketedPaste);
        let _ = disable_raw_mode();
    }
}

// === 子模块(iter-52 按职责拆分,均为 tui 私有)===
mod app;
mod clipboard;
mod command;
mod draw;
mod eventfmt;
mod input;
mod panel;
mod presentation;
mod render;
mod status;
#[cfg(test)]
mod tests;
mod transcript;

pub(crate) use app::*;
pub(crate) use clipboard::*;
pub(crate) use command::*;
pub(crate) use draw::*;
pub(crate) use eventfmt::*;
pub(crate) use input::*;
pub(crate) use panel::*;
pub(crate) use presentation::*;
pub(crate) use render::*;
pub(crate) use status::*;
pub(crate) use transcript::*;

/// Route attention shortcuts through one presentation-only fallback path.
/// Panel and editor contexts must tell the user when no matching history or
/// live block exists; the shortcut itself keeps its existing precedence.
fn apply_attention_action(ui: &mut Ui, action: InputAction) {
    let available = match action {
        InputAction::ToggleDetails => ui.toggle_details_or_history(),
        InputAction::ToggleReasoning => ui.toggle_reasoning_or_history(),
        InputAction::ToggleAnswer => ui.toggle_answer_or_history(),
        InputAction::ToggleActivity => {
            ui.toggle_activity_panel();
            true
        }
        _ => false,
    };
    if !available {
        let message = match action {
            InputAction::ToggleDetails => "no tool details or history",
            InputAction::ToggleReasoning => "no reasoning output or history",
            InputAction::ToggleAnswer => "no recoverable answer history",
            InputAction::ToggleActivity => return,
            _ => return,
        };
        ui.note(message, Color::Gray);
    }
}

enum KeyEventResult {
    Continue,
    Exit,
}

struct KeyEventContext<'a> {
    ui: &'a mut Ui,
    meta: &'a mut ReplMeta,
    swap: &'a Arc<SwapProvider>,
    bus: &'a TokenBus,
    pending: &'a mut Option<ApprovalRequest>,
    task: &'a mut Option<tokio::task::JoinHandle<()>>,
    task_started: &'a mut Option<Instant>,
    last_task: &'a Option<String>,
    retry_count: &'a mut usize,
    pending_submit: &'a mut Option<String>,
    momentary_hold: &'a mut bool,
    last_ctrl_c: &'a mut Option<Instant>,
    pressed: &'a mut std::collections::HashSet<KeyCode>,
    keylog_path: &'a Option<std::path::PathBuf>,
    guard: Option<&'a mut TerminalGuard>,
}

async fn handle_key_event(
    event: Event,
    context: &mut KeyEventContext<'_>,
) -> anyhow::Result<KeyEventResult> {
    log_key_event(&event, context.keylog_path);
    let key = match terminal_event_action(event) {
        TerminalEventAction::Paste(text) => {
            apply_paste(context.ui, &text);
            sync_input_editor_scroll(context.ui);
            return Ok(KeyEventResult::Continue);
        }
        TerminalEventAction::Mouse(mouse) => {
            handle_mouse_event(mouse, context);
            return Ok(KeyEventResult::Continue);
        }
        TerminalEventAction::Redraw => return Ok(KeyEventResult::Continue),
        TerminalEventAction::Key(key) => key,
    };
    let Some(key) = decide_key(context.pressed, &key) else {
        return Ok(KeyEventResult::Continue);
    };
    if live_hold_release_action(&key, context.ui.popup.is_some()) {
        if *context.momentary_hold {
            *context.momentary_hold = false;
            let _ = context.ui.follow_live();
        }
        return Ok(KeyEventResult::Continue);
    }
    if is_ctrl_c(&key) {
        return handle_ctrl_c(context);
    }
    if handle_approval_key(&key, context) {
        return Ok(KeyEventResult::Continue);
    }
    if context.ui.input_editor_scroll.is_some() {
        handle_input_editor_key(&key, context);
        return Ok(KeyEventResult::Continue);
    }
    if handle_global_key(&key, context) {
        return Ok(KeyEventResult::Continue);
    }
    if context.ui.panel.is_some() {
        handle_panel_key(&key, context).await?;
        return Ok(KeyEventResult::Continue);
    }
    if handle_live_key(&key, context) {
        return Ok(KeyEventResult::Continue);
    }
    handle_input_key(&key, context);
    Ok(KeyEventResult::Continue)
}

fn handle_mouse_event(mouse: crossterm::event::MouseEvent, context: &mut KeyEventContext<'_>) {
    match mouse_action(&mouse) {
        MouseAction::Scroll(delta) => handle_mouse_scroll(delta, context),
        MouseAction::Select { column, row } => select_panel_row_at(context.ui, column, row),
        MouseAction::Close => close_panel(context.ui),
        MouseAction::Ignore => {}
    }
}

fn handle_mouse_scroll(delta: i8, context: &mut KeyEventContext<'_>) {
    if let Some(scroll) = context.ui.input_editor_scroll.as_mut() {
        if delta > 0 {
            *scroll = scroll.saturating_sub(4);
        } else {
            *scroll = scroll.saturating_add(4);
        }
        return;
    }
    if let Some(panel) = context.ui.panel.as_mut() {
        scroll_panel(panel, delta);
        context.ui.sync_live_panel_focus();
    } else {
        let _ = context.ui.scroll_live(delta);
    }
}

fn scroll_panel(panel: &mut Panel, delta: i8) {
    if panel.detail_open {
        let _ = panel.scroll_detail(if delta > 0 { -1 } else { 1 });
    } else if delta > 0 {
        panel.move_up();
    } else {
        panel.move_down();
    }
}

fn select_panel_row_at(ui: &mut Ui, column: u16, row: u16) {
    let Ok((width, height)) = crossterm::terminal::size() else {
        return;
    };
    let Some(panel) = ui.panel.as_mut() else {
        return;
    };
    let rect = panel_rect_for_kind(
        Rect {
            x: 0,
            y: 0,
            width,
            height,
        },
        panel.kind,
    );
    let inner_height = rect.height.saturating_sub(2);
    let show_query = inner_height >= 3;
    let show_hint = inner_height >= 2;
    let right = rect.x.saturating_add(rect.width);
    let first_row = rect.y + 1 + u16::from(show_query);
    let last_row = rect
        .y
        .saturating_add(rect.height.saturating_sub(1 + u16::from(show_hint)));
    if column < rect.x
        || column >= right
        || row < first_row
        || row >= last_row
        || panel.view.is_empty()
    {
        return;
    }
    panel.sel = usize::from(row - first_row).min(panel.view.len() - 1);
    ui.sync_live_panel_focus();
}

fn log_key_event(event: &Event, keylog_path: &Option<std::path::PathBuf>) {
    let Some(path) = keylog_path else {
        return;
    };
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{event:?}");
    }
}

fn is_ctrl_c(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c' | 'C'))
}

fn handle_ctrl_c(context: &mut KeyEventContext<'_>) -> anyhow::Result<KeyEventResult> {
    let now = Instant::now();
    if is_second_ctrl_c(*context.last_ctrl_c, now) {
        return Ok(KeyEventResult::Exit);
    }
    *context.last_ctrl_c = Some(now);
    if let Some(handle) = context.task.take() {
        *context.momentary_hold = false;
        interrupt_task(context, handle, true);
    } else {
        context
            .ui
            .note("press Ctrl-C again within 2 seconds to exit", Color::Yellow);
    }
    Ok(KeyEventResult::Continue)
}

fn interrupt_task(
    context: &mut KeyEventContext<'_>,
    handle: tokio::task::JoinHandle<()>,
    clear_activity: bool,
) {
    mark_takeover_requested(context.ui);
    if let Some(task) = context.last_task {
        let _ = mark_durable_cancelled(active_run_dir_for(task), "cancelled by user takeover");
    }
    handle.abort();
    *context.bus.lock().unwrap() = None;
    context.ui.busy = false;
    context.ui.waiting = false;
    let elapsed = context
        .task_started
        .as_ref()
        .map(|started| started.elapsed().as_secs())
        .unwrap_or(0);
    context
        .ui
        .commit_live_reasoning(context.ui.superstep, elapsed);
    context.ui.commit_live_answers(
        "interrupted before final response",
        context.ui.superstep,
        elapsed,
    );
    context.ui.clear_streams();
    context.ui.commit_live_tools();
    context.ui.superstep = 0;
    context.ui.pending_call = None;
    if clear_activity {
        *context.task_started = None;
    }
    *context.retry_count = 0;
    *context.pending_submit = None;
    context.ui.mark_takeover_ready();
    let kept = context.ui.queued.len();
    let tail = if kept > 0 {
        format!("interrupted current task 路 takeover ready 路 {kept} queued kept")
    } else {
        "interrupted current task 路 takeover ready".into()
    };
    context.ui.note(tail, Color::Yellow);
}

fn handle_approval_key(key: &KeyEvent, context: &mut KeyEventContext<'_>) -> bool {
    let Some(pending) = context.pending.as_mut() else {
        return false;
    };
    match approval_action(key.code) {
        ApprovalAction::Approve => {
            if let Some(request) = context.pending.take() {
                if request.reply.send(true).is_ok() {
                    context.ui.resume_after_approval();
                }
            }
            context.ui.note("鉁?approved", Color::Green);
        }
        ApprovalAction::Reject => {
            if let Some(request) = context.pending.take() {
                if request.reply.send(false).is_ok() {
                    context.ui.resume_after_approval();
                }
            }
            context.ui.note("鉁?rejected", Color::Red);
        }
        ApprovalAction::Scroll(delta) => context.ui.scroll = apply_scroll(context.ui.scroll, delta),
        ApprovalAction::Ignore => {
            let _ = pending;
        }
    }
    true
}

fn handle_global_key(key: &KeyEvent, context: &mut KeyEventContext<'_>) -> bool {
    if queue_panel_toggle_action(key)
        && (context.ui.panel.is_none()
            || context
                .ui
                .panel
                .as_ref()
                .is_some_and(|panel| panel.allows_attention_switch()))
        && context.ui.popup.is_none()
    {
        context.ui.toggle_queue_panel();
        return true;
    }
    if live_history_toggle_action(
        key,
        context.ui.popup.is_some(),
        context.ui.transcript.has_history(),
    ) && (context.ui.panel.is_none()
        || context
            .ui
            .panel
            .as_ref()
            .is_some_and(|panel| panel.allows_attention_switch()))
    {
        context.ui.toggle_live_history();
        return true;
    }
    if let Some(action) = panel_attention_action(
        key,
        context
            .ui
            .panel
            .as_ref()
            .is_some_and(|panel| panel.allows_attention_switch()),
        context.ui.popup.is_some(),
    ) {
        apply_attention_action(context.ui, action);
        return true;
    }
    false
}

async fn handle_panel_key(key: &KeyEvent, context: &mut KeyEventContext<'_>) -> anyhow::Result<()> {
    if is_activity_toggle(key, context.ui) {
        context.ui.toggle_activity_panel();
        return Ok(());
    }
    match panel_action(key) {
        PanelAction::Esc => close_panel(context.ui),
        PanelAction::Remove => remove_panel_selection(context.ui, key.code == KeyCode::Backspace),
        PanelAction::Up
        | PanelAction::Down
        | PanelAction::DetailPageUp
        | PanelAction::DetailPageDown
        | PanelAction::PageUp
        | PanelAction::PageDown
        | PanelAction::First
        | PanelAction::Last => navigate_panel(context.ui, panel_action(key)),
        PanelAction::Backspace | PanelAction::Char(_) => edit_panel(context.ui, panel_action(key)),
        PanelAction::Enter => panel_enter_key(key, context).await?,
        PanelAction::Ignore => {}
    }
    Ok(())
}

fn is_activity_toggle(key: &KeyEvent, ui: &Ui) -> bool {
    key.kind == KeyEventKind::Press
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('t' | 'T'))
        && ui
            .panel
            .as_ref()
            .is_some_and(|panel| panel.kind == PanelKind::Activity)
}

fn close_panel(ui: &mut Ui) {
    let closing_kind = ui.panel.as_ref().map(|panel| panel.kind);
    let cancel_oauth = ui
        .panel
        .as_ref()
        .is_some_and(|panel| panel.editing.is_some() && panel.oauth_verifier.is_some());
    let cancel_device = ui.oauth_device.is_some() || ui.device_auth_status.is_some();
    let panel = ui.panel.as_mut().unwrap();
    if panel.editing.is_some() {
        panel.editing = None;
    } else {
        ui.panel = None;
    }
    if ui.panel.is_none() && matches!(closing_kind, Some(PanelKind::Models | PanelKind::Effort)) {
        ui.pending_model = None;
    }
    if cancel_oauth {
        ui.oauth_callback.take();
    }
    if cancel_device {
        ui.oauth_device.take();
        ui.device_auth_status = None;
    }
}

fn remove_panel_selection(ui: &mut Ui, allow_edit: bool) {
    let selection = ui.panel.as_ref().and_then(|panel| {
        if panel.editing.is_some() {
            return None;
        }
        match panel.selected_action() {
            PanelRowAction::RemoveQueued(index) => Some((index, true)),
            PanelRowAction::None if panel.kind == PanelKind::Queue => {
                panel.selected_index().map(|index| (index, false))
            }
            PanelRowAction::None | PanelRowAction::FocusLiveBlock(_) => None,
        }
    });
    if let Some((index, from_live_panel)) = selection {
        if let Some(message) = ui.remove_queued(index) {
            ui.record_activity(
                ActivityKind::Queue,
                format!("removed 路 {}", clip_display_cells(&message, 44)),
            );
            ui.note(
                format!(
                    "removed pending item ({} left): {}",
                    ui.queued.len(),
                    message
                ),
                role_color(Role::Warn),
            );
            if from_live_panel {
                ui.refresh_live_history_panel();
            } else {
                ui.refresh_queue_panel();
            }
        }
    } else if allow_edit {
        edit_panel(ui, PanelAction::Backspace);
    }
}

fn navigate_panel(ui: &mut Ui, action: PanelAction) {
    let panel = ui.panel.as_mut().unwrap();
    if panel.editing.is_some() {
        return;
    }
    let action = match action {
        // Once detail is open, PageUp/PageDown operate on the full-screen
        // document. Alt+PageUp/PageDown keep the same explicit path.
        PanelAction::PageUp if panel.detail_open => PanelAction::DetailPageUp,
        PanelAction::PageDown if panel.detail_open => PanelAction::DetailPageDown,
        action => action,
    };
    match action {
        PanelAction::Up => panel.move_up(),
        PanelAction::Down => panel.move_down(),
        PanelAction::DetailPageUp => {
            let _ = panel.scroll_detail(-1);
        }
        PanelAction::DetailPageDown => {
            let _ = panel.scroll_detail(1);
        }
        PanelAction::PageUp => panel.page_up(),
        PanelAction::PageDown => panel.page_down(),
        PanelAction::First => panel.first(),
        PanelAction::Last => panel.last(),
        _ => return,
    }
    ui.sync_live_panel_focus();
}

fn edit_panel(ui: &mut Ui, action: PanelAction) {
    let panel = ui.panel.as_mut().unwrap();
    match action {
        PanelAction::Backspace => match &mut panel.editing {
            Some(buffer) => {
                buffer.pop();
            }
            None => {
                panel.query.pop();
                panel.retype();
            }
        },
        PanelAction::Char(c) => {
            let toggle_live =
                panel.kind == PanelKind::LiveHistory && panel.editing.is_none() && c == ' ';
            if toggle_live {
                ui.toggle_live_panel_detail();
            } else {
                match &mut panel.editing {
                    Some(buffer) => buffer.push(c),
                    None => {
                        panel.query.push(c);
                        panel.retype();
                    }
                }
                ui.sync_live_panel_focus();
            }
        }
        _ => {}
    }
}

async fn panel_enter_key(key: &KeyEvent, context: &mut KeyEventContext<'_>) -> anyhow::Result<()> {
    let login_submit = matches!(
        context.ui.panel.as_ref(),
        Some(panel) if panel.kind == PanelKind::Login && panel.editing.is_some()
    );
    if !login_submit {
        panel_enter(context.ui, context.meta, context.swap);
        return Ok(());
    }
    let (id, key_text) = {
        let panel = context.ui.panel.as_ref().unwrap();
        (
            panel.selected().map(|row| row.key.clone()),
            panel.editing.clone().unwrap_or_default(),
        )
    };
    let oauth = match id.as_deref() {
        Some(id) if id == CLAUDE_OAUTH_ROW => Some(&provider::oauth::ANTHROPIC),
        Some(id) if id == CODEX_OAUTH_ROW => Some(&provider::oauth::OPENAI),
        _ => None,
    };
    if let Some(config) = oauth {
        apply_oauth_code(
            config,
            key_text.trim(),
            context.meta,
            context.swap,
            context.ui,
        )
        .await;
        return Ok(());
    }
    match id.as_deref().and_then(preset_by_id) {
        Some(preset) if !key_text.trim().is_empty() => {
            context
                .ui
                .note(format!("verifying {}…", preset.id), Color::Gray);
            login_apply_verified(
                preset,
                key_text.trim(),
                context.meta,
                context.swap,
                context.ui,
            )
            .await;
        }
        Some(_) => context.ui.note("enter a non-empty API key", Color::Yellow),
        None => context.ui.note("no provider selected", Color::Red),
    }
    let _ = key;
    Ok(())
}

fn handle_live_key(key: &KeyEvent, context: &mut KeyEventContext<'_>) -> bool {
    if handle_live_focus_key(key, context) || handle_live_view_key(key, context) {
        return true;
    }
    false
}

fn handle_live_focus_key(key: &KeyEvent, context: &mut KeyEventContext<'_>) -> bool {
    if let Some(delta) =
        tool_focus_action(key, context.ui.popup.is_some(), context.ui.has_live_tools())
    {
        let _ = context.ui.move_tool_focus(delta);
        return true;
    }
    if let Some(delta) = semantic_focus_action(
        key,
        context.ui.popup.is_some(),
        context.ui.transcript.is_inspecting(),
        context.ui.has_inspectable_live_output(),
    ) {
        let _ = context.ui.move_semantic_focus(delta);
        context
            .ui
            .note("Alt+鈫?鈫?路 semantic focus", role_color(Role::Info));
        return true;
    }
    if let Some(delta) = tool_detail_scroll_action(
        key,
        context.ui.popup.is_some(),
        context.ui.has_scrollable_live_tool(),
    ) {
        let _ = context.ui.scroll_tool_details(delta);
        return true;
    }
    false
}

fn handle_live_view_key(key: &KeyEvent, context: &mut KeyEventContext<'_>) -> bool {
    if live_hold_toggle_action(
        key,
        context.ui.popup.is_some(),
        context.ui.has_inspectable_live_output(),
    ) {
        if context.ui.transcript.is_inspecting() {
            *context.momentary_hold = false;
            let _ = context.ui.follow_live();
        } else {
            let _ = context.ui.hold_live();
            *context.momentary_hold = true;
        }
        return true;
    }
    if live_semantic_toggle_action(
        key,
        context.ui.popup.is_some(),
        context.ui.transcript.is_inspecting(),
        context.ui.has_live_tools() || context.ui.transcript.has_reasoning(),
    ) {
        let _ = context.ui.toggle_focused_semantic();
        context
            .ui
            .note("Space 路 semantic block toggled", role_color(Role::Info));
        return true;
    }
    let Some(action) = live_scroll_action(
        key,
        context.ui.popup.is_some(),
        context.ui.has_scrollable_live_tool(),
        context.ui.has_inspectable_live_output(),
    ) else {
        return false;
    };
    match action {
        LiveScrollAction::Older => {
            let _ = context.ui.scroll_live(1);
        }
        LiveScrollAction::Newer => {
            let _ = context.ui.scroll_live(-1);
        }
        LiveScrollAction::OlderPage => {
            let page_rows = crossterm::terminal::size()
                .map(|(_, height)| live_page_rows(height))
                .unwrap_or(12);
            let _ = context.ui.scroll_live_page(1, page_rows);
        }
        LiveScrollAction::NewerPage => {
            let page_rows = crossterm::terminal::size()
                .map(|(_, height)| live_page_rows(height))
                .unwrap_or(12);
            let _ = context.ui.scroll_live_page(-1, page_rows);
        }
        LiveScrollAction::Follow => {
            let _ = context.ui.follow_live();
        }
    }
    true
}

fn handle_input_key(key: &KeyEvent, context: &mut KeyEventContext<'_>) {
    let action = input_action(key, context.ui.busy, context.ui.popup.is_some());
    handle_input_action(action, context);
}

fn handle_input_editor_key(key: &KeyEvent, context: &mut KeyEventContext<'_>) {
    if input_editor_close_requested(key) {
        context.ui.input_editor_scroll = None;
        set_editor_mouse_capture(context, false);
        return;
    }
    if input_editor_paste_requested(key) {
        handle_input_action(InputAction::PasteClipboard, context);
        return;
    }
    if key.modifiers.is_empty() {
        match key.code {
            KeyCode::PageUp => {
                if let Some(scroll) = context.ui.input_editor_scroll.as_mut() {
                    *scroll = scroll.saturating_sub(8);
                }
                return;
            }
            KeyCode::PageDown => {
                if let Some(scroll) = context.ui.input_editor_scroll.as_mut() {
                    *scroll = scroll.saturating_add(8);
                }
                return;
            }
            _ => {}
        }
    }
    let action = match key.code {
        KeyCode::Enter => InputAction::NewLine,
        KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) && c == 'j' => {
            InputAction::NewLine
        }
        KeyCode::Char(c) => InputAction::Insert(c),
        KeyCode::Backspace => InputAction::Backspace,
        KeyCode::Delete => InputAction::Delete,
        KeyCode::Left => InputAction::Left,
        KeyCode::Right => InputAction::Right,
        KeyCode::Home => InputAction::Home,
        KeyCode::End => InputAction::End,
        KeyCode::Up => {
            let _ = context.ui.input.move_up();
            sync_input_editor_scroll(context.ui);
            return;
        }
        KeyCode::Down => {
            let _ = context.ui.input.move_down();
            sync_input_editor_scroll(context.ui);
            return;
        }
        _ => return,
    };
    edit_input(action, context.ui);
    sync_input_editor_scroll(context.ui);
}

fn input_editor_close_requested(key: &KeyEvent) -> bool {
    key.code == KeyCode::Esc
        || (key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('e' | 'E')))
}

fn input_editor_paste_requested(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('v' | 'V'))
}

fn handle_input_action(action: InputAction, context: &mut KeyEventContext<'_>) {
    match action {
        InputAction::Interrupt => {
            if let Some(handle) = context.task.take() {
                *context.momentary_hold = false;
                interrupt_task(context, handle, false);
            }
        }
        InputAction::Insert(_)
        | InputAction::Backspace
        | InputAction::Delete
        | InputAction::Left
        | InputAction::Right
        | InputAction::Home
        | InputAction::End
        | InputAction::NewLine
        | InputAction::CursorUpOrHistory
        | InputAction::CursorDownOrHistory => edit_input(action, context.ui),
        InputAction::PopupOpen
        | InputAction::PopupNext
        | InputAction::PopupPrev
        | InputAction::PopupAccept
        | InputAction::PopupSubmit
        | InputAction::PopupClose => handle_popup_action(action, context),
        InputAction::Submit | InputAction::Queue | InputAction::PushNow => {
            handle_submission_action(action, context.ui, context.pending_submit)
        }
        InputAction::ToggleDetails
        | InputAction::ToggleReasoning
        | InputAction::ToggleAnswer
        | InputAction::ToggleActivity => apply_attention_action(context.ui, action),
        InputAction::OpenLiveSearch => {
            if !context.ui.open_live_search("") {
                context.ui.note("no live blocks to search", Color::Gray);
            }
        }
        InputAction::OpenInputEditor => {
            if context.ui.input.is_long() {
                context.ui.popup = None;
                context.ui.input_editor_scroll = Some(0);
                set_editor_mouse_capture(context, true);
                sync_input_editor_scroll(context.ui);
            } else {
                context.ui.note(
                    "fullscreen editor opens for long or multiline input",
                    Color::Gray,
                );
            }
        }
        InputAction::PasteClipboard => match read_clipboard() {
            Ok(paste) => apply_clipboard_paste(context.ui, paste),
            Err(error) => context.ui.note(
                format!("clipboard paste unavailable: {error}"),
                Color::Yellow,
            ),
        },
        InputAction::Ignore => {}
    }
}

fn apply_clipboard_paste(ui: &mut Ui, paste: ClipboardPaste) {
    match paste {
        ClipboardPaste::Text(text) => {
            apply_paste(ui, &text);
            sync_input_editor_scroll(ui);
        }
        ClipboardPaste::Image { placeholder, path } => {
            ui.popup = None;
            ui.input.insert_str(&placeholder);
            ui.note(
                format!("pasted {placeholder} · saved {}", path.display()),
                Color::Gray,
            );
            sync_input_editor_scroll(ui);
        }
    }
}

fn set_editor_mouse_capture(context: &mut KeyEventContext<'_>, enabled: bool) {
    if let Some(guard) = context.guard.as_deref_mut() {
        guard.set_editor_mouse_capture(enabled);
    }
}

fn edit_input(action: InputAction, ui: &mut Ui) {
    match action {
        InputAction::Insert(c) => {
            ui.input.insert(c);
            ui.popup = build_popup(&ui.input);
        }
        InputAction::Backspace => {
            ui.input.backspace();
            ui.popup = build_popup(&ui.input);
        }
        InputAction::Delete => {
            ui.input.delete();
            ui.popup = build_popup(&ui.input);
        }
        InputAction::Left => ui.input.left(),
        InputAction::Right => ui.input.right(),
        InputAction::Home => ui.input.home(),
        InputAction::End => ui.input.end(),
        InputAction::NewLine => ui.input.insert('\n'),
        InputAction::CursorUpOrHistory => {
            if !ui.input.move_up() {
                let width = crossterm::terminal::size()
                    .map(|(columns, _)| columns)
                    .unwrap_or(80)
                    .saturating_sub(2);
                if up_fallback_is_home(&ui.input.buffer, ui.input.cursor, width) {
                    ui.input.home();
                } else {
                    ui.input.recall_prev();
                }
            }
        }
        InputAction::CursorDownOrHistory => cursor_down_or_history(ui),
        _ => {}
    }
}

fn sync_input_editor_scroll(ui: &mut Ui) {
    let Some(scroll) = ui.input_editor_scroll.as_mut() else {
        return;
    };
    let (width, height) = crossterm::terminal::size().unwrap_or((80, 24));
    let (_, cursor_row, _) = wrap_input(&ui.input.buffer, ui.input.cursor, width.saturating_sub(2));
    let visible = height.saturating_sub(2).max(1);
    if cursor_row < *scroll {
        *scroll = cursor_row;
    } else if cursor_row >= scroll.saturating_add(visible) {
        *scroll = cursor_row.saturating_sub(visible.saturating_sub(1));
    }
}

fn cursor_down_or_history(ui: &mut Ui) {
    if ui.input.move_down() {
        return;
    }
    ui.input.recall_next();
}

fn handle_popup_action(action: InputAction, context: &mut KeyEventContext<'_>) {
    match action {
        InputAction::PopupOpen => context.ui.popup = build_popup(&context.ui.input),
        InputAction::PopupNext => {
            if let Some(popup) = &mut context.ui.popup {
                popup.selected = (popup.selected + 1) % popup.items.len();
            }
        }
        InputAction::PopupPrev => {
            if let Some(popup) = &mut context.ui.popup {
                popup.selected = (popup.selected + popup.items.len() - 1) % popup.items.len();
            }
        }
        InputAction::PopupAccept => {
            if let Some(popup) = context.ui.popup.take() {
                apply_completion(&mut context.ui.input, &popup);
            }
        }
        InputAction::PopupSubmit => {
            if let Some(popup) = context.ui.popup.take() {
                apply_completion(&mut context.ui.input, &popup);
            }
            let input = context.ui.input.take().trim().to_owned();
            if !input.is_empty() {
                submit_or_queue_input(input, context);
            }
        }
        InputAction::PopupClose => context.ui.popup = None,
        _ => {}
    }
}

fn submit_or_queue_input(input: String, context: &mut KeyEventContext<'_>) {
    if context.ui.busy {
        queue_input(context.ui, input);
    } else {
        *context.pending_submit = Some(input);
    }
}

fn handle_submission_action(action: InputAction, ui: &mut Ui, pending_submit: &mut Option<String>) {
    let input = ui.input.take().trim().to_owned();
    if input.is_empty() {
        return;
    }
    match action {
        InputAction::Submit => *pending_submit = Some(input),
        InputAction::Queue => queue_input(ui, input),
        InputAction::PushNow => push_queue_front(ui, input),
        _ => {}
    }
}

fn queue_input(ui: &mut Ui, input: String) {
    ui.queued.push_back(input.clone());
    ui.refresh_queue_panel();
    ui.record_activity(
        ActivityKind::Queue,
        format!("queued 路 {}", clip_display_cells(&input, 48)),
    );
    ui.note(
        format!(
            "鈴?queued ({} pending; current turn continues): {input}",
            ui.queued.len()
        ),
        role_color(Role::Muted),
    );
}

fn push_queue_front(ui: &mut Ui, input: String) {
    ui.queued.push_front(input.clone());
    ui.refresh_queue_panel();
    ui.record_activity(
        ActivityKind::Queue,
        format!("front-queued 路 {}", clip_display_cells(&input, 44)),
    );
    ui.note(
        format!(
            "鈴?front-queued ({} pending; current turn continues): {input}",
            ui.queued.len()
        ),
        role_color(Role::Primary),
    );
}

const TUI_MAX_RETRIES: usize = 10;
type StartTask = Box<dyn Fn(&str, &[Message]) -> tokio::task::JoinHandle<()>>;

fn tui_approver(
    skip_danger: bool,
    tx: tokio::sync::mpsc::UnboundedSender<ApprovalRequest>,
) -> Arc<dyn Approver> {
    if skip_danger {
        Arc::new(AutoApprove)
    } else {
        Arc::new(TuiApprover { tx })
    }
}

fn session_input_history(history: &[Message]) -> Vec<String> {
    if history.is_empty() {
        return load_global_input_history();
    }
    let saved = load_session_input_history();
    if !saved.is_empty() {
        return saved;
    }
    history
        .iter()
        .filter_map(|message| match message.role {
            provider::Role::User => Some(message.content.clone()),
            _ => None,
        })
        .collect()
}

fn note_initial_ui(ui: &mut Ui, skip_danger: bool, history: &[Message]) {
    ui.note(
        "RidgeCode  路  inline mode: output lands in terminal history (native scroll/select) 路 Enter send/queue 路 Ctrl+Enter front-queue without interrupt 路 Ctrl+I/Alt+I live inspect 路 Ctrl+Q queue 路 Ctrl+Space hold/follow 路 Ctrl+A answers 路 Ctrl+T activity 路 Ctrl+J newline 路 Esc/Ctrl-C takeover; press Ctrl-C twice to exit 路 /help",
        Color::Cyan,
    );
    if skip_danger {
        ui.note(
            "鈿?skip-danger: tools auto-approved (disaster commands still hard-blocked)",
            Color::Red,
        );
    }
    if !history.is_empty() {
        ui.note(
            format!("restored {} session messages", history.len()),
            Color::Green,
        );
    }
}

fn keylog_path() -> Option<std::path::PathBuf> {
    let dir = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|home| std::path::PathBuf::from(home).join(".ridge"))
        .unwrap_or_else(std::env::temp_dir);
    let enabled = std::env::var_os("RIDGE_KEYLOG").is_some() || dir.join("keylog.on").exists();
    enabled.then(|| dir.join("keylog.txt"))
}

fn spawn_key_reader() -> tokio::sync::mpsc::UnboundedReceiver<Event> {
    let (key_tx, key_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    std::thread::spawn(move || {
        while let Ok(event) = event::read() {
            if key_tx.send(event).is_err() {
                break;
            }
        }
    });
    key_rx
}

fn poll_model_catalog(
    ui: &mut Ui,
    meta: &mut ReplMeta,
    swap: &Arc<SwapProvider>,
    receiver: &mut Option<tokio::sync::oneshot::Receiver<(ModelCatalog, u32)>>,
) -> bool {
    if ui.model_catalog_reload {
        ui.model_catalog_reload = false;
        ui.model_catalog = None;
        *receiver = Some(start_model_catalog_preload(
            &meta.provider,
            &meta.base_url,
            &meta.model,
        ));
    }
    let result = match receiver.as_mut() {
        Some(receiver) => match receiver.try_recv() {
            Ok(result) => Some(result),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => Some((Vec::new(), 1)),
        },
        None => None,
    };
    let Some((grouped, failures)) = result else {
        return false;
    };
    *receiver = None;
    auto_select_chatgpt_model(&grouped, meta, swap, ui);
    let empty = grouped.is_empty();
    ui.model_catalog = Some(grouped);
    if empty && failures > 0 {
        ui.note(
            "model catalog unavailable; retry /model after checking credentials or network",
            Color::Yellow,
        );
    }
    true
}

fn poll_device_oauth(ui: &mut Ui) -> Option<DeviceOAuthEvent> {
    match ui.oauth_device.as_mut() {
        Some(flow) => match flow.receiver.try_recv() {
            Ok(event) => Some(event),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => None,
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => Some(
                DeviceOAuthEvent::Complete(Err("device OAuth task stopped".into())),
            ),
        },
        None => None,
    }
}

fn handle_device_oauth_event(
    event: DeviceOAuthEvent,
    ui: &mut Ui,
    meta: &mut ReplMeta,
    swap: &Arc<SwapProvider>,
) {
    match event {
        DeviceOAuthEvent::Ready { user_code, opened } => {
            ui.device_auth_status = Some(format!("Device code: {user_code}"));
            ui.note(
                format!(
                    "Codex device auth: {} browser at {} and enter code: {user_code}",
                    if opened {
                        "browser opened; visit"
                    } else {
                        "open"
                    },
                    provider::oauth::OPENAI_DEVICE_VERIFICATION_URL
                ),
                Color::Cyan,
            );
        }
        DeviceOAuthEvent::Complete(result) => {
            ui.oauth_device.take();
            match result {
                Ok(token) => {
                    ui.device_auth_status = None;
                    apply_oauth_token(&provider::oauth::OPENAI, token, meta, swap, ui);
                }
                Err(error) => {
                    ui.device_auth_status = Some(format!("Device auth failed: {error}"));
                    ui.note(format!("Codex device OAuth failed: {error}"), Color::Red);
                }
            }
        }
    }
}

fn poll_oauth_callback(ui: &mut Ui) -> Option<Result<String, String>> {
    match ui.oauth_callback.as_mut() {
        Some(callback) => match callback.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                Some(Err("local OAuth callback listener stopped".into()))
            }
        },
        None => None,
    }
}

struct EventStepResult {
    exit: bool,
    dirty: bool,
}

struct EventStepContext<'a> {
    ui: &'a mut Ui,
    meta: &'a mut ReplMeta,
    swap: &'a Arc<SwapProvider>,
    bus: &'a TokenBus,
    pending: &'a mut Option<ApprovalRequest>,
    task: &'a mut Option<tokio::task::JoinHandle<()>>,
    task_started: &'a mut Option<Instant>,
    retry_count: &'a mut usize,
    pending_submit: &'a mut Option<String>,
    momentary_hold: &'a mut bool,
    last_ctrl_c: &'a mut Option<Instant>,
    pressed: &'a mut std::collections::HashSet<KeyCode>,
    keylog_path: &'a Option<std::path::PathBuf>,
    last_activity: &'a mut Option<Instant>,
    history: &'a mut Vec<Message>,
    printed: &'a mut usize,
    last_task: &'a Option<String>,
    session_tokens: &'a mut usize,
    session_turns: &'a mut usize,
    start_task: &'a StartTask,
    key_rx: &'a mut tokio::sync::mpsc::UnboundedReceiver<Event>,
    token_rx: &'a mut tokio::sync::mpsc::UnboundedReceiver<provider::StreamChunk>,
    event_rx: &'a mut tokio::sync::mpsc::UnboundedReceiver<StreamEvent<AgentState>>,
    approval_rx: &'a mut tokio::sync::mpsc::UnboundedReceiver<ApprovalRequest>,
    done_rx: &'a mut tokio::sync::mpsc::UnboundedReceiver<Result<AgentState, String>>,
    tick: &'a mut tokio::time::Interval,
    terminal: &'a mut Term,
    guard: Option<&'a mut TerminalGuard>,
    animation_due: &'a mut bool,
}

async fn run_event_step(context: EventStepContext<'_>) -> anyhow::Result<EventStepResult> {
    let EventStepContext {
        ui,
        meta,
        swap,
        bus,
        pending,
        task,
        task_started,
        retry_count,
        pending_submit,
        momentary_hold,
        last_ctrl_c,
        pressed,
        keylog_path,
        last_activity,
        history,
        printed,
        last_task,
        session_tokens,
        session_turns,
        start_task,
        key_rx,
        token_rx,
        event_rx,
        approval_rx,
        done_rx,
        tick,
        terminal,
        guard,
        animation_due,
    } = context;
    let dirty = tokio::select! {
        biased;
        Some(event) = key_rx.recv() => {
            let resized = matches!(&event, Event::Resize(_, _));
            if resized {
                // `draw` also autoresizes, but doing it at the event boundary
                // clears the inline viewport before the next frame and keeps
                // native scrollback aligned with the new terminal width.
                let _ = terminal.autoresize();
                sync_input_editor_scroll(ui);
                refresh_splash_for_width(ui, terminal);
            }
            let result = handle_key_event(
                event,
                &mut KeyEventContext {
                    ui,
                    meta,
                    swap,
                    bus,
                    pending,
                    task,
                    task_started,
                    last_task,
                    retry_count,
                    pending_submit,
                    momentary_hold,
                    last_ctrl_c,
                    pressed,
                    keylog_path,
                    guard,
                },
            ).await?;
            if matches!(result, KeyEventResult::Exit) {
                return Ok(EventStepResult { exit: true, dirty: true });
            }
            true
        }
        Some(chunk) = token_rx.recv() => {
            handle_token_chunk(chunk, ui, last_activity, token_rx);
            true
        }
        Some(event) = event_rx.recv() => {
            handle_stream_event(
                event,
                &mut StreamEventContext {
                    ui,
                    task_started: &*task_started,
                    last_activity,
                    printed,
                },
            );
            true
        }
        Some(request) = approval_rx.recv() => {
            *pending = Some(request);
            *momentary_hold = false;
            ui.scroll = 0;
            ui.busy = false;
            ui.waiting = false;
            ui.mark_approval_required();
            true
        }
        Some(result) = done_rx.recv() => {
            handle_done_result(
                result,
                &mut DoneEventContext {
                    ui,
                    history,
                    task,
                    pending_submit,
                    momentary_hold,
                    task_started,
                    last_activity,
                    printed,
                    retry_count,
                    last_task,
                    session_tokens,
                    session_turns,
                    start_task: start_task.as_ref(),
                },
            );
            true
        }
        _ = tick.tick() => {
            *animation_due = ui.busy && pending.is_none() && ui.panel.is_none();
            handle_tick(ui, &*last_activity, &*pending, terminal)
        }
        else => return Ok(EventStepResult { exit: true, dirty: false }),
    };
    Ok(EventStepResult { exit: false, dirty })
}

struct PendingSubmitContext<'a> {
    ui: &'a mut Ui,
    history: &'a mut Vec<Message>,
    meta: &'a mut ReplMeta,
    swap: &'a Arc<SwapProvider>,
    agents: &'a agent::Agents,
    commands: &'a [agent::SlashCommand],
    skills: &'a [Skill],
    session_tokens: usize,
    session_turns: usize,
    pending_submit: &'a mut Option<String>,
    retry_count: &'a mut usize,
    last_task: &'a mut Option<String>,
    task_started: &'a mut Option<Instant>,
    last_activity: &'a mut Option<Instant>,
    printed: &'a mut usize,
    task: &'a mut Option<tokio::task::JoinHandle<()>>,
    start_task: &'a StartTask,
}

async fn process_pending_submit(context: &mut PendingSubmitContext<'_>) -> anyhow::Result<bool> {
    let Some(input) = context.pending_submit.take() else {
        return Ok(false);
    };
    let command_catalog = CommandCatalog {
        agents: context.agents,
        commands: context.commands,
        skills: context.skills,
    };
    let should_exit = run_command(
        &input,
        context.ui,
        context.history,
        context.meta,
        context.swap,
        &command_catalog,
        CommandStats {
            tokens: context.session_tokens,
            turns: context.session_turns,
        },
    )
    .await?;
    let starts_session = !input.starts_with('/') || context.ui.run_task.is_some();
    if starts_session && !context.ui.input.session_mode {
        context.ui.input.drop_last_history_if(&input);
        save_global_input_history(&context.ui.input.history);
        context.ui.input.begin_session();
        context.ui.input.push_history(&input);
    } else if !context.ui.input.session_mode {
        save_global_input_history(&context.ui.input.history);
    }
    if context.ui.input.session_mode {
        save_session_input_history(&context.ui.input.history);
    }
    if should_exit {
        return Ok(true);
    }
    let task_input = if input.starts_with('/') {
        context.ui.run_task.take()
    } else {
        Some(input.clone())
    };
    if let Some(task_input) = task_input {
        context
            .ui
            .note(format!("鈥?{input}"), role_color(Role::Command));
        context
            .history
            .push(Message::user(expand_mentions(&task_input)));
        *context.last_task = Some(task_input.clone());
        *context.retry_count = 0;
        reset_task_ui(context.ui);
        *context.task_started = Some(Instant::now());
        *context.last_activity = *context.task_started;
        *context.printed = 0;
        *context.task = Some((context.start_task)(&task_input, context.history));
    }
    Ok(false)
}

fn reset_task_ui(ui: &mut Ui) {
    ui.busy = true;
    ui.waiting = false;
    ui.phase = "reasoning".into();
    ui.set_activity("starting task");
    ui.clear_streams();
    ui.stream_tokens = 0;
    ui.input_tokens = 0;
    ui.output_tokens = 0;
    ui.superstep = 0;
    ui.stall = 0;
    ui.err_streak = 0;
    ui.explore_streak = 0;
    ui.pending_call = None;
}

struct DrawFrameContext<'a> {
    terminal: &'a mut Term,
    ui: &'a mut Ui,
    meta: &'a ReplMeta,
    history: &'a [Message],
    session_tokens: usize,
    pending: Option<&'a ApprovalRequest>,
    task_started: Option<Instant>,
    live_cache: &'a mut LiveOutputCache,
}

fn draw_tui_frame(context: &mut DrawFrameContext<'_>) -> anyhow::Result<()> {
    tui_trace("draw.begin");
    context.ui.frame = context.ui.frame.wrapping_add(1);
    let elapsed_ms = context
        .task_started
        .map(|started| started.elapsed().as_millis())
        .unwrap_or(0);
    let ctx_used = context
        .history
        .iter()
        .map(|message| est_tokens(&message.content))
        .sum::<usize>();
    let vitals = Vitals {
        step: context.ui.superstep,
        elapsed_s: (elapsed_ms / 1000) as u64,
        task_tokens: context.ui.stream_tokens,
        rate: token_rate(context.ui.stream_tokens, elapsed_ms),
        ctx_used,
        queued: context.ui.queued.len(),
    };
    context.terminal.draw(|frame| {
        draw_with_cache(
            frame,
            context.ui,
            context.meta,
            context.session_tokens,
            &vitals,
            context.pending,
            context.live_cache,
        )
    })?;
    tui_trace("draw.end");
    Ok(())
}

struct LoopPrepareContext<'a> {
    ui: &'a mut Ui,
    meta: &'a mut ReplMeta,
    swap: &'a Arc<SwapProvider>,
    model_catalog_rx: &'a mut Option<tokio::sync::oneshot::Receiver<(ModelCatalog, u32)>>,
    pending_submit: &'a mut Option<String>,
    task: &'a mut Option<tokio::task::JoinHandle<()>>,
    history: &'a mut Vec<Message>,
    agents: &'a agent::Agents,
    commands: &'a [agent::SlashCommand],
    skills: &'a [Skill],
    session_tokens: usize,
    session_turns: usize,
    retry_count: &'a mut usize,
    last_task: &'a mut Option<String>,
    task_started: &'a mut Option<Instant>,
    last_activity: &'a mut Option<Instant>,
    printed: &'a mut usize,
    start_task: &'a StartTask,
    terminal: &'a mut Term,
    live_cache: &'a mut LiveOutputCache,
    pending: &'a Option<ApprovalRequest>,
    dirty: &'a mut bool,
    animation_due: &'a mut bool,
}

async fn prepare_loop(context: &mut LoopPrepareContext<'_>) -> anyhow::Result<bool> {
    if poll_model_catalog(
        context.ui,
        context.meta,
        context.swap,
        context.model_catalog_rx,
    ) {
        *context.dirty = true;
    }
    if let Some(event) = poll_device_oauth(context.ui) {
        handle_device_oauth_event(event, context.ui, context.meta, context.swap);
        *context.dirty = true;
    }
    if let Some(result) = poll_oauth_callback(context.ui) {
        context.ui.oauth_callback.take();
        match result {
            Ok(code) => {
                apply_oauth_code(
                    &provider::oauth::OPENAI,
                    &code,
                    context.meta,
                    context.swap,
                    context.ui,
                )
                .await;
            }
            Err(error) => context
                .ui
                .note(format!("OAuth callback failed: {error}"), Color::Red),
        }
        *context.dirty = true;
    }
    let had_pending_submit = context.pending_submit.is_some();
    if can_start_task(context.ui.busy, context.task.is_some())
        && process_pending_submit(&mut PendingSubmitContext {
            ui: context.ui,
            history: context.history,
            meta: context.meta,
            swap: context.swap,
            agents: context.agents,
            commands: context.commands,
            skills: context.skills,
            session_tokens: context.session_tokens,
            session_turns: context.session_turns,
            pending_submit: context.pending_submit,
            retry_count: context.retry_count,
            last_task: context.last_task,
            task_started: context.task_started,
            last_activity: context.last_activity,
            printed: context.printed,
            task: context.task,
            start_task: context.start_task,
        })
        .await?
    {
        return Ok(true);
    }
    mark_submit_dirty(had_pending_submit, context.dirty);
    if !context.ui.commits.is_empty() {
        flush_commits(context.terminal, context.ui)?;
        *context.dirty = true;
    }
    if should_draw(*context.dirty, *context.animation_due) {
        draw_tui_frame(&mut DrawFrameContext {
            terminal: context.terminal,
            ui: context.ui,
            meta: context.meta,
            history: context.history,
            session_tokens: context.session_tokens,
            pending: context.pending.as_ref(),
            task_started: *context.task_started,
            live_cache: context.live_cache,
        })?;
        *context.dirty = false;
        *context.animation_due = false;
    }
    Ok(false)
}

fn handle_token_chunk(
    chunk: provider::StreamChunk,
    ui: &mut Ui,
    last_activity: &mut Option<Instant>,
    token_rx: &mut tokio::sync::mpsc::UnboundedReceiver<provider::StreamChunk>,
) {
    ui.busy = true;
    ui.waiting = false;
    *last_activity = Some(Instant::now());
    set_token_activity(ui, &chunk);
    ui.push_chunk(chunk);
    for _ in 0..MAX_STREAM_CHUNKS_PER_WAKE {
        match next_token_chunk(token_rx, ui) {
            Some(chunk) => ui.push_chunk(chunk),
            None => break,
        }
    }
}

fn set_token_activity(ui: &mut Ui, chunk: &provider::StreamChunk) {
    ui.set_activity(match chunk {
        provider::StreamChunk::Answer(_) => "model 路 answering",
        provider::StreamChunk::Reasoning(_) => "model 路 thinking",
    });
}

fn next_token_chunk(
    token_rx: &mut tokio::sync::mpsc::UnboundedReceiver<provider::StreamChunk>,
    ui: &mut Ui,
) -> Option<provider::StreamChunk> {
    match token_rx.try_recv() {
        Ok(chunk) => {
            set_token_activity(ui, &chunk);
            Some(chunk)
        }
        Err(_) => None,
    }
}

struct StreamEventContext<'a> {
    ui: &'a mut Ui,
    task_started: &'a Option<Instant>,
    last_activity: &'a mut Option<Instant>,
    printed: &'a mut usize,
}

fn handle_stream_event(event: StreamEvent<AgentState>, context: &mut StreamEventContext<'_>) {
    context.ui.waiting = false;
    *context.last_activity = Some(Instant::now());
    match event {
        StreamEvent::NodeFinished { node, .. } => {
            context.ui.phase = node_label(&node);
            context
                .ui
                .set_activity(format!("node 路 {}", context.ui.phase));
            context.ui.busy = true;
        }
        StreamEvent::Superstep {
            step,
            active,
            state,
        } => handle_superstep_event(step, &active, state, context),
    }
}

fn handle_superstep_event(
    step: usize,
    active: &[String],
    state: AgentState,
    context: &mut StreamEventContext<'_>,
) {
    context.ui.superstep = step;
    context.ui.input_tokens = state.input_tokens;
    context.ui.output_tokens = state.output_tokens;
    context.ui.stall = state.stall;
    context.ui.err_streak = state.err_streak;
    context.ui.explore_streak = state.explore_streak;
    context.ui.pending_call = state.pending_call.clone();
    let active_label = active
        .iter()
        .map(|node| node_label(node))
        .collect::<Vec<_>>()
        .join(" + ");
    context.ui.phase = active_label.clone();
    context.ui.set_activity(superstep_activity(
        &active_label,
        state.pending_call.as_ref(),
    ));
    let answer_step = context.ui.superstep;
    let answer_elapsed_s = context
        .task_started
        .as_ref()
        .map(|started| started.elapsed().as_secs())
        .unwrap_or(0);
    let answer_tokens = context.ui.stream_tokens;
    let messages = visible_stream_messages(&state);
    for message in messages.iter().skip(*context.printed) {
        present_stream_message(
            message,
            context.ui,
            answer_step,
            answer_elapsed_s,
            answer_tokens,
        );
    }
    *context.printed = messages.len();
    record_todo_snapshot(context.ui, &state.todos);
    context.ui.todos = state.todos;
    context
        .ui
        .commit_live_reasoning(answer_step, answer_elapsed_s);
    context.ui.commit_live_tools();
    context.ui.clear_streams();
    context.ui.busy = superstep_is_busy(active);
}

fn visible_stream_messages(state: &AgentState) -> &[String] {
    if state.display_messages.len() == state.messages.len() {
        &state.display_messages
    } else {
        &state.messages
    }
}

fn superstep_activity(active_label: &str, pending_call: Option<&provider::ToolCall>) -> String {
    if let Some(call) = pending_call {
        format!("tool 路 {}", call.name)
    } else if active_label.is_empty() {
        "settling result".to_owned()
    } else {
        format!("next 路 {active_label}")
    }
}

fn present_stream_message(
    message: &str,
    ui: &mut Ui,
    answer_step: usize,
    answer_elapsed_s: u64,
    answer_tokens: usize,
) {
    if let Some(tool) = tool_preview(message) {
        ui.push_tool(tool);
        return;
    }
    let is_final = is_final_event(message);
    for (line, color) in summarize_event(message) {
        if is_final {
            ui.note_markdown_with_meta(line, answer_step, answer_elapsed_s, answer_tokens);
        } else {
            ui.note(line, color);
        }
    }
}

fn record_todo_snapshot(ui: &mut Ui, todos: &[agent::Todo]) {
    let todo_snapshot = render_todo_block(todos);
    if todo_snapshot != render_todo_block(&ui.todos) && !todos.is_empty() {
        ui.record_plan(todo_snapshot);
    }
}

struct DoneEventContext<'a> {
    ui: &'a mut Ui,
    history: &'a mut Vec<Message>,
    task: &'a mut Option<tokio::task::JoinHandle<()>>,
    pending_submit: &'a mut Option<String>,
    momentary_hold: &'a mut bool,
    task_started: &'a mut Option<Instant>,
    last_activity: &'a mut Option<Instant>,
    printed: &'a mut usize,
    retry_count: &'a mut usize,
    last_task: &'a Option<String>,
    session_tokens: &'a mut usize,
    session_turns: &'a mut usize,
    start_task: &'a dyn Fn(&str, &[Message]) -> tokio::task::JoinHandle<()>,
}

fn handle_done_result(result: Result<AgentState, String>, context: &mut DoneEventContext<'_>) {
    *context.task = None;
    *context.momentary_hold = false;
    context.ui.busy = false;
    context.ui.waiting = false;
    if let Some(reason) = unfinished_answer_reason(&result) {
        commit_done_answer(context, reason);
    }
    commit_done_streams(context);
    match result {
        Ok(output) => handle_successful_run(output, context),
        Err(error) => handle_failed_run(error, context),
    }
    if context.pending_submit.is_none() {
        *context.pending_submit = context.ui.queued.pop_front();
        context.ui.refresh_queue_panel();
    }
}

fn commit_done_answer(context: &mut DoneEventContext<'_>, reason: &str) {
    let elapsed = done_elapsed(context);
    context
        .ui
        .commit_live_answers(reason, context.ui.superstep, elapsed);
}

fn commit_done_streams(context: &mut DoneEventContext<'_>) {
    let elapsed = done_elapsed(context);
    context
        .ui
        .commit_live_reasoning(context.ui.superstep, elapsed);
    context.ui.clear_streams();
    context.ui.commit_live_tools();
    context.ui.superstep = 0;
    context.ui.pending_call = None;
    *context.task_started = None;
    *context.last_activity = None;
    *context.printed = 0;
}

fn done_elapsed(context: &DoneEventContext<'_>) -> u64 {
    context
        .task_started
        .as_ref()
        .map(|started| started.elapsed().as_secs())
        .unwrap_or(0)
}

fn handle_successful_run(output: AgentState, context: &mut DoneEventContext<'_>) {
    *context.retry_count = 0;
    *context.history = output.history.clone();
    save_session(&session_path(), context.history);
    context.ui.input_tokens = output.input_tokens;
    context.ui.output_tokens = output.output_tokens;
    *context.session_tokens += output.total_tokens;
    *context.session_turns += 1;
    context.ui.todos = output.todos.clone();
    let complete = output.approved && !agent::completion_blocked(&output);
    let reason = halt_reason(&output);
    context.ui.mark_task_outcome_with_reason(
        complete,
        (!complete).then_some(halt_reason_display(reason)),
    );
    let (status, color) = if complete {
        ("鉁?approved".to_string(), Color::Green)
    } else {
        (
            format!(
                "鉁?not approved ({}) 路 {}",
                halt_reason_display(reason),
                halt_reason_guidance(reason)
            ),
            Color::Red,
        )
    };
    context.ui.note(
        format!(
            "{status} 路 steps={} 路 tokens={}",
            output.steps, output.total_tokens
        ),
        color,
    );
    agent::fire_session_hooks(
        "stop",
        &format!("steps={} tokens={}", output.steps, output.total_tokens),
    );
}

fn handle_failed_run(error: String, context: &mut DoneEventContext<'_>) {
    let retryable = is_retryable_error(&error);
    if let Some(task_input) = context
        .last_task
        .clone()
        .filter(|_| retryable && *context.retry_count < TUI_MAX_RETRIES)
    {
        *context.retry_count += 1;
        let retry = *context.retry_count;
        context.ui.note(
            format!("鈫?Transient failure, retrying {retry}/{TUI_MAX_RETRIES} (last: {error})"),
            Color::Yellow,
        );
        context.ui.busy = true;
        context.ui.waiting = false;
        context.ui.phase = "reasoning".into();
        context
            .ui
            .set_activity(format!("retrying 路 reasoning {retry}/{TUI_MAX_RETRIES}"));
        context.ui.superstep = 0;
        context.ui.stall = 0;
        context.ui.err_streak = 0;
        context.ui.explore_streak = 0;
        context.ui.pending_call = None;
        *context.task_started = Some(Instant::now());
        *context.last_activity = *context.task_started;
        *context.task = Some((context.start_task)(&task_input, context.history));
        return;
    }
    let tail = if !retryable {
        format!("error (not retryable, stopped): {error}")
    } else if *context.retry_count >= TUI_MAX_RETRIES {
        format!("error (failed after {TUI_MAX_RETRIES} retries): {error}")
    } else {
        format!("error: {error}")
    };
    context.ui.note(tail, Color::Red);
    context.ui.mark_error();
    agent::fire_session_hooks("stop", "error");
    *context.retry_count = 0;
}

fn handle_tick(
    ui: &mut Ui,
    last_activity: &Option<Instant>,
    pending: &Option<ApprovalRequest>,
    terminal: &Term,
) -> bool {
    let was_waiting = ui.waiting;
    ui.waiting = ui.busy && last_activity.is_some_and(|at| at.elapsed() >= Duration::from_secs(8));
    if ui.waiting && !was_waiting {
        ui.record_activity(ActivityKind::Waiting, "waiting 路 no stream for 8s");
    }
    if ui.splash >= SPLASH_TICKS || ui.busy || pending.is_some() {
        return false;
    }
    ui.splash += 1;
    let width = terminal
        .size()
        .map(|size| size.width as usize)
        .unwrap_or(80);
    if ui.splash == SPLASH_TICKS {
        ui.note(splash_block(width).join("\n"), role_color(Role::Primary));
        ui.clear_streams();
    } else {
        ui.transcript.set_splash(indent(
            &splash_frame_for_width(ui.splash, SPLASH_TICKS, width),
            splash_pad(width),
        ));
    }
    true
}

fn refresh_splash_for_width(ui: &mut Ui, terminal: &Term) {
    if ui.splash >= SPLASH_TICKS || ui.busy {
        return;
    }
    let width = terminal
        .size()
        .map(|size| size.width as usize)
        .unwrap_or(80);
    ui.transcript.set_splash(indent(
        &splash_frame_for_width(ui.splash, SPLASH_TICKS, width),
        splash_pad(width),
    ));
}

struct TuiLoopContext {
    swap: Arc<SwapProvider>,
    skills: Vec<Skill>,
    agents: Arc<agent::Agents>,
    commands: Vec<agent::SlashCommand>,
    history: Vec<Message>,
    meta: ReplMeta,
    terminal: Term,
    guard: Option<TerminalGuard>,
    ui: Ui,
    live_cache: LiveOutputCache,
    model_catalog_rx: Option<tokio::sync::oneshot::Receiver<(ModelCatalog, u32)>>,
    approval_rx: tokio::sync::mpsc::UnboundedReceiver<ApprovalRequest>,
    event_rx: tokio::sync::mpsc::UnboundedReceiver<StreamEvent<AgentState>>,
    token_rx: tokio::sync::mpsc::UnboundedReceiver<provider::StreamChunk>,
    done_rx: tokio::sync::mpsc::UnboundedReceiver<Result<AgentState, String>>,
    key_rx: tokio::sync::mpsc::UnboundedReceiver<Event>,
    tick: tokio::time::Interval,
    bus: TokenBus,
    start_task: StartTask,
    keylog_path: Option<std::path::PathBuf>,
    pending: Option<ApprovalRequest>,
    task: Option<tokio::task::JoinHandle<()>>,
    session_tokens: usize,
    session_turns: usize,
    printed: usize,
    task_started: Option<Instant>,
    last_activity: Option<Instant>,
    pending_submit: Option<String>,
    retry_count: usize,
    last_task: Option<String>,
    pressed: std::collections::HashSet<KeyCode>,
    momentary_hold: bool,
    last_ctrl_c: Option<Instant>,
    dirty: bool,
    animation_due: bool,
}

async fn run_event_loop(context: TuiLoopContext) -> anyhow::Result<()> {
    let TuiLoopContext {
        swap,
        skills,
        agents,
        commands,
        mut history,
        mut meta,
        mut terminal,
        mut guard,
        mut ui,
        mut live_cache,
        mut model_catalog_rx,
        mut approval_rx,
        mut event_rx,
        mut token_rx,
        mut done_rx,
        mut key_rx,
        mut tick,
        bus,
        start_task,
        keylog_path,
        mut pending,
        mut task,
        mut session_tokens,
        mut session_turns,
        mut printed,
        mut task_started,
        mut last_activity,
        mut pending_submit,
        mut retry_count,
        mut last_task,
        mut pressed,
        mut momentary_hold,
        mut last_ctrl_c,
        mut dirty,
        mut animation_due,
    } = context;
    'main: loop {
        if prepare_loop(&mut LoopPrepareContext {
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
            session_tokens,
            session_turns,
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
        .await?
        {
            break 'main;
        }
        let step = run_event_step(EventStepContext {
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
            terminal: &mut terminal,
            guard: guard.as_mut(),
            animation_due: &mut animation_due,
        })
        .await?;
        if step.dirty {
            dirty = true;
        }
        if step.exit {
            break 'main;
        }
    }
    if let Some(handle) = task {
        handle.abort();
    }
    Ok(())
}

/// TUI 是交互入口；只有非 TTY 的自动化管道才会回落到 headless。
#[allow(clippy::too_many_arguments)]
pub(super) async fn run(
    swap: Arc<SwapProvider>,
    mcp: McpTools,
    skills: Vec<Skill>,
    skip_danger: bool,
    budget: usize,
    history: Vec<Message>,
    meta: ReplMeta,
    agents: Arc<agent::Agents>,
    read_only: bool,
    commands: Vec<agent::SlashCommand>,
    initial_effort: String,
) -> anyhow::Result<()> {
    tui_trace("run.enter");
    set_dynamic_commands(&commands); // 自定义/skill 命令名进补全源(iter-39)
    let (approval_tx, approval_rx) = tokio::sync::mpsc::unbounded_channel::<ApprovalRequest>();
    let approver = tui_approver(skip_danger, approval_tx);
    let bus = null_token_bus();
    let app = Arc::new(build_llm_agent_full(
        swap.clone(),
        mcp,
        approver,
        skills.clone(), // 留 skills 供 /skills 列出本会话已载技能
        bus.clone(),
        agents.clone(),
        read_only,
    )?);
    tui_trace("agent.ready");
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<StreamEvent<AgentState>>();
    let (token_tx, token_rx) = tokio::sync::mpsc::unbounded_channel::<provider::StreamChunk>();
    let (done_tx, done_rx) = tokio::sync::mpsc::unbounded_channel::<Result<AgentState, String>>();
    let (guard, terminal) = TerminalGuard::enter()?;
    tui_trace("terminal.ready");
    let live_cache = LiveOutputCache::default();
    let mut ui = Ui {
        effort: Some(initial_effort),
        ..Ui::default()
    };
    let commands_fixture = std::env::var("RIDGE_TUI_FIXTURE").ok().as_deref() == Some("commands");
    if commands_fixture {
        ui.model_catalog = Some(vec![
            (
                "Kimi".into(),
                vec![provider::models::ModelInfo {
                    id: "kimi-k2".into(),
                    context: Some(128_000),
                }],
            ),
            (
                "Zai".into(),
                vec![provider::models::ModelInfo {
                    id: "glm-4.5".into(),
                    context: None,
                }],
            ),
        ]);
        ui.note_markdown_with_meta("fixture full answer: COMPLETE BODY TAIL", 0, 0, 0);
    }
    let session_input_history = session_input_history(&history);
    ui.input
        .set_history(session_input_history, !history.is_empty());
    let model_catalog_rx = (!commands_fixture)
        .then(|| start_model_catalog_preload(&meta.provider, &meta.base_url, &meta.model));
    note_initial_ui(&mut ui, skip_danger, &history);
    let pending: Option<ApprovalRequest> = None;
    let task: Option<tokio::task::JoinHandle<()>> = None;
    let session_tokens = 0usize;
    let session_turns = 0usize;
    let printed = 0usize;
    // 忙碌粘条计时(iter-31):任务起点,Submit 置、done/中断清;读秒/速率据此算(app 运行时用 Instant,非脚本)。
    let task_started: Option<Instant> = None;
    let last_activity: Option<Instant> = None;
    // 统一提交点(iter-33):键入的新提交 or 队首,非 busy 时于主环顶消费(起任务/跑命令),消除重复。
    // Opt-in no-network fixture starts one durable task before the first draw;
    // the real input path remains unchanged and production never auto-submits.
    let pending_submit: Option<String> = (std::env::var("RIDGE_TUI_FIXTURE").ok().as_deref()
        == Some("busy"))
    .then(|| "fixture busy task".to_string());

    // 阻塞读线程(iter-23):不开 crossterm `event-stream` feature(免引 futures 依赖),
    // std 线程 `event::read()` 转发进 tokio 通道;主环退出后线程仍阻塞在 read 上,随进程结束回收。
    let key_rx = spawn_key_reader();
    // tick 只登记 busy 时的动画帧需求;业务 busy 不再直接触发 draw。
    let tick = tokio::time::interval(Duration::from_millis(100));
    let dirty = true;
    let animation_due = false;

    // 失败自动重试(用户需求:给 10 次机会)—— 端点抖动/超时等瞬时失败自动重跑,不打断用户;
    // 成功/中断/新任务清零。`start_task` 据任务串 + 当前 history 装配 state 并 spawn(初次与重试共用)。
    let retry_count = 0usize;
    let last_task: Option<String> = None;
    let task_bus = bus.clone();
    let start_task: StartTask = Box::new(move |ti: &str, hist: &[Message]| {
        let state = AgentState::new(ti)
            .with_history(hist.to_vec())
            .with_budget(budget)
            .with_signals(agent::load_signal_block());
        let app = app.clone();
        let bus = task_bus.clone();
        let tx = event_tx.clone();
        let done = done_tx.clone();
        let tokens = token_tx.clone();
        tokio::spawn(async move {
            *bus.lock().unwrap() = Some(tokens);
            let result = invoke_durable(&app, state, &agent_run_config(), Some(&tx))
                .await
                .map_err(|e| e.to_string());
            *bus.lock().unwrap() = None;
            let _ = done.send(result);
        })
    });

    // 诊断开关:env `RIDGE_KEYLOG` **或** 标记文件 `~/.ridge/keylog.on` 任一存在即开(标记文件防呆:
    // 免 env 未被子进程继承之坑)。日志写**绝对路径** `~/.ridge/keylog.txt`(不依赖 cwd,便于定位)。
    // 供排查「某键(如空格)按了没反应」—— 看它被投递成什么 KeyCode/kind/modifiers,还是根本没到进程。
    let keylog_path = keylog_path();

    // 「已按下集」:去重 Windows 每键的 Press+Release,并识别输入法「仅 Release」的悬空字符注入。
    let pressed: std::collections::HashSet<KeyCode> = std::collections::HashSet::new();
    // Ctrl+Space 的按住审计标记；无 Release 能力的终端自然退化为原有 toggle。
    let momentary_hold = false;
    let last_ctrl_c: Option<Instant> = None;

    run_event_loop(TuiLoopContext {
        swap,
        skills,
        agents,
        commands,
        history,
        meta,
        terminal,
        guard: Some(guard),
        ui,
        live_cache,
        model_catalog_rx,
        approval_rx,
        event_rx,
        token_rx,
        done_rx,
        key_rx,
        tick,
        bus,
        start_task,
        keylog_path,
        pending,
        task,
        session_tokens,
        session_turns,
        printed,
        task_started,
        last_activity,
        pending_submit,
        retry_count,
        last_task,
        pressed,
        momentary_hold,
        last_ctrl_c,
        dirty,
        animation_due,
    })
    .await?;
    Ok(())
}
