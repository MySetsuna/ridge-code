//! RidgeCode 的交互式终端界面。执行图跑在后台 Tokio task，绘制与键盘事件留在前台，
//! 因而 token 流、工具事件和权限门都不会卡住界面。

use std::io;
use std::sync::{mpsc, Arc};
use std::time::Duration;

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
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

struct TuiApprover {
    tx: mpsc::Sender<ApprovalRequest>,
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
    log: Vec<(String, Color)>,
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
        self.log.push((text.into(), color));
    }
    fn output_lines(&self) -> Vec<Line<'static>> {
        let mut lines: Vec<_> = self
            .log
            .iter()
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
    let (approval_tx, approval_rx) = mpsc::channel();
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

    loop {
        while let Ok(token) = token_rx.try_recv() {
            ui.busy = true;
            ui.stream.push_str(&token);
        }
        while let Ok(event) = event_rx.try_recv() {
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
        }
        while let Ok(request) = approval_rx.try_recv() {
            pending = Some(request);
            ui.busy = false;
        }
        while let Ok(result) = done_rx.try_recv() {
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
        }
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
        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if let Some(request) = pending.take() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    let _ = request.reply.send(true);
                    ui.note("✓ 已批准", Color::Green);
                }
                _ => {
                    let _ = request.reply.send(false);
                    ui.note("✗ 已拒绝", Color::Red);
                }
            }
            continue;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            if let Some(handle) = task.take() {
                handle.abort();
                *bus.lock().unwrap() = None;
                ui.busy = false;
                ui.stream.clear();
                ui.note("已中断当前任务", Color::Yellow);
            }
            continue;
        }
        match key.code {
            KeyCode::Char(c) => ui.input.push(c),
            KeyCode::Backspace => {
                ui.input.pop();
            }
            KeyCode::Up => ui.scroll = ui.scroll.saturating_add(1),
            KeyCode::Down => ui.scroll = ui.scroll.saturating_sub(1),
            KeyCode::PageUp => ui.scroll = ui.scroll.saturating_add(8),
            KeyCode::PageDown => ui.scroll = ui.scroll.saturating_sub(8),
            KeyCode::Enter if !ui.busy => {
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
                    break;
                }
                if input.starts_with('/') {
                    continue;
                }
                ui.note(format!("› {input}"), Color::Cyan);
                history.push(Message::user(expand_mentions(&input)));
                let state = AgentState::new(&input)
                    .with_history(history.clone())
                    .with_budget(budget);
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
            _ => {}
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
        "/help" => ui.note("/exit /reset /compact /cost /tools /model [name] /provider /agent /config [set key value]；@path 引用文件；Ctrl-C 中断；批准弹窗按 y/Enter 或任意键拒绝。", Color::Gray),
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
    frame.render_widget(
        Paragraph::new(Text::from(ui.output_lines()))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" 输出 / 执行流 "),
            )
            .wrap(Wrap { trim: false })
            .scroll((ui.scroll, 0)),
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
            "⚠ 允许执行 {action}？\n\n{detail}\n\ny / Enter: 批准    任意其他键: 拒绝"
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
}
