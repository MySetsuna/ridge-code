//! ratatui 实时视图(M4):后台 task 跑编排发 `Event`,主线程渲染循环收事件、重绘 DAG/进度。
//! `--tui` 开启;不开则走原路径(tracing 日志 + 末尾报告)。

use anyhow::{Context, Result};
use ratatui::crossterm::event::{self, Event as CtEvent, KeyCode, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use rc_core::{Orchestrator, Outcome};
use rc_types::{Cost, Difficulty, Event, ModelTier, Phase};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const MAX_TOOLS: usize = 30;
const MAX_LOG: usize = 200;
const POLL_MS: u64 = 80;

/// 起终端 → 后台跑编排(发事件)→ 主循环渲染 → 恢复终端 → 关 MCP → 返回 Outcome。
pub async fn run_with_tui(orch: Orchestrator, task: String) -> Result<Outcome> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
    let orch = Arc::new(orch.with_events(tx));
    let handle: JoinHandle<Result<Outcome>> = {
        let orch = orch.clone();
        tokio::spawn(async move { orch.run(&task).await })
    };

    // ratatui::init 会装 panic 钩子,panic 时也恢复终端。
    let mut terminal = ratatui::init();
    let mut state = TuiState::new();
    let loop_res = run_loop(&mut terminal, &mut state, &mut rx, &handle);
    ratatui::restore();
    loop_res?;

    let outcome = handle.await.context("编排任务 join 失败")?;
    // 此时后台 task 已结束、其 Arc 已释放,try_unwrap 拿回 Orchestrator 关 MCP。
    if let Ok(o) = Arc::try_unwrap(orch) {
        o.shutdown().await;
    }
    outcome
}

/// 渲染循环(同步:内部只有非阻塞 try_recv + 终端轮询,编排在别的 runtime 线程推进)。
fn run_loop(
    terminal: &mut DefaultTerminal,
    state: &mut TuiState,
    rx: &mut mpsc::UnboundedReceiver<Event>,
    handle: &JoinHandle<Result<Outcome>>,
) -> Result<()> {
    loop {
        while let Ok(ev) = rx.try_recv() {
            state.apply(ev);
        }
        // 编排结束(含出错未发 Finished 的情况)也标记完成,避免卡住。
        if handle.is_finished() {
            while let Ok(ev) = rx.try_recv() {
                state.apply(ev);
            }
            state.finished = true;
        }
        terminal.draw(|f| ui(f, state))?;
        if event::poll(Duration::from_millis(POLL_MS))? {
            if let CtEvent::Key(k) = event::read()? {
                let quit = k.code == KeyCode::Char('q')
                    || (k.code == KeyCode::Char('c')
                        && k.modifiers.contains(KeyModifiers::CONTROL));
                if quit {
                    return Ok(());
                }
            }
        }
    }
}

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
}

/// TUI 状态:被 `Event` 增量更新(纯逻辑,可单测)。
struct TuiState {
    phase: Phase,
    subtasks: Vec<SubtaskRow>,
    tools: VecDeque<String>,
    log: VecDeque<String>,
    cost: Cost,
    finished: bool,
}

impl TuiState {
    fn new() -> Self {
        Self {
            phase: Phase::Planning,
            subtasks: Vec::new(),
            tools: VecDeque::new(),
            log: VecDeque::new(),
            cost: Cost::default(),
            finished: false,
        }
    }

    fn log_line(&mut self, s: impl Into<String>) {
        self.log.push_back(s.into());
        while self.log.len() > MAX_LOG {
            self.log.pop_front();
        }
    }

    /// 应用一个编排事件到状态(TUI 的核心纯逻辑)。
    fn apply(&mut self, ev: Event) {
        match ev {
            Event::Phase(p) => {
                self.phase = p;
                self.log_line(format!("— 阶段: {} —", phase_label(p)));
            }
            Event::Planned(list) => {
                self.subtasks = list
                    .into_iter()
                    .map(|s| SubtaskRow {
                        id: s.id,
                        desc: s.description,
                        difficulty: s.difficulty,
                        status: RowStatus::Pending,
                    })
                    .collect();
                self.log_line(format!("规划出 {} 个子任务", self.subtasks.len()));
            }
            Event::SubtaskStarted { id, tier } => {
                if let Some(row) = self.subtasks.iter_mut().find(|r| r.id == id) {
                    row.status = RowStatus::Running;
                }
                self.log_line(format!("▶ {id}({})", tier_label(tier)));
            }
            Event::SubtaskDone { id } => {
                if let Some(row) = self.subtasks.iter_mut().find(|r| r.id == id) {
                    row.status = RowStatus::Done;
                }
            }
            Event::Tool { step, name } => {
                self.tools.push_back(format!("[{step}] {name}"));
                while self.tools.len() > MAX_TOOLS {
                    self.tools.pop_front();
                }
            }
            Event::Repair { round } => self.log_line(format!("🔧 验证失败,修复第 {round} 轮")),
            Event::Review { approved } => self.log_line(if approved {
                "评审通过"
            } else {
                "评审未通过"
            }),
            Event::Cost(c) => self.cost = c,
            Event::Note(s) => self.log_line(s),
            Event::Finished => {
                self.finished = true;
                self.phase = Phase::Done;
            }
        }
    }
}

