//! RidgeCode 的交互式终端界面 —— **主屏内联 REPL**(iter-26)。
//! 不再霸占备用屏:历史内容经 `Terminal::insert_before` 静态提交进终端原生 scrollback
//! (原生滚动/选取/搜索全保留),ratatui 只渲染底部一小块 Live 视口(状态行 + 流式尾巴 + 输入框)。
//! 执行图跑在后台 Tokio task,token 流、工具事件和权限门都不会卡住界面(iter-23 事件驱动主环)。

use std::io;
use std::sync::{mpsc, Arc};
use std::time::Duration;

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
        let _ = execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
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
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
        let _ = execute!(io::stdout(), event::DisableBracketedPaste);
        let _ = disable_raw_mode();
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

/// 应用滚动增量到偏移(u16 饱和)。审批模态看长 diff 用。
fn apply_scroll(scroll: u16, delta: i16) -> u16 {
    if delta >= 0 {
        scroll.saturating_add(delta as u16)
    } else {
        scroll.saturating_sub(delta.unsigned_abs())
    }
}

/// 主输入态对一次按键的**纯决策**(续 iter-22 `approval_action` 模式):副作用由主环执行。
/// iter-27:模态优先级 = 审批(在主环上游) > 补全浮窗 > 输入编辑。
/// Shift/Alt+Enter、Ctrl+J → 换行(CSI u 下 Shift 精确;Alt+Enter 免协议全平台通,
/// Ctrl+J 在 unix legacy 与 Enter 同字节故仅作兼收);首行 Up = 历史召回(转换函数惯例)。
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum InputAction {
    Insert(char),
    Backspace,
    Left,
    Right,
    Home,
    End,
    NewLine,
    Submit,
    Interrupt,
    CursorUpOrHistory,
    CursorDownOrHistory,
    PopupOpen,
    PopupNext,
    PopupPrev,
    PopupApply,
    PopupClose,
    Ignore,
}

fn input_action(key: &KeyEvent, busy: bool, popup_open: bool) -> InputAction {
    if key.kind != KeyEventKind::Press {
        return InputAction::Ignore;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return InputAction::Interrupt;
    }
    if popup_open {
        // 浮窗态:↑↓/Tab 选、Enter 应用、Esc 关;字符/退格穿透继续编辑(主环先关浮窗)。
        return match key.code {
            KeyCode::Tab | KeyCode::Down => InputAction::PopupNext,
            KeyCode::Up => InputAction::PopupPrev,
            KeyCode::Enter => InputAction::PopupApply,
            KeyCode::Char(c) => InputAction::Insert(c),
            KeyCode::Backspace => InputAction::Backspace,
            _ => InputAction::PopupClose,
        };
    }
    match key.code {
        KeyCode::Enter
            if key.modifiers.contains(KeyModifiers::SHIFT)
                || key.modifiers.contains(KeyModifiers::ALT) =>
        {
            InputAction::NewLine
        }
        KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => InputAction::NewLine,
        KeyCode::Enter if !busy => InputAction::Submit,
        KeyCode::Tab => InputAction::PopupOpen,
        KeyCode::Char(c) => InputAction::Insert(c),
        KeyCode::Backspace => InputAction::Backspace,
        KeyCode::Left => InputAction::Left,
        KeyCode::Right => InputAction::Right,
        KeyCode::Home => InputAction::Home,
        KeyCode::End => InputAction::End,
        KeyCode::Up => InputAction::CursorUpOrHistory,
        KeyCode::Down => InputAction::CursorDownOrHistory,
        _ => InputAction::Ignore,
    }
}

// ───────────────────────── 输入状态机(iter-27)─────────────────────────

/// 多行输入编辑器:单 String 缓冲 + 字符光标 + 会话内历史召回。全纯方法、离线可测。
/// 光标按**逻辑行**('\n')计,折行内微移不做(ponytail:要所见即所得再算折行几何)。
#[derive(Default)]
struct InputState {
    buffer: String,
    /// 光标 = 字符偏移(非字节;`byte_at` 换算)。
    cursor: usize,
    history: Vec<String>,
    hist_idx: Option<usize>,
    /// 召回历史前暂存的未提交草稿(Down 到底还原)。
    draft: String,
}

