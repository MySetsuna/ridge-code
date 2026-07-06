//! ratatui 实时视图:后台 task 跑编排发 `Event`,主线程渲染循环收事件、重绘进度。
//! 支持中英文界面切换。

use anyhow::{Context, Result};
use ratatui::crossterm::event::{self, Event as CtEvent, KeyCode, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use rc_core::{Orchestrator, Outcome};
use rc_types::{Cost, Difficulty, Event, ModelTier, Phase};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const MAX_TOOLS: usize = 30;
const MAX_LOG: usize = 100;
const POLL_MS: u64 = 50;

// ═══════════════════════════════════════════════════════════════
// 多语言支持
// ═══════════════════════════════════════════════════════════════

#[derive(Clone, Copy, PartialEq)]
pub enum Lang {
    Zh,
    En,
}

impl Lang {
    fn phase(&self, p: Phase) -> &'static str {
        match self {
            Lang::Zh => match p {
                Phase::Planning => "规划中",
                Phase::Executing => "执行中",
                Phase::Verifying => "验证中",
                Phase::Reviewing => "评审中",
                Phase::Done => "完成",
            },
            Lang::En => match p {
                Phase::Planning => "Planning",
                Phase::Executing => "Executing",
                Phase::Verifying => "Verifying",
                Phase::Reviewing => "Reviewing",
                Phase::Done => "Done",
            },
        }
    }

    fn tier(&self, t: ModelTier) -> &'static str {
        match self {
            Lang::Zh => match t {
                ModelTier::Strong => "强",
                ModelTier::Weak => "弱",
            },
            Lang::En => match t {
                ModelTier::Strong => "S",
                ModelTier::Weak => "W",
            },
        }
    }

    fn difficulty(&self, d: Difficulty) -> &'static str {
        match self {
            Lang::Zh => match d {
                Difficulty::Trivial => "易",
                Difficulty::Moderate => "中",
                Difficulty::Hard => "难",
            },
            Lang::En => match d {
                Difficulty::Trivial => "E",
                Difficulty::Moderate => "M",
                Difficulty::Hard => "H",
            },
        }
    }

    fn title(&self) -> &str {
        match self {
            Lang::Zh => " ridge-code ",
            Lang::En => " ridge-code ",
        }
    }

    fn tokens_label(&self) -> &str {
        match self {
            Lang::Zh => "Token",
            Lang::En => "Tokens",
        }
    }

    fn cost_label(&self) -> &str {
        match self {
            Lang::Zh => "成本",
            Lang::En => "Cost",
        }
    }

    fn strong_ratio_label(&self) -> &str {
        match self {
            Lang::Zh => "强占比",
            Lang::En => "Strong%",
        }
    }

    fn subtask_label(&self) -> &str {
        match self {
            Lang::Zh => "子任务",
            Lang::En => "Subtasks",
        }
    }

    fn tools_label(&self) -> &str {
        match self {
            Lang::Zh => "工具调用",
            Lang::En => "Tools",
        }
    }

    fn events_label(&self) -> &str {
        match self {
            Lang::Zh => "事件",
            Lang::En => "Events",
        }
    }

    fn message_label(&self) -> &str {
        match self {
            Lang::Zh => "消息",
            Lang::En => "Messages",
        }
    }

    fn running(&self) -> &str {
        match self {
            Lang::Zh => "运行中...",
            Lang::En => "Running...",
        }
    }

    fn finished(&self) -> &str {
        match self {
            Lang::Zh => "完成",
            Lang::En => "Done",
        }
    }

    fn quit_hint(&self) -> &str {
        match self {
            Lang::Zh => "按 q / Ctrl-C 退出",
            Lang::En => "Press q / Ctrl-C to quit",
        }
    }

    fn model_label(&self) -> &str {
        match self {
            Lang::Zh => "模型",
            Lang::En => "Model",
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// 动画帧
// ═══════════════════════════════════════════════════════════════

struct Spinner {
    frames: &'static [&'static str],
    index: usize,
    last_tick: Instant,
}

impl Spinner {
    fn new() -> Self {
        Self {
            frames: &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
            index: 0,
            last_tick: Instant::now(),
        }
    }

    fn tick(&mut self) -> &str {
        if self.last_tick.elapsed() >= Duration::from_millis(80) {
            self.index = (self.index + 1) % self.frames.len();
            self.last_tick = Instant::now();
        }
        self.frames[self.index]
    }
}

// ═══════════════════════════════════════════════════════════════
// TUI 状态
// ═══════════════════════════════════════════════════════════════

#[derive(Clone, Copy, PartialEq, Debug)]
enum RowStatus {
    Pending,
    Running,
    Done,
}

struct SubtaskRow {
    id: String,
    desc: String,
    difficulty: Difficulty,
    status: RowStatus,
    tier: Option<ModelTier>,
}

struct TuiState {
    phase: Phase,
    subtasks: Vec<SubtaskRow>,
    tools: VecDeque<String>,
    log: VecDeque<String>,
    messages: VecDeque<String>,  // 模型输出内容
    tool_results: VecDeque<String>, // 工具结果
    output: String,              // 最终产出
    cost: Cost,
    finished: bool,
    lang: Lang,
    spinner: Spinner,
    start_time: Instant,
    current_model: String,
}

impl TuiState {
    fn new(lang: Lang) -> Self {
        Self {
            phase: Phase::Planning,
            subtasks: Vec::new(),
            tools: VecDeque::new(),
            log: VecDeque::new(),
            messages: VecDeque::new(),
            tool_results: VecDeque::new(),
            output: String::new(),
            cost: Cost::default(),
            finished: false,
            lang,
            spinner: Spinner::new(),
            start_time: Instant::now(),
            current_model: String::new(),
        }
    }

    fn log_line(&mut self, s: impl Into<String>) {
        self.log.push_back(s.into());
        while self.log.len() > MAX_LOG {
            self.log.pop_front();
        }
    }

    fn add_message(&mut self, s: impl Into<String>) {
        let msg = s.into();
        self.messages.push_back(msg.clone());
        while self.messages.len() > 50 {
            self.messages.pop_front();
        }
        // 同时作为最终输出
        if !msg.is_empty() {
            self.output = msg;
        }
    }

    fn apply(&mut self, ev: Event) {
        match ev {
            Event::Phase(p) => {
                self.phase = p;
                self.log_line(format!("▶ {}", self.lang.phase(p)));
            }
            Event::Planned(list) => {
                self.subtasks = list
                    .into_iter()
                    .map(|s| SubtaskRow {
                        id: s.id,
                        desc: s.description,
                        difficulty: s.difficulty,
                        status: RowStatus::Pending,
                        tier: None,
                    })
                    .collect();
                self.log_line(format!(
                    "{} {}",
                    self.subtasks.len(),
                    if self.lang == Lang::Zh {
                        "个子任务"
                    } else {
                        "subtasks"
                    }
                ));
            }
            Event::SubtaskStarted { id, tier } => {
                if let Some(row) = self.subtasks.iter_mut().find(|r| r.id == id) {
                    row.status = RowStatus::Running;
                    row.tier = Some(tier);
                }
                self.current_model = match tier {
                    ModelTier::Strong => "strong".to_string(),
                    ModelTier::Weak => "weak".to_string(),
                };
                self.log_line(format!("▶ {} ({})", id, self.lang.tier(tier)));
            }
            Event::SubtaskDone { id } => {
                if let Some(row) = self.subtasks.iter_mut().find(|r| r.id == id) {
                    row.status = RowStatus::Done;
                }
            }
            Event::Tool { step, name } => {
                self.tools.push_back(format!("[{}] {}", step, name));
                while self.tools.len() > MAX_TOOLS {
                    self.tools.pop_front();
                }
            }
            Event::ToolResult { name, summary } => {
                self.tool_results.push_back(format!("{}: {}", name, summary));
                while self.tool_results.len() > 20 {
                    self.tool_results.pop_front();
                }
            }
            Event::Repair { round } => {
                self.log_line(format!(
                    "🔧 {} #{}",
                    if self.lang == Lang::Zh {
                        "修复"
                    } else {
                        "Repair"
                    },
                    round
                ));
            }
            Event::Review { approved } => {
                self.log_line(if approved {
                    if self.lang == Lang::Zh {
                        "✅ 评审通过"
                    } else {
                        "✅ Review passed"
                    }
                } else {
                    if self.lang == Lang::Zh {
                        "❌ 评审未通过"
                    } else {
                        "❌ Review failed"
                    }
                });
            }
            Event::Cost(c) => self.cost = c,
            Event::Note(s) => {
                self.log_line(s);
            }
            Event::Message { role, content } => {
                if role == "assistant" && !content.is_empty() {
                    self.add_message(content);
                }
            }
            Event::Output(s) => {
                self.output = s;
            }
            Event::Finished => {
                self.finished = true;
                self.phase = Phase::Done;
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// 主入口
// ═══════════════════════════════════════════════════════════════

pub async fn run_with_tui(orch: Orchestrator, task: String) -> Result<Outcome> {
    run_with_tui_and_lang(orch, task, Lang::Zh).await
}

pub async fn run_with_tui_and_lang(
    orch: Orchestrator,
    task: String,
    lang: Lang,
) -> Result<Outcome> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
    let orch = Arc::new(orch.with_events(tx));
    let handle: JoinHandle<Result<Outcome>> = {
        let orch = orch.clone();
        tokio::spawn(async move { orch.run(&task).await })
    };

    let mut terminal = ratatui::init();
    let mut state = TuiState::new(lang);
    let loop_res = run_loop(&mut terminal, &mut state, &mut rx, &handle);
    ratatui::restore();
    loop_res?;

    let outcome = handle.await.context("编排任务 join 失败")?;
    if let Ok(o) = Arc::try_unwrap(orch) {
        o.shutdown().await;
    }
    outcome
}

// ═══════════════════════════════════════════════════════════════
// 渲染循环
// ═══════════════════════════════════════════════════════════════

fn run_loop(
    terminal: &mut DefaultTerminal,
    mut state: &mut TuiState,
    rx: &mut mpsc::UnboundedReceiver<Event>,
    handle: &JoinHandle<Result<Outcome>>,
) -> Result<()> {
    loop {
        while let Ok(ev) = rx.try_recv() {
            state.apply(ev);
        }
        if handle.is_finished() {
            while let Ok(ev) = rx.try_recv() {
                state.apply(ev);
            }
            state.finished = true;
        }
        terminal.draw(|f| ui(f, &mut state))?;
        if event::poll(Duration::from_millis(POLL_MS))? {
            if let CtEvent::Key(k) = event::read()? {
                let quit = k.code == KeyCode::Char('q')
                    || (k.code == KeyCode::Char('c')
                        && k.modifiers.contains(KeyModifiers::CONTROL));
                if quit {
                    return Ok(());
                }
                // L 键切换语言
                if k.code == KeyCode::Char('l') && !k.modifiers.contains(KeyModifiers::CONTROL)
                {
                    state.lang = if state.lang == Lang::Zh {
                        Lang::En
                    } else {
                        Lang::Zh
                    };
                }
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// UI 渲染
// ═══════════════════════════════════════════════════════════════

fn ui(frame: &mut Frame, state: &mut TuiState) {
    let [header, main, footer] = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(0),
        Constraint::Length(2),
    ])
    .areas(frame.area());

    render_header(frame, state, header);
    render_main(frame, state, main);
    render_footer(frame, state, footer);
}

/// 顶部：标题 + 阶段 + 动画 + Token 统计
fn render_header(frame: &mut Frame, state: &mut TuiState, area: Rect) {
    let spinner = state.spinner.tick().to_string();
    let elapsed = state.start_time.elapsed();
    let mins = elapsed.as_secs() / 60;
    let secs = elapsed.as_secs() % 60;

    let c = &state.cost;
    let strong_tok = c.strong_tokens();
    let weak_tok = c.weak_tokens();
    let total_tok = strong_tok + weak_tok;
    let ratio = c.strong_share() * 100.0;

    // 第一行：标题 + 阶段 + 动画
    let line1 = Line::from(vec![
        Span::styled(
            state.lang.title(),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            spinner,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            state.lang.phase(state.phase),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {:02}:{:02}", mins, secs),
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    // 第二行：Token 统计
    let line2 = Line::from(vec![
        Span::styled(
            format!("  {}:", state.lang.tokens_label()),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!(" {} ", strong_tok),
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("[{}]", state.lang.tier(ModelTier::Strong)),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(" + "),
        Span::styled(
            format!(" {} ", weak_tok),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("[{}]", state.lang.tier(ModelTier::Weak)),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(" = "),
        Span::styled(
            format!("{} ", total_tok),
            Style::default().fg(Color::White),
        ),
        Span::styled(
            format!("({} {:.0}%)", state.lang.strong_ratio_label(), ratio),
            Style::default().fg(Color::Yellow),
        ),
    ]);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let paragraph = Paragraph::new(vec![line1, line2]).block(block);
    frame.render_widget(paragraph, area);
}

/// 主体：左侧子任务 + 右侧（消息 + 工具 + 事件）
fn render_main(frame: &mut Frame, state: &mut TuiState, area: Rect) {
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(area);

    // 左侧：子任务列表
    render_subtasks(frame, state, left);

    // 右侧：消息 + 工具 + 事件
    let [msg_area, tool_area, log_area] = Layout::vertical([
        Constraint::Percentage(40),
        Constraint::Percentage(25),
        Constraint::Min(0),
    ])
    .areas(right);

    render_messages(frame, state, msg_area);
    render_tools(frame, state, tool_area);
    render_log(frame, state, log_area);
}

/// 子任务列表
fn render_subtasks(frame: &mut Frame, state: &mut TuiState, area: Rect) {
    let spinner = state.spinner.tick().to_string();
    let lang = state.lang;
    let items: Vec<ListItem> = state
        .subtasks
        .iter()
        .map(|s| {
            let (glyph, style) = match s.status {
                RowStatus::Pending => ("○", Style::default().fg(Color::DarkGray)),
                RowStatus::Running => (
                    spinner.as_str(),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                RowStatus::Done => ("✓", Style::default().fg(Color::Green)),
            };

            let tier_str = s
                .tier
                .map(|t| format!(" [{}]", lang.tier(t)))
                .unwrap_or_default();

            let mut spans = vec![
                Span::styled(format!(" {} ", glyph), style),
                Span::styled(
                    format!("[{}] ", state.lang.difficulty(s.difficulty)),
                    Style::default().fg(Color::Magenta),
                ),
                Span::raw(truncate(&s.desc, 35)),
            ];

            if !tier_str.is_empty() {
                spans.push(Span::styled(
                    tier_str,
                    Style::default().fg(Color::DarkGray),
                ));
            }

            ListItem::new(Line::from(spans))
        })
        .collect();

    let title = format!(" {} ({}/{}) ", state.lang.subtask_label(), 
        state.subtasks.iter().filter(|s| s.status == RowStatus::Done).count(),
        state.subtasks.len());

    frame.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(Style::default().fg(Color::Blue)),
        ),
        area,
    );
}

/// 消息/思考区域
fn render_messages(frame: &mut Frame, state: &mut TuiState, area: Rect) {
    let lines: Vec<Line> = state
        .messages
        .iter()
        .rev()
        .take(20)
        .enumerate()
        .map(|(i, msg)| {
            let style = if i == 0 {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            // 显示多行，不截断
            let display_lines: Vec<Line> = msg
                .lines()
                .take(3)
                .map(|l| Line::styled(truncate(l, 55), style))
                .collect();
            if display_lines.is_empty() {
                vec![Line::raw("")]
            } else {
                display_lines
            }
        })
        .flatten()
        .collect();

    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", state.lang.message_label()))
                .border_style(Style::default().fg(Color::Green)),
        ),
        area,
    );
}

/// 工具调用
fn render_tools(frame: &mut Frame, state: &TuiState, area: Rect) {
    let items: Vec<ListItem> = state
        .tools
        .iter()
        .rev()
        .take(10)
        .map(|t| {
            ListItem::new(Line::styled(
                t.as_str(),
                Style::default().fg(Color::Cyan),
            ))
        })
        .collect();

    frame.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .title(state.lang.tools_label())
                .border_style(Style::default().fg(Color::Yellow)),
        ),
        area,
    );
}

/// 事件日志
fn render_log(frame: &mut Frame, state: &TuiState, area: Rect) {
    let lines: Vec<Line> = state
        .log
        .iter()
        .rev()
        .take(20)
        .map(|l| Line::raw(l.clone()))
        .collect();

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(state.lang.events_label())
                    .border_style(Style::default().fg(Color::Gray)),
            ),
        area,
    );
}

/// 底部状态栏
fn render_footer(frame: &mut Frame, state: &TuiState, area: Rect) {
    let [status_area, output_area] =
        Layout::horizontal([Constraint::Length(15), Constraint::Min(0)]).areas(area);

    // 左侧：状态
    let status = if state.finished {
        Span::styled(
            format!(" {} ", state.lang.finished()),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            format!(" {} ", state.lang.running()),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    };

    frame.render_widget(Paragraph::new(status), status_area);

    // 右侧：产出摘要
    if state.finished && !state.output.is_empty() {
        let output_line = Line::from(vec![
            Span::styled("Output: ", Style::default().fg(Color::DarkGray)),
            Span::styled(truncate(&state.output, 60), Style::default().fg(Color::White)),
        ]);
        frame.render_widget(Paragraph::new(output_line), output_area);
    } else {
        let help = Line::from(vec![
            Span::styled(
                " q:quit  l:lang",
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(help).alignment(ratatui::layout::Alignment::Right),
            output_area,
        );
    }
}

// ═══════════════════════════════════════════════════════════════
// 工具函数
// ═══════════════════════════════════════════════════════════════

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max_chars).collect();
        out.push('…');
        out
    }
}

// ═══════════════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use rc_types::PlannedSubtask;

    #[test]
    fn apply_updates_state() {
        let mut st = TuiState::new(Lang::Zh);
        st.apply(Event::Phase(Phase::Executing));
        assert_eq!(st.phase, Phase::Executing);

        st.apply(Event::Planned(vec![PlannedSubtask {
            id: "s1".into(),
            description: "test".into(),
            difficulty: Difficulty::Hard,
        }]));
        assert_eq!(st.subtasks.len(), 1);

        st.apply(Event::SubtaskStarted {
            id: "s1".into(),
            tier: ModelTier::Strong,
        });
        assert_eq!(st.subtasks[0].status, RowStatus::Running);

        st.apply(Event::SubtaskDone { id: "s1".into() });
        assert_eq!(st.subtasks[0].status, RowStatus::Done);

        let mut cost = Cost::default();
        cost.add(ModelTier::Strong, 10, 5);
        st.apply(Event::Cost(cost));
        assert_eq!(st.cost.strong_tokens(), 15);

        st.apply(Event::Finished);
        assert!(st.finished);
    }

    #[test]
    fn lang_switch() {
        let lang = Lang::En;
        assert_eq!(lang.phase(Phase::Planning), "Planning");
        assert_eq!(lang.tier(ModelTier::Strong), "S");
    }
}
