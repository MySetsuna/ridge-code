//! RidgeCode 的交互式终端界面。执行图跑在后台 Tokio task，绘制与键盘事件留在前台，
//! 因而 token 流、工具事件和权限门都不会卡住界面。

use std::collections::VecDeque;
use std::io;
use std::sync::{mpsc, Arc};
use std::time::Duration;

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
    Terminal,
};

use super::*;

type Term = Terminal<CrosstermBackend<io::Stdout>>;

struct TerminalGuard;
impl TerminalGuard {
    fn enter() -> anyhow::Result<(Self, Term)> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        Ok((Self, Terminal::new(CrosstermBackend::new(stdout))?))
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

struct ApprovalRequest {
    action: String,
    detail: String,
    reply: mpsc::SyncSender<bool>,
}

/// 审批挂起时对一次按键的**纯决策**。修「滚动即拒绝」根因 —— 此前审批态下除 `y`/`Enter`
/// 外一切键(含滚动键)都落 `_ => 拒绝`,用户想滚动看 diff 反而误拒。滚动/忽略**不消**审批请求。
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum ApprovalAction {
    Approve,
    Reject,
    Scroll(i16),
    Ignore,
}

fn approval_action(key: KeyCode) -> ApprovalAction {
    match key {
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => ApprovalAction::Approve,
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => ApprovalAction::Reject,
        KeyCode::Up => ApprovalAction::Scroll(1),
        KeyCode::Down => ApprovalAction::Scroll(-1),
        KeyCode::PageUp => ApprovalAction::Scroll(8),
        KeyCode::PageDown => ApprovalAction::Scroll(-8),
        _ => ApprovalAction::Ignore,
    }
}

/// 应用滚动增量到偏移(u16 饱和)。正=向历史回滚(同主输入区 `Up` 语义)。
fn apply_scroll(scroll: u16, delta: i16) -> u16 {
    if delta >= 0 {
        scroll.saturating_add(delta as u16)
    } else {
        scroll.saturating_sub(delta.unsigned_abs())
    }
}

/// 主输入态对一次按键的**纯决策**(续 iter-22 `approval_action` 模式,iter-23):
/// 副作用(改 input/派任务/中断)由主环按返回值执行,函数本身零副作用、离线可测。
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum InputAction {
    Insert(char),
    Backspace,
    Scroll(i16),
    Submit,
    Interrupt,
    Ignore,
}

fn input_action(key: &KeyEvent, busy: bool) -> InputAction {
    if key.kind != KeyEventKind::Press {
        return InputAction::Ignore;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return InputAction::Interrupt;
    }
    match key.code {
        KeyCode::Char(c) => InputAction::Insert(c),
        KeyCode::Backspace => InputAction::Backspace,
        KeyCode::Up => InputAction::Scroll(1),
        KeyCode::Down => InputAction::Scroll(-1),
        KeyCode::PageUp => InputAction::Scroll(8),
        KeyCode::PageDown => InputAction::Scroll(-8),
        KeyCode::Enter if !busy => InputAction::Submit,
        _ => InputAction::Ignore,
    }
}

/// 要不要画这一帧:有状态变更(dirty)或 busy(spinner 需动)才画;空闲零重绘(iter-23)。
fn should_draw(dirty: bool, busy: bool) -> bool {
    dirty || busy
}

/// 日志环形缓冲上限:超出淘汰最旧行 —— 有界内存,长会话不膨胀(iter-23)。
const LOG_CAP: usize = 2000;

/// 视口尾窗:从 `len` 条日志取「距尾 `scroll` 行处、往上最多 `rows` 行」区间。
/// 越顶钳在顶端;len < rows 全量。纯函数 O(1) —— 每帧只构建窗口内行,替代全量重建。
/// ponytail: 逻辑行≈视觉行(超长行折行会溢出截尾);需精确锚底再算折行高度。
fn tail_window(len: usize, rows: usize, scroll: usize) -> std::ops::Range<usize> {
    let end = len.saturating_sub(scroll).max(rows.min(len));
    let start = end.saturating_sub(rows);
    start..end
}

struct TuiApprover {
    tx: tokio::sync::mpsc::UnboundedSender<ApprovalRequest>,
}
impl Approver for TuiApprover {
    fn approve(&self, action: &str, detail: &str) -> bool {
        let (reply, wait) = mpsc::sync_channel(1);
        self.tx
            .send(ApprovalRequest {
                action: action.into(),
                detail: detail.into(),
                reply,
            })
            .is_ok()
            && wait.recv().unwrap_or(false)
    }
}

#[derive(Default)]
struct Ui {
    input: String,
    log: VecDeque<(String, Color)>,
    stream: String,
    todos: Vec<Todo>,
    tool: String,
    scroll: u16,
    busy: bool,
    phase: String,
    frame: usize,
}
impl Ui {
    fn note(&mut self, text: impl Into<String>, color: Color) {
        self.log.push_back((text.into(), color));
        if self.log.len() > LOG_CAP {
            self.log.pop_front(); // 环形淘汰:有界内存
        }
    }
    /// 只构建视口尾窗内的行(O(rows)/帧),滚动经 `tail_window` 而非 Paragraph 偏移。
    fn output_lines(&self, rows: usize, scroll: usize) -> Vec<Line<'static>> {
        let w = tail_window(self.log.len(), rows, scroll);
        let mut lines: Vec<_> = self
            .log
            .iter()
            .skip(w.start)
            .take(w.len())
            .flat_map(|(s, c)| {
                s.lines().map(move |line| {
                    Line::from(Span::styled(line.to_owned(), Style::default().fg(*c)))
                })
            })
            .collect();
        if !self.stream.is_empty() {
            lines.extend(self.stream.lines().map(|s| {
                Line::from(Span::styled(
                    s.to_owned(),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ))
            }));
        }
        lines
    }
}