impl InputState {
    fn byte_at(&self, char_idx: usize) -> usize {
        self.buffer
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(self.buffer.len())
    }
    fn insert(&mut self, c: char) {
        let b = self.byte_at(self.cursor);
        self.buffer.insert(b, c);
        self.cursor += 1;
    }
    fn insert_str(&mut self, s: &str) {
        let b = self.byte_at(self.cursor);
        self.buffer.insert_str(b, s);
        self.cursor += s.chars().count();
    }
    fn backspace(&mut self) {
        if self.cursor > 0 {
            let b = self.byte_at(self.cursor - 1);
            self.buffer.remove(b);
            self.cursor -= 1;
        }
    }
    fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }
    fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.buffer.chars().count());
    }
    fn home(&mut self) {
        let (_, col) = self.row_col();
        self.cursor -= col;
    }
    fn end(&mut self) {
        let chars: Vec<char> = self.buffer.chars().collect();
        let mut i = self.cursor;
        while i < chars.len() && chars[i] != '\n' {
            i += 1;
        }
        self.cursor = i;
    }
    /// 光标所在 (逻辑行, 列)。
    fn row_col(&self) -> (usize, usize) {
        let (mut row, mut col) = (0, 0);
        for c in self.buffer.chars().take(self.cursor) {
            if c == '\n' {
                row += 1;
                col = 0;
            } else {
                col += 1;
            }
        }
        (row, col)
    }
    fn rows(&self) -> usize {
        self.buffer.chars().filter(|c| *c == '\n').count() + 1
    }
    fn line_len(&self, row: usize) -> usize {
        self.buffer
            .split('\n')
            .nth(row)
            .map(|l| l.chars().count())
            .unwrap_or(0)
    }
    fn cursor_to(&mut self, row: usize, col: usize) {
        let col = col.min(self.line_len(row));
        let mut idx = 0;
        for (r, line) in self.buffer.split('\n').enumerate() {
            if r == row {
                idx += col;
                break;
            }
            idx += line.chars().count() + 1; // +1 = 换行符本身
        }
        self.cursor = idx;
    }
    /// 上移一逻辑行(列钳位);已在首行 → false(调用方转历史召回)。
    fn move_up(&mut self) -> bool {
        let (row, col) = self.row_col();
        if row == 0 {
            return false;
        }
        self.cursor_to(row - 1, col);
        true
    }
    /// 下移一逻辑行;已在末行 → false(调用方转历史前进/还原草稿)。
    fn move_down(&mut self) -> bool {
        let (row, col) = self.row_col();
        if row + 1 >= self.rows() {
            return false;
        }
        self.cursor_to(row + 1, col);
        true
    }
    fn recall_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        match self.hist_idx {
            None => {
                self.draft = std::mem::take(&mut self.buffer);
                self.hist_idx = Some(self.history.len() - 1);
            }
            Some(0) => {}
            Some(i) => self.hist_idx = Some(i - 1),
        }
        if let Some(i) = self.hist_idx {
            self.buffer = self.history[i].clone();
            self.cursor = self.buffer.chars().count();
        }
    }
    fn recall_next(&mut self) {
        match self.hist_idx {
            None => {}
            Some(i) if i + 1 < self.history.len() => {
                self.hist_idx = Some(i + 1);
                self.buffer = self.history[i + 1].clone();
                self.cursor = self.buffer.chars().count();
            }
            Some(_) => {
                self.hist_idx = None;
                self.buffer = std::mem::take(&mut self.draft);
                self.cursor = self.buffer.chars().count();
            }
        }
    }
    /// 提交:取走全文,非空入历史,复位光标/召回态。
    fn take(&mut self) -> String {
        let s = std::mem::take(&mut self.buffer);
        self.cursor = 0;
        self.hist_idx = None;
        self.draft.clear();
        if !s.trim().is_empty() {
            self.history.push(s.clone());
        }
        s
    }
}

// ───────────────────────── 补全浮窗(iter-27)─────────────────────────

/// 斜杠命令静态表(补全数据源,与 `run_command` 分支对齐;有序稳态)。
const SLASH_COMMANDS: &[&str] = &[
    "/agent",
    "/compact",
    "/config",
    "/cost",
    "/exit",
    "/help",
    "/model",
    "/provider",
    "/quit",
    "/reset",
    "/tools",
];