fn ui(frame: &mut Frame, state: &TuiState) {
    let [header, main, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    // 顶部:标题 + 阶段 + 实时成本。
    let c = &state.cost;
    let header_line = Line::from(vec![
        Span::styled(
            " ridge-code ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  阶段 "),
        Span::styled(
            phase_label(state.phase),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "   强 {} / 弱 {} tok   强占比 {:.0}%",
            c.strong_tokens(),
            c.weak_tokens(),
            c.strong_share() * 100.0
        )),
    ]);
    frame.render_widget(Paragraph::new(header_line).block(Block::bordered()), header);

    let [left, right] =
        Layout::horizontal([Constraint::Percentage(48), Constraint::Percentage(52)]).areas(main);

    // 左:子任务 DAG。
    let items: Vec<ListItem> = state
        .subtasks
        .iter()
        .map(|s| {
            let (glyph, style) = match s.status {
                RowStatus::Pending => ("○", Style::default().fg(Color::DarkGray)),
                RowStatus::Running => (
                    "▶",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                RowStatus::Done => ("✓", Style::default().fg(Color::Green)),
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{glyph} "), style),
                Span::styled(
                    format!("[{}] ", difficulty_label(s.difficulty)),
                    Style::default().fg(Color::Magenta),
                ),
                Span::raw(truncate(&s.desc, 40)),
            ]))
        })
        .collect();
    frame.render_widget(
        List::new(items).block(Block::bordered().title(" 子任务 DAG ")),
        left,
    );

    // 右:最近工具调用 + 事件日志。
    let [tools_area, log_area] =
        Layout::vertical([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(right);
    let tool_items: Vec<ListItem> = state
        .tools
        .iter()
        .rev()
        .map(|t| ListItem::new(t.as_str()))
        .collect();
    frame.render_widget(
        List::new(tool_items).block(Block::bordered().title(" 最近工具调用 ")),
        tools_area,
    );
    let log_lines: Vec<Line> = state
        .log
        .iter()
        .rev()
        .map(|l| Line::raw(l.clone()))
        .collect();
    frame.render_widget(
        Paragraph::new(log_lines)
            .wrap(Wrap { trim: true })
            .block(Block::bordered().title(" 事件 ")),
        log_area,
    );

    // 底部提示。
    let foot = if state.finished {
        " ✅ 完成 — 按 q 退出 "
    } else {
        " 运行中…  按 q / Ctrl-C 退出 "
    };
    frame.render_widget(
        Paragraph::new(foot).style(Style::default().fg(Color::DarkGray)),
        footer,
    );
}

fn phase_label(p: Phase) -> &'static str {
    match p {
        Phase::Planning => "规划",
        Phase::Executing => "执行",
        Phase::Verifying => "验证",
        Phase::Reviewing => "评审",
        Phase::Done => "完成",
    }
}

fn tier_label(t: ModelTier) -> &'static str {
    match t {
        ModelTier::Strong => "强",
        ModelTier::Weak => "弱",
    }
}

fn difficulty_label(d: Difficulty) -> &'static str {
    match d {
        Difficulty::Trivial => "易",
        Difficulty::Moderate => "中",
        Difficulty::Hard => "难",
    }
}

/// 按字符截断(中文安全),超长加省略号。
fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max_chars).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rc_types::PlannedSubtask;

    #[test]
    fn apply_updates_phase_subtasks_cost() {
        let mut st = TuiState::new();
        st.apply(Event::Phase(Phase::Executing));
        assert_eq!(st.phase, Phase::Executing);

        st.apply(Event::Planned(vec![PlannedSubtask {
            id: "s1".into(),
            description: "干活".into(),
            difficulty: Difficulty::Hard,
        }]));
        assert_eq!(st.subtasks.len(), 1);
        assert_eq!(st.subtasks[0].status, RowStatus::Pending);

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

        assert!(!st.finished);
        st.apply(Event::Finished);
        assert!(st.finished);
        assert_eq!(st.phase, Phase::Done);
    }

    #[test]
    fn tool_buffer_is_capped() {
        let mut st = TuiState::new();
        for i in 0..(MAX_TOOLS + 10) {
            st.apply(Event::Tool {
                step: i,
                name: "read_file".into(),
            });
        }
        assert_eq!(st.tools.len(), MAX_TOOLS);
    }

    #[test]
    fn truncate_is_char_safe() {
        assert_eq!(truncate("abc", 5), "abc");
        assert_eq!(truncate("一二三四五六", 3), "一二三…");
    }
}