/// TUI 是交互入口；只有非 TTY 的自动化管道才会回落到旧文本 REPL。
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
) -> anyhow::Result<()> {
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
        skills,
        bus.clone(),
        agents.clone(),
        read_only,
    )?);
    let (event_tx, mut event_rx) =
        tokio::sync::mpsc::unbounded_channel::<StreamEvent<AgentState>>();
    let (token_tx, mut token_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let (done_tx, mut done_rx) =
        tokio::sync::mpsc::unbounded_channel::<Result<AgentState, String>>();
    let (_guard, mut terminal) = TerminalGuard::enter()?;
    let mut ui = Ui::default();
    ui.note(
        "RidgeCode  TUI  ·  Enter 发送 · Ctrl-C 中断 · /help 命令",
        Color::Cyan,
    );
    if skip_danger {
        ui.note(
            "⚠ skip-danger: 工具自动放行（灾难命令仍硬拦截）",
            Color::Red,
        );
    }
    if !history.is_empty() {
        ui.note(format!("已恢复 {} 条会话消息", history.len()), Color::Green);
    }
    let mut pending: Option<ApprovalRequest> = None;
    let mut task: Option<tokio::task::JoinHandle<()>> = None;
    let mut session_tokens = 0usize;
    let mut session_turns = 0usize;
    let mut printed = 0usize;

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

    'main: loop {
        if should_draw(dirty, ui.busy) {
            ui.frame = ui.frame.wrapping_add(1);
            terminal.draw(|frame| {
                draw(
                    frame,
                    &ui,
                    &meta,
                    session_tokens,
                    session_turns,
                    pending.as_ref(),
                )
            })?;
            dirty = false;
        }
        // 事件驱动多路复用替代 50ms 固定轮询(iter-23):无事时阻塞挂起,不烧 CPU。
        tokio::select! {
            biased;
            Some(ev) = key_rx.recv() => {
                dirty = true; // 键盘/resize 皆需重绘
                let Event::Key(key) = ev else { continue };
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if pending.is_some() {
                    // 模态状态机:审批态下滚动键**只滚不拒**(可先看 diff),仅 y/Enter 批准、n/Esc 拒绝,余键忽略。
                    match approval_action(key.code) {
                        ApprovalAction::Approve => {
                            if let Some(r) = pending.take() {
                                let _ = r.reply.send(true);
                            }
                            ui.note("✓ 已批准", Color::Green);
                        }
                        ApprovalAction::Reject => {
                            if let Some(r) = pending.take() {
                                let _ = r.reply.send(false);
                            }
                            ui.note("✗ 已拒绝", Color::Red);
                        }
                        ApprovalAction::Scroll(d) => ui.scroll = apply_scroll(ui.scroll, d),
                        ApprovalAction::Ignore => {}
                    }
                    continue;
                }
                match input_action(&key, ui.busy) {
                    InputAction::Interrupt => {
                        if let Some(handle) = task.take() {
                            handle.abort();
                            *bus.lock().unwrap() = None;
                            ui.busy = false;
                            ui.stream.clear();
                            ui.note("已中断当前任务", Color::Yellow);
                        }
                    }
                    InputAction::Insert(c) => ui.input.push(c),
                    InputAction::Backspace => {
                        ui.input.pop();
                    }
                    InputAction::Scroll(d) => ui.scroll = apply_scroll(ui.scroll, d),
                    InputAction::Submit => {
                        let input = std::mem::take(&mut ui.input);
                        let input = input.trim().to_owned();
                        if input.is_empty() {
                            continue;
                        }
                        if run_command(
                            &input,
                            &mut ui,
                            &mut history,
                            &mut meta,
                            &swap,
                            &agents,
                            session_tokens,
                            session_turns,
                        )? {
                            break 'main;
                        }
                        if input.starts_with('/') {
                            continue;
                        }
                        ui.note(format!("› {input}"), Color::Cyan);
                        history.push(Message::user(expand_mentions(&input)));
                        let state = AgentState::new(&input)
                            .with_history(history.clone())
                            .with_budget(budget)
                            .with_signals(agent::load_signal_block());
                        let app = app.clone();
                        let bus = bus.clone();
                        let tx = event_tx.clone();
                        let done = done_tx.clone();
                        let tokens = token_tx.clone();
                        ui.busy = true;
                        ui.phase = "推理中".into();
                        ui.stream.clear();
                        printed = 0;
                        task = Some(tokio::spawn(async move {
                            *bus.lock().unwrap() = Some(tokens);
                            let result = app
                                .invoke_with(state, &RunConfig::default(), None, Some(&tx))
                                .await
                                .map_err(|e| e.to_string());
                            *bus.lock().unwrap() = None;
                            let _ = done.send(result);
                        }));
                    }
                    InputAction::Ignore => {}
                }
            }
            Some(token) = token_rx.recv() => {
                ui.busy = true;
                ui.stream.push_str(&token);
                // 批量排空积压 token,免逐 token 一帧。
                while let Ok(t) = token_rx.try_recv() {
                    ui.stream.push_str(&t);
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
                            ui.note(format_event_plain(m), event_color(m));
                        }
                        printed = state.messages.len();
                        ui.todos = state.todos;
                        ui.stream.clear();
                        ui.busy = false;
                    }
                }
                dirty = true;
            }
            Some(request) = approval_rx.recv() => {
                pending = Some(request);
                ui.busy = false;
                dirty = true;
            }
            Some(result) = done_rx.recv() => {
                task = None;
                ui.busy = false;
                ui.stream.clear();
                printed = 0;
                match result {
                    Ok(out) => {
                        history = out.history.clone();
                        save_session(&session_path(), &history);
                        session_tokens += out.total_tokens;
                        session_turns += 1;
                        ui.todos = out.todos.clone();
                        ui.note(
                            format!(
                                "{} · steps={} · tokens={}",
                                if out.approved {
                                    "✓ approved"
                                } else {
                                    "✗ not approved"
                                },
                                out.steps,
                                out.total_tokens
                            ),
                            if out.approved {
                                Color::Green
                            } else {
                                Color::Red
                            },
                        );
                    }
                    Err(e) => ui.note(format!("错误: {e}"), Color::Red),
                }
                dirty = true;
            }
            _ = tick.tick() => {}
            else => break 'main,
        }
    }
    if let Some(handle) = task {
        handle.abort();
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_command(
    input: &str,
    ui: &mut Ui,
    history: &mut Vec<Message>,
    meta: &mut ReplMeta,
    swap: &Arc<SwapProvider>,
    agents: &agent::Agents,
    tokens: usize,
    turns: usize,
) -> anyhow::Result<bool> {
    match input {
        "/exit" | "/quit" => return Ok(true),
        "/help" => ui.note("/exit /reset /compact /cost /tools /model [name] /provider /agent /config [set key value]；@path 引用文件；Ctrl-C 中断；批准弹窗:y/Enter 批准、n/Esc 拒绝、↑↓ 滚动看详情。", Color::Gray),
        "/tools" => ui.note(format!("可用工具({}): {}", meta.tools.len(), meta.tools.join(", ")), Color::Gray),
        "/reset" => { history.clear(); save_session(&session_path(), history); ui.note("上下文已清空", Color::Yellow); }
        "/compact" => { let n = history.len(); *history = compact_history(std::mem::take(history), 4); ui.note(format!("上下文已压缩: {n} → {} 条", history.len()), Color::Yellow); }
        "/cost" => ui.note(format!("本会话累计: {tokens} tokens · {turns} 轮任务"), Color::Gray),
        _ if input == "/model" => ui.note(format!("provider={} · model={} · base_url={}\n热切换: /model <name>", meta.provider, meta.model, meta.base_url), Color::Gray),
        _ if input.starts_with("/model ") => {
            let name = input[7..].trim();
            if let Some(key) = std::env::var("RIDGE_API_KEY").ok().filter(|v| !v.is_empty()) { swap.swap(make_provider(&meta.provider, name, &meta.base_url, key)); meta.model = name.into(); ui.note(format!("已热切换 model={name}"), Color::Green); } else { ui.note("未设 RIDGE_API_KEY，无法切换模型", Color::Red); }
        }
        _ if input == "/config" => ui.note(format!("配置文件: {}（JSON，可直接编辑）\n当前: {} · {}\n持久化: /config set <key> <value>", config_path(), meta.provider, meta.model), Color::Gray),
        _ if input.starts_with("/config set ") => { let parts: Vec<_> = input.splitn(4, ' ').collect(); if parts.len() == 4 { match persist_config(parts[2], parts[3]) { Ok(path) => ui.note(format!("已写入 {path}；下次启动生效"), Color::Green), Err(e) => ui.note(format!("写入失败: {e}"), Color::Red) } } else { ui.note("用法: /config set <key> <value>", Color::Yellow); } }
        _ if input == "/provider" || input == "/provider list" => { let cfg = Config::load(config_path()); let list = cfg.providers.iter().map(|p| format!("{} · {} · {}", p.name, p.kind, p.model)).collect::<Vec<_>>().join("\n"); ui.note(if list.is_empty() { "没有 provider 档案；/provider add 请在 config.json 添加，或继续使用 /model。".into() } else { list }, Color::Gray); }
        _ if input.starts_with("/provider use ") => { let name = input[14..].trim(); if let Some(p) = Config::load(config_path()).providers.into_iter().find(|p| p.name == name) { match std::env::var(&p.key_env).ok().filter(|v| !v.is_empty()) { Some(key) => { swap.swap(make_provider(&p.kind, &p.model, &p.base_url, key)); meta.provider=p.kind; meta.model=p.model; meta.base_url=p.base_url; ui.note(format!("已切换 provider {name}"), Color::Green); }, None => ui.note(format!("{} 未设", p.key_env), Color::Red) } } else { ui.note(format!("没有 provider: {name}"), Color::Red); } }
        _ if input == "/agent" => {
            if agents.defs.is_empty() { ui.note("无可用 sub-agent", Color::Gray); }
            else { let list = agents.defs.iter().map(|d| format!("{} —— {}", d.name, d.description)).collect::<Vec<_>>().join("\n"); ui.note(format!("可用 sub-agent（主 agent 会自动 dispatch；文本 REPL 里可 /agent <name> <task> 手动派）：\n{list}"), Color::Gray); }
        }
        _ if input.starts_with('/') => ui.note(format!("未知命令: {input}（/help）"), Color::Yellow),
        _ => return Ok(false),
    }
    Ok(false)
}

fn draw(
    frame: &mut ratatui::Frame,
    ui: &Ui,
    meta: &ReplMeta,
    tokens: usize,
    _turns: usize,
    approval: Option<&ApprovalRequest>,
) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(frame.area());
    let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"][ui.frame % 10];
    let status = if ui.busy {
        format!(" {spinner} {}", ui.phase)
    } else {
        " ready".into()
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " RidgeCode ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                " {} · {} · {} tokens · {}{}",
                meta.provider,
                meta.model,
                tokens,
                std::env::current_dir()
                    .ok()
                    .and_then(|p| p.file_name().map(|x| x.to_string_lossy().to_string()))
                    .unwrap_or_default(),
                status
            )),
        ]))
        .style(Style::default().bg(Color::DarkGray)),
        outer[0],
    );
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(outer[1]);
    // 视口虚拟化(iter-23):只构建尾窗内的行,滚动经 tail_window,不再用 Paragraph 偏移。
    let rows = body[0].height.saturating_sub(2) as usize; // 减上下边框
    frame.render_widget(
        Paragraph::new(Text::from(ui.output_lines(rows, ui.scroll as usize)))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" 输出 / 执行流 "),
            )
            .wrap(Wrap { trim: false }),
        body[0],
    );
    let side = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(body[1]);
    let todos: Vec<ListItem> = ui
        .todos
        .iter()
        .map(|t| {
            let mark = match t.status.as_str() {
                "completed" => "✓",
                "in_progress" => "~",
                _ => " ",
            };
            ListItem::new(format!("[{mark}] {}", t.content))
        })
        .collect();
    frame.render_widget(
        List::new(todos).block(Block::default().borders(Borders::ALL).title(" TODO ")),
        side[0],
    );
    frame.render_widget(
        Paragraph::new(if ui.tool.is_empty() {
            "工具调用与批准详情会显示在这里"
        } else {
            &ui.tool
        })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" 工具 / Diff "),
        )
        .wrap(Wrap { trim: true }),
        side[1],
    );
    frame.render_widget(
        Paragraph::new(ui.input.as_str())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" 输入（Enter 发送）"),
            )
            .wrap(Wrap { trim: false }),
        outer[2],
    );
    if let Some(req) = approval {
        modal(frame, &req.action, &req.detail);
    }
}

