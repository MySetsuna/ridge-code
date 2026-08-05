//! RidgeCode 的交互式终端界面 —— **主屏内联 REPL**(iter-26)。
//! 不再霸占备用屏:历史内容经 `Terminal::insert_before` 静态提交进终端原生 scrollback
//! (原生滚动/选取/搜索全保留),ratatui 只渲染底部一小块 Live 视口(状态行 + 流式尾巴 + 输入框)。
//! 执行图跑在后台 Tokio task,token 流、工具事件和权限门都不会卡住界面(iter-23 事件驱动主环)。

use std::collections::VecDeque;
use std::io;
use std::sync::{mpsc, Arc};
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
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Widget, Wrap,
    },
    Terminal, TerminalOptions, Viewport,
};

use super::*;

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

/// Superstep 后仍有 frontier 即任务仍运行；空 frontier 才允许输入启动下一任务。
fn superstep_is_busy(active: &[String]) -> bool {
    !active.is_empty()
}

/// 仅在 UI 空闲且旧 task 已由 done 分支收走时启动新任务。
fn can_start_task(busy: bool, task_running: bool) -> bool {
    !busy && !task_running
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
}
impl TerminalGuard {
    fn enter() -> anyhow::Result<(Self, Term)> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        // BPM best-effort(iter-24):旧 Windows conhost 不支持则静默退化为逐字粘贴,绝不阻 TUI 启动。
        let _ = execute!(stdout, event::EnableBracketedPaste);
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
            },
            term,
        ))
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // 与 enter 对称:仅在 KKP 命令确实写出后还原,避免误发 pop。
        if self.keyboard_enhancement_pushed {
            let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
        }
        let _ = execute!(io::stdout(), event::DisableBracketedPaste);
        let _ = disable_raw_mode();
    }
}

// === 子模块(iter-52 按职责拆分,均为 tui 私有)===
mod app;
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
pub(crate) use command::*;
pub(crate) use draw::*;
pub(crate) use eventfmt::*;
pub(crate) use input::*;
pub(crate) use panel::*;
pub(crate) use presentation::*;
pub(crate) use render::*;
pub(crate) use status::*;
pub(crate) use transcript::*;