struct Popup {
    items: Vec<String>,
    selected: usize,
    /// 被补全词的起始**字符**偏移(应用时替换 [anchor, cursor))。
    anchor: usize,
}

/// 光标前当前词(空白定界):(起始字符偏移, 词)。
fn current_word(buffer: &str, cursor: usize) -> (usize, String) {
    let chars: Vec<char> = buffer.chars().collect();
    let end = cursor.min(chars.len());
    let mut start = end;
    while start > 0 && !chars[start - 1].is_whitespace() {
        start -= 1;
    }
    (start, chars[start..end].iter().collect())
}

/// 前缀过滤 + 排序(有序稳态)。
fn filter_prefix<'a>(cands: impl IntoIterator<Item = &'a str>, prefix: &str) -> Vec<String> {
    let mut v: Vec<String> = cands
        .into_iter()
        .filter(|c| c.starts_with(prefix))
        .map(str::to_owned)
        .collect();
    v.sort();
    v
}

/// `@` 路径候选:词的目录部分单层 `read_dir`(不递归,防 IO 卡 UI),前缀过滤,目录带 `/`。
fn path_candidates(part: &str) -> Vec<String> {
    let (dir, prefix) = match part.rfind('/') {
        Some(i) => (&part[..=i], &part[i + 1..]),
        None => ("", part),
    };
    let read_at = if dir.is_empty() { "." } else { dir };
    let Ok(rd) = std::fs::read_dir(read_at) else {
        return Vec::new();
    };
    let mut v: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.starts_with(prefix) {
                return None;
            }
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            Some(format!("{dir}{name}{}", if is_dir { "/" } else { "" }))
        })
        .collect();
    v.sort();
    v.truncate(20); // 有界:防巨目录撑爆浮窗
    v
}

/// Tab 触发:行首 `/` 词补命令;词内 `@` 补路径(候选带回词前缀,应用时整词替换)。
fn build_popup(input: &InputState) -> Option<Popup> {
    let (anchor, word) = current_word(&input.buffer, input.cursor);
    if word.starts_with('/') && anchor == 0 {
        let items = filter_prefix(SLASH_COMMANDS.iter().copied(), &word);
        return (!items.is_empty()).then_some(Popup {
            items,
            selected: 0,
            anchor,
        });
    }
    if let Some(at) = word.rfind('@') {
        let items: Vec<String> = path_candidates(&word[at + 1..])
            .into_iter()
            .map(|p| format!("{}@{p}", &word[..at]))
            .collect();
        return (!items.is_empty()).then_some(Popup {
            items,
            selected: 0,
            anchor,
        });
    }
    None
}

/// 应用选中项:替换 [anchor, cursor) 区间的词,光标落在补全末尾。
fn apply_completion(input: &mut InputState, popup: &Popup) {
    let sel = popup.items[popup.selected].clone();
    let start_b = input.byte_at(popup.anchor);
    let end_b = input.byte_at(input.cursor);
    input.buffer.replace_range(start_b..end_b, &sel);
    input.cursor = popup.anchor + sel.chars().count();
}

/// 要不要画这一帧:有状态变更(dirty)或 busy(spinner 需动)才画;空闲零重绘(iter-23)。
fn should_draw(dirty: bool, busy: bool) -> bool {
    dirty || busy
}

/// 折行行数(iter-26 抽取,`input_height`/`commit_height` 共用):按字符数近似显示宽。
/// ponytail: CJK 宽字符按 1 格计,偏差可容;要精确再引 wcwidth 口径。
fn wrapped_rows(content: &str, width: u16) -> usize {
    let w = width.max(1) as usize;
    content
        .split('\n')
        .map(|l| l.chars().count().div_ceil(w).max(1))
        .sum()
}

/// 静态提交一段文本需占的终端行数(供 `insert_before`)。
fn commit_height(text: &str, width: u16) -> u16 {
    wrapped_rows(text, width).min(u16::MAX as usize).max(1) as u16
}