fn modal(frame: &mut ratatui::Frame, action: &str, detail: &str) {
    let area = centered_rect(70, 45, frame.area());
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(format!(
            "⚠ 允许执行 {action}？\n\n{detail}\n\ny/Enter: 批准    n/Esc: 拒绝    ↑↓/PgUp/PgDn: 滚动看详情"
        ))
        .style(Style::default().fg(Color::Yellow))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" 需要权限 ")
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .wrap(Wrap { trim: false }),
        area,
    );
}
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(v[1])[1]
}
fn format_event_plain(m: &str) -> String {
    m.strip_prefix("(final) ")
        .map(|x| format!("🤖 {x}"))
        .unwrap_or_else(|| m.to_owned())
}
fn event_color(m: &str) -> Color {
    if m.starts_with("verify: PASS") {
        Color::Green
    } else if m.starts_with("verify: FAIL") {
        Color::Red
    } else if m.starts_with("act:") {
        Color::Yellow
    } else if m.contains("(final)") {
        Color::White
    } else {
        Color::Cyan
    }
}

#[cfg(test)]
mod tests {
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

    /// iter-23:主输入态键位路由纯函数 —— busy 时 Enter 不提交,Ctrl-C 恒中断,松键忽略。
    #[test]
    fn input_action_routes_keys() {
        let press = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert_eq!(
            input_action(&press(KeyCode::Char('a')), false),
            InputAction::Insert('a')
        );
        assert_eq!(
            input_action(&press(KeyCode::Backspace), false),
            InputAction::Backspace
        );
        assert_eq!(
            input_action(&press(KeyCode::Up), false),
            InputAction::Scroll(1)
        );
        assert_eq!(
            input_action(&press(KeyCode::PageDown), false),
            InputAction::Scroll(-8)
        );
        assert_eq!(
            input_action(&press(KeyCode::Enter), false),
            InputAction::Submit
        );
        // busy 时 Enter 不提交(任务进行中);Ctrl-C 是中断。
        assert_eq!(
            input_action(&press(KeyCode::Enter), true),
            InputAction::Ignore
        );
        assert_eq!(
            input_action(
                &KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                true
            ),
            InputAction::Interrupt
        );
        // 松键(Release)不触发任何动作。
        assert_eq!(
            input_action(
                &KeyEvent::new_with_kind(
                    KeyCode::Char('a'),
                    KeyModifiers::NONE,
                    KeyEventKind::Release
                ),
                false
            ),
            InputAction::Ignore
        );
    }

