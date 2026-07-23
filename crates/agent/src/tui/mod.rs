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
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Widget, Wrap},
    Terminal, TerminalOptions, Viewport,
};

use super::*;

type Term = Terminal<CrosstermBackend<io::Stdout>>;

/// Live 视口总高:状态行 1 + 流式尾巴 ≥5 + 输入框 3..=8。内联模式下 ratatui 只管这块,
/// 高度恒小于终端 —— 从根上杜绝「动态高度超视口触发全屏清屏」的闪烁根因。
const LIVE_HEIGHT: u16 = 14;

struct TerminalGuard;
impl TerminalGuard {
    fn enter() -> anyhow::Result<(Self, Term)> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        // BPM best-effort(iter-24):旧 Windows conhost 不支持则静默退化为逐字粘贴,绝不阻 TUI 启动。
        let _ = execute!(stdout, event::EnableBracketedPaste);
        // CSI u best-effort(iter-27):现代终端(Ghostty/WezTerm/iTerm2/kitty)得 Shift+Enter
        // 精确修饰键;不支持则静默降级(Alt+Enter / Ctrl+J 仍可换行)。只推 DISAMBIGUATE,
        // 不开 REPORT_EVENT_TYPES(免 press/release/repeat 事件噪声)。
        // ⚠ **仅非 Windows 推**:Windows Terminal 的 Kitty 键盘协议实现有缺陷 —— 开了它,**逐字打的空格键
        // 会被吞**(粘贴走 BracketedPaste 不受影响,故长任务粘贴照常);Windows 回落普通 WinAPI 键事件,
        // 空格正常,仅失 Shift+Enter 精确换行(Alt+Enter / Ctrl+J 仍可换行,损失可接受)。
        if !cfg!(windows) {
            let _ = execute!(
                stdout,
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            );
        }
        // 主屏内联视口(iter-26):不进备用屏,终端原生历史/选取/搜索神圣不可侵犯。
        let term = Terminal::with_options(
            CrosstermBackend::new(stdout),
            TerminalOptions {
                viewport: Viewport::Inline(LIVE_HEIGHT),
            },
        )?;
        Ok((Self, term))
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // 与 enter 对称:非 Windows 才需还原(Windows 从未推,pop 空栈无谓)。
        if !cfg!(windows) {
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
mod render;
mod status;
#[cfg(test)]
mod tests;

pub(crate) use app::*;
pub(crate) use command::*;
pub(crate) use draw::*;
pub(crate) use eventfmt::*;
pub(crate) use input::*;
pub(crate) use panel::*;
pub(crate) use render::*;
pub(crate) use status::*;

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
) -> anyhow::Result<()> {
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
    let (event_tx, mut event_rx) =
        tokio::sync::mpsc::unbounded_channel::<StreamEvent<AgentState>>();
    let (token_tx, mut token_rx) = tokio::sync::mpsc::unbounded_channel::<provider::StreamChunk>();
    let (done_tx, mut done_rx) =
        tokio::sync::mpsc::unbounded_channel::<Result<AgentState, String>>();
    let (_guard, mut terminal) = TerminalGuard::enter()?;
    let mut ui = Ui::default();
    ui.note(
        "RidgeCode  ·  inline mode: output lands in terminal history (native scroll/select) · Enter to send · Ctrl+J newline (Shift+Enter where supported) · Ctrl-C to interrupt · /help",
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
    // 统一提交点(iter-33):键入的新提交 or 队首,非 busy 时于主环顶消费(起任务/跑命令),消除重复。
    let mut pending_submit: Option<String> = None;

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
    // tick 只为 busy 时的 spinner 重绘;空闲时 should_draw=false,tick 醒来即再入睡,零重绘。
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let mut dirty = true;

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

    'main: loop {
        // 统一提交点(iter-33):非 busy 时消费 pending_submit —— 键入的新提交,或上一任务毕后接跑的队首。
        // 起任务/跑命令的逻辑**只此一处**(键 Submit 臂与 done 队列接跑共用),消除重复。
        if !ui.busy {
            if let Some(input) = pending_submit.take() {
                if run_command(
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
                .await?
                {
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
                    ui.phase = "reasoning".into();
                    ui.clear_streams();
                    ui.stream_tokens = 0;
                    task_started = Some(Instant::now());
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
        if should_draw(dirty, ui.busy) {
            ui.frame = ui.frame.wrapping_add(1);
            let elapsed_ms = task_started.map(|t| t.elapsed().as_millis()).unwrap_or(0);
            let ctx_used = history
                .iter()
                .map(|m| est_tokens(&m.content))
                .sum::<usize>();
            let vitals = Vitals {
                elapsed_s: (elapsed_ms / 1000) as u64,
                task_tokens: ui.stream_tokens,
                rate: token_rate(ui.stream_tokens, elapsed_ms),
                ctx_used,
                queued: ui.queued.len(),
            };
            terminal
                .draw(|frame| draw(frame, &ui, &meta, session_tokens, &vitals, pending.as_ref()))?;
            dirty = false;
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
                if let Event::Paste(s) = &ev {
                    // BPM(iter-24):整块原子注入(iter-27 起并入 InputState 光标处事务)。
                    ui.popup = None;
                    ui.input.insert_str(&sanitize_paste(s));
                    continue;
                }
                let Event::Key(key) = ev else { continue };
                // 去重 Windows 的 Press+Release 双触发,并**兜住输入法「仅 Release」的字符注入**
                //(实测:某些中文/国际输入法把空格键作为 Char('\u{a0}') 且只发 Release,旧「只收 Press」
                // 逻辑整个丢弃 → 打不出空格)。顺带把 no-break/全角空格归一为普通空格。见 `decide_key`。
                let Some(key) = decide_key(&mut pressed, &key) else {
                    continue;
                };
                if pending.is_some() {
                    // 模态状态机:审批态下滚动键**只滚不拒**(可先看 diff),仅 y/Enter 批准、n/Esc 拒绝,余键忽略。
                    match approval_action(key.code) {
                        ApprovalAction::Approve => {
                            if let Some(r) = pending.take() {
                                let _ = r.reply.send(true);
                            }
                            ui.note("✓ approved", Color::Green);
                        }
                        ApprovalAction::Reject => {
                            if let Some(r) = pending.take() {
                                let _ = r.reply.send(false);
                            }
                            ui.note("✗ rejected", Color::Red);
                        }
                        ApprovalAction::Scroll(d) => ui.scroll = apply_scroll(ui.scroll, d),
                        ApprovalAction::Ignore => {}
                    }
                    continue;
                }
                // 交互页模态(iter-35):优先级 审批 > Panel > 浮窗 > 输入。编辑态字符入编辑缓冲,浏览态入 query。
                if ui.panel.is_some() {
                    match panel_action(&key) {
                        PanelAction::Esc => {
                            let p = ui.panel.as_mut().unwrap();
                            if p.editing.is_some() {
                                p.editing = None; // 取消编辑
                            } else {
                                ui.panel = None; // 关页
                            }
                        }
                        PanelAction::Up => {
                            let p = ui.panel.as_mut().unwrap();
                            if p.editing.is_none() {
                                p.move_up();
                            }
                        }
                        PanelAction::Down => {
                            let p = ui.panel.as_mut().unwrap();
                            if p.editing.is_none() {
                                p.move_down();
                            }
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
                        }
                        PanelAction::Char(c) => {
                            let p = ui.panel.as_mut().unwrap();
                            match &mut p.editing {
                                Some(buf) => buf.push(c),
                                None => {
                                    p.query.push(c);
                                    p.retype();
                                }
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
                match input_action(&key, ui.busy, ui.popup.is_some()) {
                    InputAction::Interrupt => {
                        if let Some(handle) = task.take() {
                            handle.abort();
                            *bus.lock().unwrap() = None;
                            ui.busy = false;
                            ui.clear_streams();
                            task_started = None;
                            retry_count = 0; // 中断即取消重试链
                            // 中止即取消全部待跑(iter-33):不让排队项在中断后意外接跑。
                            let dropped = ui.queued.len();
                            ui.queued.clear();
                            pending_submit = None;
                            let tail = if dropped > 0 {
                                format!("interrupted current task (and cleared {dropped} queued)")
                            } else {
                                "interrupted current task".into()
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
                    InputAction::PopupApply => {
                        if let Some(p) = ui.popup.take() {
                            apply_completion(&mut ui.input, &p);
                        }
                    }
                    InputAction::PopupClose => ui.popup = None,
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
                            ui.note(
                                format!("⏳ queued ({} pending): {input}", ui.queued.len()),
                                role_color(Role::Muted),
                            );
                        }
                    }
                    InputAction::Ignore => {}
                }
            }
            Some(chunk) = token_rx.recv() => {
                ui.busy = true;
                ui.push_chunk(chunk); // 分道:回答→白尾巴,思考→灰尾巴
                // 批量排空积压 token,免逐 token 一帧。
                while let Ok(c) = token_rx.try_recv() {
                    ui.push_chunk(c);
                }
                dirty = true;
            }
            Some(event) = event_rx.recv() => {
                match event {
                    StreamEvent::NodeFinished { node, .. } => {
                        ui.phase = node_label(&node);
                        ui.busy = true;
                    }
                    StreamEvent::Superstep { state, .. } => {
                        for m in state.messages.iter().skip(printed) {
                            // 总览化(用户需求):读只显路径、写显预览、改显 ± diff;减噪。
                            for (line, color) in summarize_event(m) {
                                ui.note(line, color);
                            }
                        }
                        printed = state.messages.len();
                        // TODO 变更 → 清单快照静态提交进历史(取代旧侧边栏面板)。
                        if render_todo_block(&state.todos) != render_todo_block(&ui.todos)
                            && !state.todos.is_empty()
                        {
                            ui.note(render_todo_block(&state.todos), Color::Cyan);
                        }
                        ui.todos = state.todos;
                        // 流式已完段落随 Superstep 消息历史化,Live 只留尾巴。
                        ui.clear_streams();
                        ui.busy = false;
                    }
                }
                dirty = true;
            }
            Some(request) = approval_rx.recv() => {
                pending = Some(request);
                ui.scroll = 0; // 新审批从头看
                ui.busy = false;
                dirty = true;
            }
            Some(result) = done_rx.recv() => {
                task = None;
                ui.busy = false;
                ui.clear_streams();
                task_started = None;
                printed = 0;
                match result {
                    Ok(out) => {
                        retry_count = 0; // 成功:重试计数清零
                        history = out.history.clone();
                        save_session(&session_path(), &history);
                        session_tokens += out.total_tokens;
                        session_turns += 1;
                        ui.todos = out.todos.clone();
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
                                    format!("↻ 瞬时失败,自动重试 {retry_count}/{MAX_RETRIES}(上次: {e})"),
                                    Color::Yellow,
                                );
                                ui.busy = true;
                                ui.phase = "reasoning".into();
                                task_started = Some(Instant::now());
                                task = Some(start_task(&ti, &history));
                            }
                            _ => {
                                let tail = if !retryable {
                                    format!("error(不可重试,已停): {e}")
                                } else if retry_count >= MAX_RETRIES {
                                    format!("error(已重试 {MAX_RETRIES} 次仍失败): {e}")
                                } else {
                                    format!("error: {e}")
                                };
                                ui.note(tail, Color::Red);
                                agent::fire_session_hooks("stop", "error");
                                retry_count = 0;
                            }
                        }
                    }
                }
                // 排队接跑(iter-33):任务毕,取队首交主环顶统一提交点起下一任务。
                if pending_submit.is_none() {
                    pending_submit = ui.queued.pop_front();
                }
                dirty = true;
            }
            _ = tick.tick() => {
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
                        ui.stream = indent(&splash_frame(ui.splash, SPLASH_TICKS), splash_pad(width));
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