/// 粘贴净化(iter-24):CRLF/CR 归一 LF,滤除其余控制字符(留 \n \t),防转义序列注入输入框。
fn sanitize_paste(s: &str) -> String {
    s.replace("\r\n", "\n")
        .chars()
        .map(|c| if c == '\r' { '\n' } else { c })
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

/// 动态输入框高度(iter-24):按内容折行数伸缩,clamp 在 [min,max](计入上下边框 2 行)。
fn input_height(content: &str, width: u16, min: u16, max: u16) -> u16 {
    (wrapped_rows(content, width).min(u16::MAX as usize) as u16)
        .saturating_add(2)
        .clamp(min, max)
}

/// 流式尾巴:Live 视口只显示正在生成文本的最后 `k` 行(前面的行等 Superstep 后整段历史化)。
fn stream_tail(stream: &str, k: usize) -> Vec<&str> {
    let lines: Vec<&str> = stream.lines().collect();
    let start = lines.len().saturating_sub(k);
    lines[start..].to_vec()
}

/// TODO 进度 (done, total);空清单 → None(状态行不显)。
fn todo_progress(todos: &[Todo]) -> Option<(usize, usize)> {
    if todos.is_empty() {
        return None;
    }
    let done = todos.iter().filter(|t| t.status == "completed").count();
    Some((done, todos.len()))
}

/// TODO 清单渲染成静态提交块(变更时整段落进终端历史,取代旧侧边栏面板)。
fn render_todo_block(todos: &[Todo]) -> String {
    todos
        .iter()
        .map(|t| {
            let mark = match t.status.as_str() {
                "completed" => "✓",
                "in_progress" => "~",
                _ => " ",
            };
            format!("[{mark}] {}", t.content)
        })
        .collect::<Vec<_>>()
        .join("\n")
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
    input: InputState,
    /// 补全浮窗(iter-27):Some = 浮窗开(键位模态优先级:审批 > 浮窗 > 输入)。
    popup: Option<Popup>,
    /// 待静态提交队列(iter-26):`note` 只入队,主环 drain 经 `insert_before` 写进终端原生历史。
    /// 队列是瞬态的(每圈清空),无需环形上限 —— 有界性由「提交即出队」保证。
    commits: Vec<(String, Color)>,
    stream: String,
    todos: Vec<Todo>,
    scroll: u16,
    busy: bool,
    phase: String,
    frame: usize,
}
impl Ui {
    fn note(&mut self, text: impl Into<String>, color: Color) {
        self.commits.push((text.into(), color));
    }
    fn drain_commits(&mut self) -> Vec<(String, Color)> {
        std::mem::take(&mut self.commits)
    }
}

/// 把积压的历史行静态提交进终端 scrollback(iter-26 核心):行一经 `insert_before`
/// 即成原生历史,永不参与后续帧的差分重绘 —— Live 视口恒小,闪烁根因根除。
fn flush_commits(terminal: &mut Term, ui: &mut Ui) -> io::Result<()> {
    let width = terminal.size()?.width;
    for (text, color) in ui.drain_commits() {
        let h = commit_height(&text, width);
        terminal.insert_before(h, |buf| {
            let lines: Vec<Line> = text
                .lines()
                .map(|l| Line::from(Span::styled(l.to_owned(), Style::default().fg(color))))
                .collect();
            Paragraph::new(Text::from(lines))
                .wrap(Wrap { trim: false })
                .render(buf.area, buf);
        })?;
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
        "RidgeCode  ·  内联模式:输出直接落进终端历史(原生滚动/选取)· Enter 发送 · Ctrl-C 中断 · /help",
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
        // 静态提交先于绘制:历史行离开 Live 视口,进终端 scrollback。
        if !ui.commits.is_empty() {
            flush_commits(&mut terminal, &mut ui)?;
            dirty = true;
        }
        if should_draw(dirty, ui.busy) {
            ui.frame = ui.frame.wrapping_add(1);
            terminal.draw(|frame| draw(frame, &ui, &meta, session_tokens, pending.as_ref()))?;
            dirty = false;
        }
        // 事件驱动多路复用替代固定轮询(iter-23):无事时阻塞挂起,不烧 CPU。
        tokio::select! {
            biased;
            Some(ev) = key_rx.recv() => {
                dirty = true; // 键盘/粘贴/resize 皆需重绘
                if let Event::Paste(s) = &ev {
                    // BPM(iter-24):整块原子注入(iter-27 起并入 InputState 光标处事务)。
                    ui.popup = None;
                    ui.input.insert_str(&sanitize_paste(s));
                    continue;
                }
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
                match input_action(&key, ui.busy, ui.popup.is_some()) {
                    InputAction::Interrupt => {
                        if let Some(handle) = task.take() {
                            handle.abort();
                            *bus.lock().unwrap() = None;
                            ui.busy = false;
                            ui.stream.clear();
                            ui.note("已中断当前任务", Color::Yellow);
                        }
                    }
                    InputAction::Insert(c) => {
                        ui.popup = None; // 字符穿透:关浮窗继续编辑
                        ui.input.insert(c);
                    }
                    InputAction::Backspace => {
                        ui.popup = None;
                        ui.input.backspace();
                    }
                    InputAction::Left => ui.input.left(),
                    InputAction::Right => ui.input.right(),
                    InputAction::Home => ui.input.home(),
                    InputAction::End => ui.input.end(),
                    InputAction::NewLine => ui.input.insert('\n'),
                    InputAction::CursorUpOrHistory => {
                        // 转换函数惯例(iter-27):非首行 = 光标上移;首行 = 历史召回。
                        if !ui.input.move_up() {
                            ui.input.recall_prev();
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
                        let input = ui.input.take();
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
                        // TODO 变更 → 清单快照静态提交进历史(取代旧侧边栏面板)。
                        if render_todo_block(&state.todos) != render_todo_block(&ui.todos)
                            && !state.todos.is_empty()
                        {
                            ui.note(render_todo_block(&state.todos), Color::Cyan);
                        }
                        ui.todos = state.todos;
                        // 流式已完段落随 Superstep 消息历史化,Live 只留尾巴。
                        ui.stream.clear();
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
        "/help" => ui.note("/exit /reset /compact /cost /tools /model [name] /provider /agent /config [set key value]；@path 引用文件；Ctrl-C 中断；历史滚动/选取用终端原生能力；批准弹窗:y/Enter 批准、n/Esc 拒绝、↑↓ 滚动看详情。", Color::Gray),
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

/// Live 视口绘制(iter-26):状态行 + 流式尾巴 + 输入框;审批模态覆整个视口。
fn draw(
    frame: &mut ratatui::Frame,
    ui: &Ui,
    meta: &ReplMeta,
    tokens: usize,
    approval: Option<&ApprovalRequest>,
) {
    let area = frame.area();
    let input_rows = input_height(&ui.input.buffer, area.width.saturating_sub(2), 3, 8);
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(input_rows),
        ])
        .split(area);
    let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"][ui.frame % 10];
    let status = if ui.busy {
        format!(" {spinner} {}", ui.phase)
    } else {
        " ready".into()
    };
    let todo = todo_progress(&ui.todos)
        .map(|(d, n)| format!(" · todo {d}/{n}"))
        .unwrap_or_default();
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
                " {} · {} · {} tokens{todo} · {}{}",
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
    // 流式尾巴:只画最后 K 行(已完段落随 Superstep 静态提交进历史)。
    let k = outer[1].height as usize;
    let tail: Vec<Line> = stream_tail(&ui.stream, k)
        .into_iter()
        .map(|s| {
            Line::from(Span::styled(
                s.to_owned(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ))
        })
        .collect();
    frame.render_widget(Paragraph::new(Text::from(tail)), outer[1]);
    frame.render_widget(
        Paragraph::new(ui.input.buffer.as_str())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" 输入（Enter 发送 · Shift/Alt+Enter 换行 · Tab 补全）"),
            )
            .wrap(Wrap { trim: false }),
        outer[2],
    );
    // 真光标(iter-27):按逻辑行列落在输入框内(钳在框内;审批模态时不显)。
    if approval.is_none() {
        let (row, col) = ui.input.row_col();
        let inner = outer[2];
        let x = (inner.x + 1 + col as u16).min(inner.right().saturating_sub(2));
        let y = (inner.y + 1 + row as u16).min(inner.bottom().saturating_sub(2));
        frame.set_cursor_position(Position { x, y });
    }
    // 补全浮窗(iter-27):输入框上方,有界尺寸,高亮选中项。
    if let Some(p) = &ui.popup {
        let h = (p.items.len().min(6) as u16) + 2;
        let w = p
            .items
            .iter()
            .map(|s| s.chars().count())
            .max()
            .unwrap_or(10)
            .min(48) as u16
            + 4;
        let x = outer[2].x + 1;
        let y = outer[2].y.saturating_sub(h);
        let rect = Rect {
            x,
            y,
            width: w.min(area.width.saturating_sub(x)),
            height: h.min(area.height),
        };
        frame.render_widget(Clear, rect);
        let items: Vec<ListItem> = p.items.iter().map(|s| ListItem::new(s.as_str())).collect();
        let mut state = ListState::default();
        state.select(Some(p.selected));
        frame.render_stateful_widget(
            List::new(items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Tab/↑↓ 选 · Enter 用 · Esc 关 "),
                )
                .highlight_style(
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            rect,
            &mut state,
        );
    }
    if let Some(req) = approval {
        // 审批模态覆整个 Live 视口;↑↓ 滚动看长 diff(scroll 接到 Paragraph 偏移)。
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(format!(
                "⚠ 允许执行 {}？\n\n{}\n\ny/Enter: 批准    n/Esc: 拒绝    ↑↓/PgUp/PgDn: 滚动看详情",
                req.action, req.detail
            ))
            .style(Style::default().fg(Color::Yellow))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" 需要权限 ")
                    .border_style(Style::default().fg(Color::Yellow)),
            )
            .wrap(Wrap { trim: false })
            .scroll((ui.scroll, 0)),
            area,
        );
    }
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

    /// iter-27:主输入键位路由矩阵 —— Shift/Alt+Enter/Ctrl+J 换行,Up/Down 归光标/历史枢纽,
    /// busy 时 Enter 不提交,浮窗态 ↑↓/Tab/Enter/Esc 归浮窗、字符穿透,松键忽略。
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
        assert_eq!(
            input_action(&press(KeyCode::Enter), true, false),
            InputAction::Ignore
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
        // 浮窗态
        assert_eq!(
            input_action(&press(KeyCode::Tab), false, true),
            InputAction::PopupNext
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
            InputAction::PopupApply
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
                &KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                true,
                true
            ),
            InputAction::Interrupt
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

    /// iter-27:词提取 + 前缀过滤 + 应用替换 + build_popup 触发条件。
    #[test]
    fn completion_word_filter_and_apply() {
        assert_eq!(current_word("/mo", 3), (0, "/mo".to_string()));
        assert_eq!(current_word("fix @src/ma", 11), (4, "@src/ma".to_string()));
        assert_eq!(
            filter_prefix(SLASH_COMMANDS.iter().copied(), "/co"),
            vec![
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
        q.insert_str("/re");
        let pop = build_popup(&q).expect("应有候选");
        assert_eq!(pop.items, vec!["/reset".to_string()]);
        assert_eq!(pop.anchor, 0);
        let mut r = InputState::default();
        r.insert_str("say /re");
        assert!(build_popup(&r).is_none());
    }

    /// iter-23:重绘判定 —— 脏或 busy(spinner)才画,空闲零重绘。
    #[test]
    fn draw_only_when_dirty_or_busy() {
        assert!(should_draw(true, false));
        assert!(should_draw(false, true));
        assert!(!should_draw(false, false));
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
    #[test]
    fn input_height_grows_and_clamps() {
        assert_eq!(input_height("", 80, 3, 8), 3);
        assert_eq!(input_height("hi", 80, 3, 8), 3);
        assert_eq!(input_height(&"x".repeat(85), 80, 3, 8), 4);
        assert_eq!(input_height("a\nb\nc", 80, 3, 8), 5);
        assert_eq!(input_height(&"a\n".repeat(30), 80, 3, 8), 8);
        assert_eq!(input_height("abc", 0, 3, 8), 5);
    }

    /// iter-26:流式尾巴 —— 少于 K 全量,多于 K 取尾。
    #[test]
    fn stream_tail_takes_last_k_lines() {
        assert_eq!(stream_tail("a\nb\nc", 5), vec!["a", "b", "c"]);
        assert_eq!(stream_tail("a\nb\nc\nd\ne\nf", 3), vec!["d", "e", "f"]);
        assert!(stream_tail("", 3).is_empty());
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
}