/// TUI 是交互入口；只有非 TTY 的自动化管道才会回落到 headless。
#[allow(clippy::too_many_arguments)]
pub(super) async fn run(
    swap: Arc<SwapProvider>,
    mcp: McpTools,
    skills: Vec<Skill>,
    skip_danger: bool,
    budget: usize,
    mut history: Vec<Message>,
    mut meta: ReplMeta,
    agents: Arc<agent::Agents>,
    read_only: bool,
    commands: Vec<agent::SlashCommand>,
    initial_effort: String,
) -> anyhow::Result<()> {
    tui_trace("run.enter");
    set_dynamic_commands(&commands); // 自定义/skill 命令名进补全源(iter-39)
    let (approval_tx, mut approval_rx) = tokio::sync::mpsc::unbounded_channel::<ApprovalRequest>();
    let approver: Arc<dyn Approver> = if skip_danger {
        Arc::new(AutoApprove)
    } else {
        Arc::new(TuiApprover { tx: approval_tx })
    };
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
    let (event_tx, mut event_rx) =
        tokio::sync::mpsc::unbounded_channel::<StreamEvent<AgentState>>();
    let (token_tx, mut token_rx) = tokio::sync::mpsc::unbounded_channel::<provider::StreamChunk>();
    let (done_tx, mut done_rx) =
        tokio::sync::mpsc::unbounded_channel::<Result<AgentState, String>>();
    let (_guard, mut terminal) = TerminalGuard::enter()?;
    tui_trace("terminal.ready");
    let mut live_cache = LiveOutputCache::default();
    let mut ui = Ui {
        effort: Some(initial_effort),
        ..Ui::default()
    };
    let session_input_history = if history.is_empty() {
        load_global_input_history()
    } else {
        let saved = load_session_input_history();
        if saved.is_empty() {
            history
                .iter()
                .filter_map(|message| match message.role {
                    provider::Role::User => Some(message.content.clone()),
                    _ => None,
                })
                .collect()
        } else {
            saved
        }
    };
    ui.input
        .set_history(session_input_history, !history.is_empty());
    let mut model_catalog_rx = Some(start_model_catalog_preload(&meta.provider, &meta.base_url));
    ui.note(
        "RidgeCode  ·  inline mode: output lands in terminal history (native scroll/select) · Enter send/queue · Ctrl+Enter front-queue without interrupt · Ctrl+I/Alt+I live inspect · Ctrl+Q queue · Ctrl+Space hold/follow · Ctrl+A answers · Ctrl+T activity · Ctrl+J newline · Esc/Ctrl-C takeover; press Ctrl-C twice to exit · /help",
        Color::Cyan,
    );
    if skip_danger {
        ui.note(
            "⚠ skip-danger: tools auto-approved (disaster commands still hard-blocked)",
            Color::Red,
        );
    }
    if !history.is_empty() {
        ui.note(
            format!("restored {} session messages", history.len()),
            Color::Green,
        );
    }
    let mut pending: Option<ApprovalRequest> = None;
    let mut task: Option<tokio::task::JoinHandle<()>> = None;
    let mut session_tokens = 0usize;
    let mut session_turns = 0usize;
    let mut printed = 0usize;
    // 忙碌粘条计时(iter-31):任务起点,Submit 置、done/中断清;读秒/速率据此算(app 运行时用 Instant,非脚本)。
    let mut task_started: Option<Instant> = None;
    let mut last_activity: Option<Instant> = None;
    // 统一提交点(iter-33):键入的新提交 or 队首,非 busy 时于主环顶消费(起任务/跑命令),消除重复。
    // Opt-in no-network fixture starts one durable task before the first draw;
    // the real input path remains unchanged and production never auto-submits.
    let mut pending_submit: Option<String> = (std::env::var("RIDGE_TUI_FIXTURE").ok().as_deref()
        == Some("busy"))
    .then(|| "fixture busy task".to_string());

    // 阻塞读线程(iter-23):不开 crossterm `event-stream` feature(免引 futures 依赖),
    // std 线程 `event::read()` 转发进 tokio 通道;主环退出后线程仍阻塞在 read 上,随进程结束回收。
    let (key_tx, mut key_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    std::thread::spawn(move || {
        while let Ok(ev) = event::read() {
            if key_tx.send(ev).is_err() {
                break;
            }
        }
    });
    // tick 只登记 busy 时的动画帧需求;业务 busy 不再直接触发 draw。
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let mut dirty = true;
    let mut animation_due = false;

    // 失败自动重试(用户需求:给 10 次机会)—— 端点抖动/超时等瞬时失败自动重跑,不打断用户;
    // 成功/中断/新任务清零。`start_task` 据任务串 + 当前 history 装配 state 并 spawn(初次与重试共用)。
    const MAX_RETRIES: usize = 10;
    let mut retry_count = 0usize;
    let mut last_task: Option<String> = None;
    let start_task = |ti: &str, hist: &[Message]| -> tokio::task::JoinHandle<()> {
        let state = AgentState::new(ti)
            .with_history(hist.to_vec())
            .with_budget(budget)
            .with_signals(agent::load_signal_block());
        let app = app.clone();
        let bus = bus.clone();
        let tx = event_tx.clone();
        let done = done_tx.clone();
        let tokens = token_tx.clone();
        tokio::spawn(async move {
            *bus.lock().unwrap() = Some(tokens);
            let result = app
                .invoke_with(state, &agent_run_config(), None, Some(&tx))
                .await
                .map_err(|e| e.to_string());
            *bus.lock().unwrap() = None;
            let _ = done.send(result);
        })
    };

    // 诊断开关:env `RIDGE_KEYLOG` **或** 标记文件 `~/.ridge/keylog.on` 任一存在即开(标记文件防呆:
    // 免 env 未被子进程继承之坑)。日志写**绝对路径** `~/.ridge/keylog.txt`(不依赖 cwd,便于定位)。
    // 供排查「某键(如空格)按了没反应」—— 看它被投递成什么 KeyCode/kind/modifiers,还是根本没到进程。
    let keylog_path: Option<std::path::PathBuf> = {
        let dir = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(|h| std::path::PathBuf::from(h).join(".ridge"))
            .unwrap_or_else(std::env::temp_dir);
        let on = std::env::var_os("RIDGE_KEYLOG").is_some() || dir.join("keylog.on").exists();
        on.then(|| dir.join("keylog.txt"))
    };

    // 「已按下集」:去重 Windows 每键的 Press+Release,并识别输入法「仅 Release」的悬空字符注入。
    let mut pressed: std::collections::HashSet<KeyCode> = std::collections::HashSet::new();
    // Ctrl+Space 的按住审计标记；无 Release 能力的终端自然退化为原有 toggle。
    let mut momentary_hold = false;
    let mut last_ctrl_c: Option<Instant> = None;

    'main: loop {
        if ui.model_catalog_reload {
            ui.model_catalog_reload = false;
            ui.model_catalog = None;
            model_catalog_rx = Some(start_model_catalog_preload(&meta.provider, &meta.base_url));
        }
        let model_catalog_result = match model_catalog_rx.as_mut() {
            Some(receiver) => match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => Some((Vec::new(), 1)),
            },
            None => None,
        };
        if let Some((grouped, failures)) = model_catalog_result {
            model_catalog_rx = None;
            auto_select_chatgpt_model(&grouped, &mut meta, &swap, &mut ui);
            let empty = grouped.is_empty();
            ui.model_catalog = Some(grouped);
            if empty && failures > 0 {
                ui.note(
                    "model catalog unavailable; retry /model after checking credentials or network",
                    Color::Yellow,
                );
            }
            dirty = true;
        }
        let oauth_device_event = match ui.oauth_device.as_mut() {
            Some(flow) => match flow.receiver.try_recv() {
                Ok(event) => Some(event),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => None,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => Some(
                    DeviceOAuthEvent::Complete(Err("device OAuth task stopped".into())),
                ),
            },
            None => None,
        };
        if let Some(event) = oauth_device_event {
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
                            apply_oauth_token(
                                &provider::oauth::OPENAI,
                                token,
                                &mut meta,
                                &swap,
                                &mut ui,
                            );
                        }
                        Err(error) => {
                            ui.device_auth_status = Some(format!("Device auth failed: {error}"));
                            ui.note(format!("Codex device OAuth failed: {error}"), Color::Red)
                        }
                    }
                }
            }
            dirty = true;
        }
        // OAuth callback server runs independently of keyboard input; poll it on
        // the 100ms event loop so browser completion needs no code paste.
        let oauth_result = match ui.oauth_callback.as_mut() {
            Some(callback) => match callback.receiver.try_recv() {
                Ok(result) => Some(result),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    Some(Err("local OAuth callback listener stopped".into()))
                }
            },
            None => None,
        };
        if let Some(result) = oauth_result {
            ui.oauth_callback.take();
            match result {
                Ok(code) => {
                    apply_oauth_code(&provider::oauth::OPENAI, &code, &mut meta, &swap, &mut ui)
                        .await;
                }
                Err(error) => ui.note(format!("OAuth callback failed: {error}"), Color::Red),
            }
            dirty = true;
        }
        // 统一提交点(iter-33):非 busy 时消费 pending_submit —— 键入的新提交,或上一任务毕后接跑的队首。
        // 起任务/跑命令的逻辑**只此一处**(键 Submit 臂与 done 队列接跑共用),消除重复。
        if can_start_task(ui.busy, task.is_some()) {
            if let Some(input) = pending_submit.take() {
                let should_exit = run_command(
                    &input,
                    &mut ui,
                    &mut history,
                    &mut meta,
                    &swap,
                    &agents,
                    &commands,
                    &skills,
                    session_tokens,
                    session_turns,
                )
                .await?;
                let starts_session = !input.starts_with('/') || ui.run_task.is_some();
                if starts_session && !ui.input.session_mode {
                    ui.input.drop_last_history_if(&input);
                    save_global_input_history(&ui.input.history);
                    ui.input.begin_session();
                    ui.input.push_history(&input);
                } else if !ui.input.session_mode {
                    save_global_input_history(&ui.input.history);
                }
                if ui.input.session_mode {
                    save_session_input_history(&ui.input.history);
                }
                if should_exit {
                    break 'main;
                }
                // 普通输入直接是任务;斜杠命令若为自定义/skill 命令,run_command 已把展开的 prompt 置 ui.run_task。
                let task_input = if input.starts_with('/') {
                    ui.run_task.take()
                } else {
                    Some(input.clone())
                };
                if let Some(ti) = task_input {
                    ui.note(format!("› {input}"), role_color(Role::Command));
                    history.push(Message::user(expand_mentions(&ti)));
                    last_task = Some(ti.clone());
                    retry_count = 0; // 新任务:重试计数清零
                    ui.busy = true;
                    ui.waiting = false;
                    ui.phase = "reasoning".into();
                    ui.set_activity("starting task");
                    ui.clear_streams();
                    ui.stream_tokens = 0;
                    ui.input_tokens = 0;
                    ui.output_tokens = 0;
                    ui.superstep = 0;
                    ui.pending_call = None;
                    task_started = Some(Instant::now());
                    last_activity = task_started;
                    printed = 0;
                    task = Some(start_task(&ti, &history));
                }
                dirty = true;
            }
        }
        // 静态提交先于绘制:历史行离开 Live 视口,进终端 scrollback。
        if !ui.commits.is_empty() {
            flush_commits(&mut terminal, &mut ui)?;
            dirty = true;
        }
        if should_draw(dirty, animation_due) {
            tui_trace("draw.begin");
            ui.frame = ui.frame.wrapping_add(1);
            let elapsed_ms = task_started.map(|t| t.elapsed().as_millis()).unwrap_or(0);
            let ctx_used = history
                .iter()
                .map(|m| est_tokens(&m.content))
                .sum::<usize>();
            let vitals = Vitals {
                step: ui.superstep,
                elapsed_s: (elapsed_ms / 1000) as u64,
                task_tokens: ui.stream_tokens,
                rate: token_rate(ui.stream_tokens, elapsed_ms),
                ctx_used,
                queued: ui.queued.len(),
            };
            terminal.draw(|frame| {
                draw_with_cache(
                    frame,
                    &ui,
                    &meta,
                    session_tokens,
                    &vitals,
                    pending.as_ref(),
                    &mut live_cache,
                )
            })?;
            tui_trace("draw.end");
            dirty = false;
            animation_due = false;
        }
        // 事件驱动多路复用替代固定轮询(iter-23):无事时阻塞挂起,不烧 CPU。
        tokio::select! {
            biased;
            Some(ev) = key_rx.recv() => {
                dirty = true; // 键盘/粘贴/resize 皆需重绘
                if let Some(p) = &keylog_path {
                    // Press 过滤之前记录,连 Release/Repeat 都留痕(诊断空格丢失的关键证据)。
                    use std::io::Write;
                    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
                        let _ = writeln!(f, "{ev:?}");
                    }
                }
                let key = match terminal_event_action(ev) {
                    TerminalEventAction::Paste(text) => {
                        apply_paste(&mut ui, &text);
                        continue;
                    }
                    TerminalEventAction::Redraw => continue,
                    TerminalEventAction::Key(key) => key,
                };
                // 去重 Windows 的 Press+Release 双触发,并**兜住输入法「仅 Release」的字符注入**
                //(实测:某些中文/国际输入法把空格键作为 Char('\u{a0}') 且只发 Release,旧「只收 Press」
                // 逻辑整个丢弃 → 打不出空格)。顺带把 no-break/全角空格归一为普通空格。见 `decide_key`。
                let Some(key) = decide_key(&mut pressed, &key) else {
                    continue;
                };
                if live_hold_release_action(&key, ui.popup.is_some()) {
                    if momentary_hold {
                        momentary_hold = false;
                        let _ = ui.follow_live();
                    }
                    continue;
                }
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(key.code, KeyCode::Char('c' | 'C'))
                {
                    let now = Instant::now();
                    if is_second_ctrl_c(last_ctrl_c, now) {
                        break 'main;
                    }
                    last_ctrl_c = Some(now);
                    if let Some(handle) = task.take() {
                        momentary_hold = false;
                        mark_takeover_requested(&mut ui);
                        handle.abort();
                        *bus.lock().unwrap() = None;
                        ui.busy = false;
                        ui.waiting = false;
                        ui.commit_live_reasoning(
                            ui.superstep,
                            task_started.map(|t| t.elapsed().as_secs()).unwrap_or(0),
                        );
                        ui.commit_live_answers(
                            "interrupted before final response",
                            ui.superstep,
                            task_started.map(|t| t.elapsed().as_secs()).unwrap_or(0),
                        );
                        ui.clear_streams();
                        ui.commit_live_tools();
                        ui.superstep = 0;
                        ui.pending_call = None;
                        task_started = None;
                        last_activity = None;
                        retry_count = 0;
                        pending_submit = None;
                        ui.set_activity("takeover ready");
                        let kept = ui.queued.len();
                        let tail = if kept > 0 {
                            format!("interrupted current task · takeover ready · {kept} queued kept")
                        } else {
                            "interrupted current task · takeover ready".into()
                        };
                        ui.note(tail, Color::Yellow);
                    } else {
                        ui.note(
                            "press Ctrl-C again within 2 seconds to exit",
                            Color::Yellow,
                        );
                    }
                    continue;
                }
                if pending.is_some() {
                    // 模态状态机:审批态下滚动键**只滚不拒**(可先看 diff),仅 y/Enter 批准、n/Esc 拒绝,余键忽略。
                    match approval_action(key.code) {
                        ApprovalAction::Approve => {
                            if let Some(r) = pending.take() {
                                if r.reply.send(true).is_ok() {
                                    ui.resume_after_approval();
                                }
                            }
                            ui.note("✓ approved", Color::Green);
                        }
                        ApprovalAction::Reject => {
                            if let Some(r) = pending.take() {
                                if r.reply.send(false).is_ok() {
                                    ui.resume_after_approval();
                                }
                            }
                            ui.note("✗ rejected", Color::Red);
                        }
                        ApprovalAction::Scroll(d) => ui.scroll = apply_scroll(ui.scroll, d),
                        ApprovalAction::Ignore => {}
                    }
                    continue;
                }
                if queue_panel_toggle_action(&key)
                    && (ui.panel.is_none()
                        || ui
                            .panel
                            .as_ref()
                            .is_some_and(|panel| panel.allows_attention_switch()))
                    && ui.popup.is_none()
                {
                    ui.toggle_queue_panel();
                    continue;
                }
                if live_history_toggle_action(
                    &key,
                    ui.popup.is_some(),
                    ui.transcript.has_history(),
                ) && (ui.panel.is_none()
                    || ui
                        .panel
                        .as_ref()
                        .is_some_and(|panel| panel.allows_attention_switch()))
                {
                    ui.toggle_live_history();
                    continue;
                }
                if let Some(action) = panel_attention_action(
                    &key,
                    ui.panel
                        .as_ref()
                        .is_some_and(|panel| panel.allows_attention_switch()),
                    ui.popup.is_some(),
                ) {
                    match action {
                        InputAction::ToggleDetails => {
                            if !ui.toggle_details_or_history() {
                                ui.note("no tool details or history", Color::Gray);
                            }
                        }
                        InputAction::ToggleReasoning => {
                            if !ui.toggle_reasoning_or_history() {
                                ui.note("no reasoning output or history", Color::Gray);
                            }
                        }
                        InputAction::ToggleAnswer => {
                            if !ui.toggle_answer_or_history() {
                                ui.note("no recoverable answer history", Color::Gray);
                            }
                        }
                        InputAction::ToggleActivity => ui.toggle_activity_panel(),
                        _ => unreachable!("panel_attention_action only returns attention actions"),
                    }
                    continue;
                }
                // 交互页模态(iter-35):优先级 审批 > Panel > 浮窗 > 输入。编辑态字符入编辑缓冲,浏览态入 query。
                if ui.panel.is_some() {
                    if key.kind == KeyEventKind::Press
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                        && matches!(key.code, KeyCode::Char('t' | 'T'))
                        && ui
                            .panel
                            .as_ref()
                            .is_some_and(|panel| panel.kind == PanelKind::Activity)
                    {
                        ui.toggle_activity_panel();
                        continue;
                    }
                    match panel_action(&key) {
                        PanelAction::Esc => {
                            let cancel_oauth = ui.panel.as_ref().is_some_and(|p| {
                                p.editing.is_some() && p.oauth_verifier.is_some()
                            });
                            let cancel_device = ui.oauth_device.is_some()
                                || ui.device_auth_status.is_some();
                            let p = ui.panel.as_mut().unwrap();
                            if p.editing.is_some() {
                                p.editing = None; // 取消编辑
                            } else {
                                ui.panel = None; // 关页
                            }
                            if cancel_oauth {
                                ui.oauth_callback.take();
                            }
                            if cancel_device {
                                ui.oauth_device.take();
                                ui.device_auth_status = None;
                            }
                        }
                        PanelAction::Remove => {
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
                                        format!("removed · {}", clip_display_cells(&message, 44)),
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
                            } else if key.code == KeyCode::Backspace {
                                let p = ui.panel.as_mut().unwrap();
                                match &mut p.editing {
                                    Some(buf) => {
                                        buf.pop();
                                    }
                                    None => {
                                        p.query.pop();
                                        p.retype();
                                    }
                                }
                            }
                        }
                        PanelAction::Up => {
                            {
                                let p = ui.panel.as_mut().unwrap();
                                if p.editing.is_none() {
                                    p.move_up();
                                }
                            }
                            ui.sync_live_panel_focus();
                        }
                        PanelAction::Down => {
                            {
                                let p = ui.panel.as_mut().unwrap();
                                if p.editing.is_none() {
                                    p.move_down();
                                }
                            }
                            ui.sync_live_panel_focus();
                        }
                        PanelAction::DetailPageUp => {
                            let p = ui.panel.as_mut().unwrap();
                            if p.editing.is_none() {
                                let _ = p.scroll_detail(-1);
                            }
                        }
                        PanelAction::DetailPageDown => {
                            let p = ui.panel.as_mut().unwrap();
                            if p.editing.is_none() {
                                let _ = p.scroll_detail(1);
                            }
                        }
                        PanelAction::PageUp => {
                            {
                                let p = ui.panel.as_mut().unwrap();
                                if p.editing.is_none() {
                                    p.page_up();
                                }
                            }
                            ui.sync_live_panel_focus();
                        }
                        PanelAction::PageDown => {
                            {
                                let p = ui.panel.as_mut().unwrap();
                                if p.editing.is_none() {
                                    p.page_down();
                                }
                            }
                            ui.sync_live_panel_focus();
                        }
                        PanelAction::First => {
                            {
                                let p = ui.panel.as_mut().unwrap();
                                if p.editing.is_none() {
                                    p.first();
                                }
                            }
                            ui.sync_live_panel_focus();
                        }
                        PanelAction::Last => {
                            {
                                let p = ui.panel.as_mut().unwrap();
                                if p.editing.is_none() {
                                    p.last();
                                }
                            }
                            ui.sync_live_panel_focus();
                        }
                        PanelAction::Backspace => {
                            let p = ui.panel.as_mut().unwrap();
                            match &mut p.editing {
                                Some(buf) => {
                                    buf.pop();
                                }
                                None => {
                                    p.query.pop();
                                    p.retype();
                                }
                            }
                            ui.sync_live_panel_focus();
                        }
                        PanelAction::Char(c) => {
                            let toggle_live = {
                                let p = ui.panel.as_mut().unwrap();
                                if p.kind == PanelKind::LiveHistory
                                    && p.editing.is_none()
                                    && c == ' '
                                {
                                    true
                                } else {
                                    match &mut p.editing {
                                        Some(buf) => buf.push(c),
                                        None => {
                                            p.query.push(c);
                                            p.retype();
                                        }
                                    }
                                    false
                                }
                            };
                            if toggle_live {
                                ui.toggle_live_panel_detail();
                            } else {
                                ui.sync_live_panel_focus();
                            }
                        }
                        PanelAction::Enter => {
                            // 登录页 key 输入态提交 → 异步校验 + 接入(唯一异步 Enter 分支);余走同步 panel_enter。
                            let login_submit = matches!(ui.panel.as_ref(), Some(p) if p.kind == PanelKind::Login && p.editing.is_some());
                            if login_submit {
                                let (id, key) = {
                                    let p = ui.panel.as_ref().unwrap();
                                    (
                                        p.selected().map(|r| r.key.clone()),
                                        p.editing.clone().unwrap_or_default(),
                                    )
                                };
                                // 订阅 OAuth 行(iter-43 claude / iter-48 codex)→ 泛化交换分支。
                                let ocfg = match id.as_deref() {
                                    Some(k) if k == CLAUDE_OAUTH_ROW => {
                                        Some(&provider::oauth::ANTHROPIC)
                                    }
                                    Some(k) if k == CODEX_OAUTH_ROW => {
                                        Some(&provider::oauth::OPENAI)
                                    }
                                    _ => None,
                                };
                                if let Some(ocfg) = ocfg {
                                    apply_oauth_code(ocfg, key.trim(), &mut meta, &swap, &mut ui)
                                        .await;
                                } else {
                                    match id.as_deref().and_then(preset_by_id) {
                                        Some(preset) if !key.trim().is_empty() => {
                                            ui.note(
                                                format!("verifying {}…", preset.id),
                                                Color::Gray,
                                            );
                                            login_apply_verified(
                                                preset,
                                                key.trim(),
                                                &mut meta,
                                                &swap,
                                                &mut ui,
                                            )
                                            .await;
                                        }
                                        Some(_) => {
                                            ui.note("enter a non-empty API key", Color::Yellow)
                                        }
                                        None => ui.note("no provider selected", Color::Red),
                                    }
                                }
                            } else {
                                panel_enter(&mut ui, &mut meta, &swap);
                            }
                        }
                        PanelAction::Ignore => {}
                    }
                    continue;
                }
                if let Some(delta) = tool_focus_action(&key, ui.popup.is_some(), ui.has_live_tools()) {
                    let _ = ui.move_tool_focus(delta);
                    continue;
                }
                if let Some(delta) = semantic_focus_action(
                    &key,
                    ui.popup.is_some(),
                    ui.transcript.is_inspecting(),
                    ui.has_inspectable_live_output(),
                ) {
                    let _ = ui.move_semantic_focus(delta);
                    ui.note("Alt+←/→ · semantic focus", role_color(Role::Info));
                    continue;
                }
                if let Some(delta) = tool_detail_scroll_action(
                    &key,
                    ui.popup.is_some(),
                    ui.has_scrollable_live_tool(),
                ) {
                    let _ = ui.scroll_tool_details(delta);
                    continue;
                }
                if live_hold_toggle_action(
                    &key,
                    ui.popup.is_some(),
                    ui.has_inspectable_live_output(),
                ) {
                    if ui.transcript.is_inspecting() {
                        momentary_hold = false;
                        let _ = ui.follow_live();
                    } else {
                        let _ = ui.hold_live();
                        momentary_hold = true;
                    }
                    continue;
                }
                if live_semantic_toggle_action(
                    &key,
                    ui.popup.is_some(),
                    ui.transcript.is_inspecting(),
                    ui.has_live_tools() || ui.transcript.has_reasoning(),
                ) {
                    let _ = ui.toggle_focused_semantic();
                    ui.note("Space · semantic block toggled", role_color(Role::Info));
                    continue;
                }
                if let Some(action) = live_scroll_action(
                    &key,
                    ui.popup.is_some(),
                    ui.has_scrollable_live_tool(),
                    ui.has_inspectable_live_output(),
                ) {
                    match action {
                        LiveScrollAction::Older => {
                            let _ = ui.scroll_live(1);
                        }
                        LiveScrollAction::Newer => {
                            let _ = ui.scroll_live(-1);
                        }
                        LiveScrollAction::OlderPage => {
                            let page_rows = crossterm::terminal::size()
                                .map(|(_, height)| live_page_rows(height))
                                .unwrap_or(12);
                            let _ = ui.scroll_live_page(1, page_rows);
                        }
                        LiveScrollAction::NewerPage => {
                            let page_rows = crossterm::terminal::size()
                                .map(|(_, height)| live_page_rows(height))
                                .unwrap_or(12);
                            let _ = ui.scroll_live_page(-1, page_rows);
                        }
                        LiveScrollAction::Follow => {
                            let _ = ui.follow_live();
                        }
                    }
                    continue;
                }
                match input_action(&key, ui.busy, ui.popup.is_some()) {
                    InputAction::Interrupt => {
                        if let Some(handle) = task.take() {
                            momentary_hold = false;
                            mark_takeover_requested(&mut ui);
                            handle.abort();
                            *bus.lock().unwrap() = None;
                            ui.busy = false;
                            ui.commit_live_reasoning(
                                ui.superstep,
                                task_started.map(|t| t.elapsed().as_secs()).unwrap_or(0),
                            );
                            ui.commit_live_answers(
                                "interrupted before final response",
                                ui.superstep,
                                task_started.map(|t| t.elapsed().as_secs()).unwrap_or(0),
                            );
                            ui.clear_streams();
                            ui.commit_live_tools();
                            ui.superstep = 0;
                            ui.pending_call = None;
                            task_started = None;
                            retry_count = 0; // 中断即取消重试链
                            pending_submit = None;
                            ui.set_activity("takeover ready");
                            let kept = ui.queued.len();
                            let tail = if kept > 0 {
                                format!("interrupted current task · takeover ready · {kept} queued kept")
                            } else {
                                "interrupted current task · takeover ready".into()
                            };
                            ui.note(tail, Color::Yellow);
                        }
                    }
                    InputAction::Insert(c) => {
                        // 随打随滤(iter-35):打 `/`/`@` 即弹补全并实时过滤,余字符自然关窗(build_popup→None)。
                        ui.input.insert(c);
                        ui.popup = build_popup(&ui.input);
                    }
                    InputAction::Backspace => {
                        ui.input.backspace();
                        ui.popup = build_popup(&ui.input);
                    }
                    InputAction::Left => ui.input.left(),
                    InputAction::Right => ui.input.right(),
                    InputAction::Home => ui.input.home(),
                    InputAction::End => ui.input.end(),
                    InputAction::NewLine => ui.input.insert('\n'),
                    InputAction::CursorUpOrHistory => {
                        // 转换函数惯例(iter-27):非首行 = 光标上移;首行 = 历史召回。
                        // iter-48 G5:首行折多视觉行且非行首 → 先跳行首,再 Up 才召回
                        // (修长草稿被历史突变替换=「光标卡首行」)。
                        if !ui.input.move_up() {
                            let w = crossterm::terminal::size()
                                .map(|(c, _)| c)
                                .unwrap_or(80)
                                .saturating_sub(2);
                            if up_fallback_is_home(&ui.input.buffer, ui.input.cursor, w) {
                                ui.input.home();
                            } else {
                                ui.input.recall_prev();
                            }
                        }
                    }
                    InputAction::CursorDownOrHistory => {
                        if !ui.input.move_down() {
                            ui.input.recall_next();
                        }
                    }
                    InputAction::PopupOpen => ui.popup = build_popup(&ui.input),
                    InputAction::PopupNext => {
                        if let Some(p) = &mut ui.popup {
                            p.selected = (p.selected + 1) % p.items.len();
                        }
                    }
                    InputAction::PopupPrev => {
                        if let Some(p) = &mut ui.popup {
                            p.selected = (p.selected + p.items.len() - 1) % p.items.len();
                        }
                    }
                    InputAction::PopupAccept => {
                        if let Some(p) = ui.popup.take() {
                            apply_completion(&mut ui.input, &p);
                        }
                    }
                    InputAction::PopupSubmit => {
                        if let Some(p) = ui.popup.take() {
                            apply_completion(&mut ui.input, &p);
                        }
                        let input = ui.input.take().trim().to_owned();
                        if !input.is_empty() {
                            if ui.busy {
                                ui.queued.push_back(input.clone());
                                ui.note(
                                    format!("⏳ queued ({} pending; current turn continues): {input}", ui.queued.len()),
                                    role_color(Role::Muted),
                                );
                            } else {
                                pending_submit = Some(input);
                            }
                        }
                    }
                    InputAction::PopupClose => ui.popup = None,
                    InputAction::ToggleDetails => {
                        let _ = ui.toggle_details_or_history();
                    }
                    InputAction::ToggleReasoning => {
                        let _ = ui.toggle_reasoning_or_history();
                    }
                    InputAction::ToggleAnswer => {
                        let _ = ui.toggle_answer_or_history();
                    }
                    InputAction::ToggleActivity => ui.toggle_activity_panel(),
                    InputAction::OpenLiveSearch => {
                        if !ui.open_live_search("") {
                            ui.note("no live blocks to search", Color::Gray);
                        }
                    }
                    InputAction::Submit => {
                        // 空闲提交:交主环顶统一提交点起任务/跑命令(iter-33)。
                        let input = ui.input.take().trim().to_owned();
                        if !input.is_empty() {
                            pending_submit = Some(input);
                        }
                    }
                    InputAction::Queue => {
                        // busy 提交:入队,当前任务毕自动接跑(iter-33)。
                        let input = ui.input.take().trim().to_owned();
                        if !input.is_empty() {
                            ui.queued.push_back(input.clone());
                            ui.refresh_queue_panel();
                            ui.record_activity(
                                ActivityKind::Queue,
                                format!("queued · {}", clip_display_cells(&input, 48)),
                            );
                            ui.note(
                                format!("⏳ queued ({} pending; current turn continues): {input}", ui.queued.len()),
                                role_color(Role::Muted),
                            );
                        }
                    }
                    InputAction::PushNow => {
                        let input = ui.input.take().trim().to_owned();
                        if !input.is_empty() {
                            ui.queued.push_front(input.clone());
                            ui.refresh_queue_panel();
                            ui.record_activity(
                                ActivityKind::Queue,
                                format!("front-queued · {}", clip_display_cells(&input, 44)),
                            );
                            ui.note(
                                format!("⏩ front-queued ({} pending; current turn continues): {input}", ui.queued.len()),
                                role_color(Role::Primary),
                            );
                        }
                    }
                    InputAction::Ignore => {}
                }
            }
            Some(chunk) = token_rx.recv() => {
                ui.busy = true;
                ui.waiting = false;
                last_activity = Some(Instant::now());
                ui.set_activity(match &chunk {
                    provider::StreamChunk::Answer(_) => "model · answering",
                    provider::StreamChunk::Reasoning(_) => "model · thinking",
                });
                ui.push_chunk(chunk); // 分道:回答→白尾巴,思考→灰尾巴
                // 批量排空积压 token,免逐 token 一帧。
                for _ in 0..MAX_STREAM_CHUNKS_PER_WAKE {
                    match token_rx.try_recv() {
                        Ok(c) => {
                            ui.set_activity(match &c {
                                provider::StreamChunk::Answer(_) => "model · answering",
                                provider::StreamChunk::Reasoning(_) => "model · thinking",
                            });
                            ui.push_chunk(c);
                        }
                        Err(_) => break,
                    }
                }
                dirty = true;
            }
            Some(event) = event_rx.recv() => {
                ui.waiting = false;
                last_activity = Some(Instant::now());
                match event {
                    StreamEvent::NodeFinished { node, .. } => {
                        ui.phase = node_label(&node);
                        ui.set_activity(format!("node · {}", ui.phase));
                        ui.busy = true;
                    }
                    StreamEvent::Superstep {
                        step,
                        active,
                        state,
                    } => {
                        ui.superstep = step;
                        ui.input_tokens = state.input_tokens;
                        ui.output_tokens = state.output_tokens;
                        ui.pending_call = state.pending_call.clone();
                        let active_label = active
                            .iter()
                            .map(|node| node_label(node))
                            .collect::<Vec<_>>()
                            .join(" + ");
                        let activity = if let Some(call) = state.pending_call.as_ref() {
                            format!("tool · {}", call.name)
                        } else if active_label.is_empty() {
                            "settling result".to_owned()
                        } else {
                            format!("next · {active_label}")
                        };
                        ui.set_activity(activity);
                        let answer_step = ui.superstep;
                        let answer_elapsed_s =
                            task_started.map(|started| started.elapsed().as_secs()).unwrap_or(0);
                        let answer_tokens = ui.stream_tokens;
                        for m in state.messages.iter().skip(printed) {
                            // 总览化(用户需求):读只显路径、写显预览、改显 ± diff;减噪。
                            if let Some(tool) = tool_preview(m) {
                                ui.push_tool(tool);
                            } else {
                                let is_final = is_final_event(m);
                                for (line, color) in summarize_event(m) {
                                    if is_final {
                                        ui.note_markdown_with_meta(
                                            line,
                                            answer_step,
                                            answer_elapsed_s,
                                            answer_tokens,
                                        );
                                    } else {
                                        ui.note(line, color);
                                    }
                                }
                            }
                        }
                        printed = state.messages.len();
                        // TODO 变更 → 进入有界 PLAN 活动锚点；详情仍由 Ctrl+T 展开。
                        let todo_snapshot = render_todo_block(&state.todos);
                        if todo_snapshot != render_todo_block(&ui.todos)
                            && !state.todos.is_empty()
                        {
                            ui.record_plan(todo_snapshot);
                        }
                        ui.todos = state.todos;
                        // 流式已完段落随 Superstep 消息历史化,Live 只留尾巴。
                        ui.commit_live_reasoning(
                            ui.superstep,
                            task_started.map(|t| t.elapsed().as_secs()).unwrap_or(0),
                        );
                        ui.clear_streams();
                        ui.busy = superstep_is_busy(&active);
                    }
                }
                dirty = true;
            }
            Some(request) = approval_rx.recv() => {
                pending = Some(request);
                momentary_hold = false;
                ui.scroll = 0; // 新审批从头看
                ui.busy = false;
                ui.waiting = false;
                ui.set_activity("approval required · user can take over");
                dirty = true;
            }
            Some(result) = done_rx.recv() => {
                task = None;
                momentary_hold = false;
                ui.busy = false;
                ui.waiting = false;
                let partial_answer_reason = unfinished_answer_reason(&result);
                ui.commit_live_reasoning(
                    ui.superstep,
                    task_started.map(|t| t.elapsed().as_secs()).unwrap_or(0),
                );
                if let Some(reason) = partial_answer_reason {
                    ui.commit_live_answers(
                        reason,
                        ui.superstep,
                        task_started.map(|t| t.elapsed().as_secs()).unwrap_or(0),
                    );
                }
                ui.clear_streams();
                ui.commit_live_tools();
                ui.superstep = 0;
                ui.pending_call = None;
                task_started = None;
                last_activity = None;
                printed = 0;
                match result {
                    Ok(out) => {
                        retry_count = 0; // 成功:重试计数清零
                        history = out.history.clone();
                        save_session(&session_path(), &history);
                        ui.input_tokens = out.input_tokens;
                        ui.output_tokens = out.output_tokens;
                        session_tokens += out.total_tokens;
                        session_turns += 1;
                        ui.todos = out.todos.clone();
                        ui.set_activity(if out.approved {
                            "completed"
                        } else {
                            "stopped · not approved"
                        });
                        // 显停机原因:未通过时把 halt_reason 一并播报(为何停一眼可见),配合「收束回合」的模型陈述。
                        let status = if out.approved {
                            "✓ approved".to_string()
                        } else {
                            format!("✗ not approved ({})", halt_reason(&out).as_str())
                        };
                        ui.note(
                            format!(
                                "{status} · steps={} · tokens={}",
                                out.steps, out.total_tokens
                            ),
                            if out.approved {
                                Color::Green
                            } else {
                                Color::Red
                            },
                        );
                        // Hook(iter-40):任务毕 → 审计留痕 +(notify)响铃 + config stop hook。
                        agent::fire_session_hooks(
                            "stop",
                            &format!("steps={} tokens={}", out.steps, out.total_tokens),
                        );
                    }
                    Err(e) => {
                        // 自动重试只管**瞬时**失败(端点抖动/超时/5xx/限流),不打断用户;
                        // **永久**失败(余额不足/鉴权失败/400 坏请求)不进重试链 —— 重试同样输入只白烧,
                        // 立刻把 provider 原文摊给用户,让「余额/key 失效」一眼可辨(iter-51)。
                        let retryable = is_retryable_error(&e);
                        match last_task.clone() {
                            Some(ti) if retryable && retry_count < MAX_RETRIES => {
                                retry_count += 1;
                                ui.note(
                                    format!(
                                        "↻ Transient failure, retrying {retry_count}/{MAX_RETRIES} (last: {e})"
                                    ),
                                    Color::Yellow,
                                );
                                ui.busy = true;
                                ui.waiting = false;
                                ui.phase = "reasoning".into();
                                ui.set_activity(format!("retrying · reasoning {retry_count}/{MAX_RETRIES}"));
                                ui.superstep = 0;
                                ui.pending_call = None;
                                task_started = Some(Instant::now());
                                last_activity = task_started;
                                task = Some(start_task(&ti, &history));
                            }
                            _ => {
                                let tail = if !retryable {
                                    format!("error (not retryable, stopped): {e}")
                                } else if retry_count >= MAX_RETRIES {
                                    format!("error (failed after {MAX_RETRIES} retries): {e}")
                                } else {
                                    format!("error: {e}")
                                };
                                ui.note(tail, Color::Red);
                                ui.set_activity("stopped · error");
                                agent::fire_session_hooks("stop", "error");
                                retry_count = 0;
                            }
                        }
                    }
                }
                // 排队接跑(iter-33):任务毕,取队首交主环顶统一提交点起下一任务。
                if pending_submit.is_none() {
                    pending_submit = ui.queued.pop_front();
                    ui.refresh_queue_panel();
                }
                dirty = true;
            }
            _ = tick.tick() => {
                let was_waiting = ui.waiting;
                ui.waiting = ui.busy
                    && last_activity
                        .is_some_and(|at| at.elapsed() >= Duration::from_secs(8));
                if ui.waiting && !was_waiting {
                    ui.record_activity(ActivityKind::Waiting, "waiting · no stream for 8s");
                }
                animation_due = ui.busy && pending.is_none() && ui.panel.is_none();
                // 启动帧序列(iter-28;iter-36 居中+防折行):空闲时借 tick 渐显 banner,末帧整幅入历史。
                if ui.splash < SPLASH_TICKS && !ui.busy && pending.is_none() {
                    ui.splash += 1;
                    let width = terminal.size().map(|s| s.width as usize).unwrap_or(80);
                    if ui.splash == SPLASH_TICKS {
                        // 落定 banner:居中 + 不折行(splash_block 逐行 ≤ width)+ tagline。
                        ui.note(splash_block(width).join("\n"), role_color(Role::Primary));
                        ui.clear_streams();
                    } else {
                        // 动画帧与落定 banner 同一居中偏移,消除「揭示→落定」的横跳。
                        ui.transcript
                            .set_splash(indent(&splash_frame(ui.splash, SPLASH_TICKS), splash_pad(width)));
                    }
                    dirty = true;
                }
            }
            else => break 'main,
        }
    }
    if let Some(handle) = task {
        handle.abort();
    }
    Ok(())
}