    /// iter-23:重绘判定 —— 脏或 busy(spinner)才画,空闲零重绘。
    #[test]
    fn draw_only_when_dirty_or_busy() {
        assert!(should_draw(true, false));
        assert!(should_draw(false, true));
        assert!(!should_draw(false, false));
    }

    /// iter-23:视口尾窗 —— 尾部取窗、回滚平移、越顶钳住、不足一屏全量、极值饱和不 panic。
    #[test]
    fn tail_window_clamps_and_saturates() {
        assert_eq!(tail_window(100, 10, 0), 90..100);
        assert_eq!(tail_window(100, 10, 5), 85..95);
        assert_eq!(tail_window(100, 10, 95), 0..10);
        assert_eq!(tail_window(100, 10, usize::MAX), 0..10);
        assert_eq!(tail_window(5, 10, 0), 0..5);
        assert_eq!(tail_window(5, 10, 3), 0..5);
        assert_eq!(tail_window(0, 10, 0), 0..0);
    }

    /// iter-23:日志环形缓冲有界,淘汰最旧、留存最新。
    #[test]
    fn log_ring_buffer_is_bounded_and_keeps_newest() {
        let mut ui = Ui::default();
        for i in 0..(LOG_CAP + 5) {
            ui.note(format!("line {i}"), Color::White);
        }
        assert_eq!(ui.log.len(), LOG_CAP);
        assert_eq!(ui.log.back().unwrap().0, format!("line {}", LOG_CAP + 4));
        assert_eq!(ui.log.front().unwrap().0, "line 5");
    }
}
