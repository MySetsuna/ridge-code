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
    /// busy 时提交 → 入队(iter-33),当前任务毕自动接跑。
    Queue,
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
        // busy 时 Enter → 入队(iter-33),空闲 → 立即提交。
        KeyCode::Enter if busy => InputAction::Queue,
        KeyCode::Enter => InputAction::Submit,
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
    /// 光标所在 (逻辑行, 字符列)。
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
    /// 光标所在 (逻辑行, **显示单元格列**)(iter-30):CJK/emoji 按实占 2 格累加,
    /// 真光标据此落点 —— 修「中文输入光标不落末端、偏左」根因。
    fn cursor_display_col(&self) -> (usize, usize) {
        let (mut row, mut col) = (0, 0);
        for c in self.buffer.chars().take(self.cursor) {
            if c == '\n' {
                row += 1;
                col = 0;
            } else {
                col += char_cells(c);
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
    "/jailbreak",
    "/login",
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

// ───────────────────────── 交互页 Panel(iter-35)─────────────────────────

/// 交互页类别:决定 Enter 动作与提示文案。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PanelKind {
    /// 配置页:Enter 就地编辑选中键的值。
    Config,
    /// Provider 页:Enter 热切换到选中档。
    Provider,
    /// 工具页:只读浏览 + 搜索。
    Tools,
    /// 模型页:Enter 热切换到选中模型 + 缓存 ctx_window。
    Models,
    /// Sub-agent 页:只读浏览 + 搜索。
    Agent,
    /// 登录页(iter-38):↑↓ 选内置供应商 → Enter 就地输入 key(掩码)→ Enter 校验并接入。
    Login,
}

/// 一行:动作键(config 键 / provider 名 / 模型 id / 工具名 / agent 名)+ 右列值 + (模型)上下文窗口。
struct PanelRow {
    key: String,
    value: String,
    ctx: Option<u64>,
}

/// 模态交互页(iter-35):标题 + 搜索框(随打随滤)+ 过滤列表(选中高亮)+ 选中动作。
/// `view` 是 `rows` 的过滤下标,`sel` 是 `view` 内位次;`editing=Some` = 配置页正编辑选中键值。
struct Panel {
    kind: PanelKind,
    title: String,
    query: String,
    rows: Vec<PanelRow>,
    view: Vec<usize>,
    sel: usize,
    editing: Option<String>,
}

/// 过滤:key/value 不分大小写子串命中;空 query = 全含。有序稳态(保 rows 原序)。纯函数。
fn panel_filter(rows: &[PanelRow], query: &str) -> Vec<usize> {
    let q = query.to_lowercase();
    rows.iter()
        .enumerate()
        .filter(|(_, r)| {
            q.is_empty() || r.key.to_lowercase().contains(&q) || r.value.to_lowercase().contains(&q)
        })
        .map(|(i, _)| i)
        .collect()
}

impl Panel {
    fn new(kind: PanelKind, title: String, rows: Vec<PanelRow>) -> Self {
        let view = panel_filter(&rows, "");
        Panel {
            kind,
            title,
            query: String::new(),
            rows,
            view,
            sel: 0,
            editing: None,
        }
    }
    /// query 变更后重算 view 并把 sel 钳回范围内。
    fn retype(&mut self) {
        self.view = panel_filter(&self.rows, &self.query);
        if self.sel >= self.view.len() {
            self.sel = self.view.len().saturating_sub(1);
        }
    }
    fn move_up(&mut self) {
        self.sel = self.sel.saturating_sub(1);
    }
    fn move_down(&mut self) {
        if self.sel + 1 < self.view.len() {
            self.sel += 1;
        }
    }
    fn selected(&self) -> Option<&PanelRow> {
        self.view.get(self.sel).map(|&i| &self.rows[i])
    }
}

/// 当前 config 某标量键的显示值(空 → 空串,由调用方替 `(未设)`)。
fn config_value(cfg: &Config, key: &str) -> String {
    match key {
        "provider" => cfg.provider.clone().unwrap_or_default(),
        "model" => cfg.model.clone().unwrap_or_default(),
        "base_url" => cfg.base_url.clone().unwrap_or_default(),
        "budget_tokens" => cfg.budget_tokens.map(|n| n.to_string()).unwrap_or_default(),
        "skills_dir" => cfg.skills_dir.clone().unwrap_or_default(),
        "skip_danger" => cfg.skip_danger.map(|b| b.to_string()).unwrap_or_default(),
        "status_bar" => cfg.status_bar.clone().unwrap_or_default(),
        "allow_jailbreak" => cfg
            .allow_jailbreak
            .map(|b| b.to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// 配置页:行取自 `CONFIG_KEYS`,值现读现显(缺 → `(未设)`)。
fn config_panel() -> Panel {
    let cfg = Config::load(config_path());
    let rows = agent::CONFIG_KEYS
        .iter()
        .map(|k| {
            let v = config_value(&cfg, k);
            PanelRow {
                key: (*k).to_string(),
                value: if v.is_empty() { "(unset)".into() } else { v },
                ctx: None,
            }
        })
        .collect();
    Panel::new(
        PanelKind::Config,
        "Config · ↑↓ select · Enter edit · type to filter · Esc close".into(),
        rows,
    )
}

/// Provider 页:列命名档(名 · kind · model)。
fn provider_panel() -> Panel {
    let cfg = Config::load(config_path());
    let rows = cfg
        .providers
        .iter()
        .map(|p| PanelRow {
            key: p.name.clone(),
            value: format!("{} · {}", p.kind, p.model),
            ctx: None,
        })
        .collect();
    Panel::new(
        PanelKind::Provider,
        "Provider · ↑↓ select · Enter switch · Esc close".into(),
        rows,
    )
}

/// 工具页(只读):列工具名。
fn tools_panel(tools: &[String]) -> Panel {
    let rows = tools
        .iter()
        .map(|t| PanelRow {
            key: t.clone(),
            value: String::new(),
            ctx: None,
        })
        .collect();
    Panel::new(
        PanelKind::Tools,
        "Tools (read-only) · type to filter · Esc close".into(),
        rows,
    )
}

/// 模型页:列实时模型(id · ctx),`sel` 落当前模型;`ctx` 携真实窗口供选中缓存。
fn models_panel(list: &[provider::models::ModelInfo], current: &str) -> Panel {
    let rows = list
        .iter()
        .map(|m| PanelRow {
            key: m.id.clone(),
            value: format!(
                "ctx {}",
                m.context.map(fmt_ctx).unwrap_or_else(|| "?".into())
            ),
            ctx: m.context,
        })
        .collect();
    let mut p = Panel::new(
        PanelKind::Models,
        "Models · ↑↓ select · Enter switch · type to filter · Esc close".into(),
        rows,
    );
    if let Some(pos) = p.view.iter().position(|&i| p.rows[i].key == current) {
        p.sel = pos;
    }
    p
}

/// Sub-agent 页(只读):列 agent 名 + 描述。
fn agent_panel(defs: &[agent::Agent]) -> Panel {
    let rows = defs
        .iter()
        .map(|a| PanelRow {
            key: a.name.clone(),
            value: a.description.clone(),
            ctx: None,
        })
        .collect();
    Panel::new(
        PanelKind::Agent,
        "Sub-agents (read-only) · type to filter · Esc close".into(),
        rows,
    )
}

/// 登录页(iter-38):列 14 家内置供应商 preset(id · label · model),Enter 选中后就地输入 key。
fn login_panel() -> Panel {
    let rows = PROVIDER_PRESETS
        .iter()
        .map(|p| PanelRow {
            key: p.id.to_string(),
            value: format!("{} · {}", p.label, p.default_model),
            ctx: None,
        })
        .collect();
    Panel::new(
        PanelKind::Login,
        "Login · ↑↓ pick provider · Enter enter key · type to filter · Esc close".into(),
        rows,
    )
}

/// 交互页键路由(iter-35,纯函数):模态优先级 审批 > Panel > 浮窗 > 输入。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PanelAction {
    Up,
    Down,
    Enter,
    Esc,
    Char(char),
    Backspace,
    Ignore,
}

fn panel_action(key: &KeyEvent) -> PanelAction {
    if key.kind != KeyEventKind::Press {
        return PanelAction::Ignore;
    }
    match key.code {
        KeyCode::Up => PanelAction::Up,
        KeyCode::Down => PanelAction::Down,
        KeyCode::Enter => PanelAction::Enter,
        KeyCode::Esc => PanelAction::Esc,
        KeyCode::Backspace => PanelAction::Backspace,
        KeyCode::Char(c) => PanelAction::Char(c),
        _ => PanelAction::Ignore,
    }
}

/// 要不要画这一帧:有状态变更(dirty)或 busy(spinner 需动)才画;空闲零重绘(iter-23)。
fn should_draw(dirty: bool, busy: bool) -> bool {
    dirty || busy
}

/// 单字符终端单元格宽度(wcwidth 口径):CJK/emoji=2、控制/零宽=0、常规=1(iter-30)。
fn char_cells(c: char) -> usize {
    unicode_width::UnicodeWidthChar::width(c).unwrap_or(0)
}

/// 字符串显示单元格宽度(iter-30):替代 `.chars().count()`,CJK/emoji 按实占计。
fn str_cells(s: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(s)
}

/// 折行行数(iter-26 抽取,`input_height`/`commit_height` 共用):按**显示单元格宽**折行
/// (iter-30 起用 wcwidth 口径,CJK 实占 2 格,不再低估行数致边框撕裂)。
fn wrapped_rows(content: &str, width: u16) -> usize {
    let w = width.max(1) as usize;
    content
        .split('\n')
        .map(|l| str_cells(l).div_ceil(w).max(1))
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

// ───────────────────────── 视觉与反馈(iter-28)─────────────────────────

/// 语义化色角色(iter-28):界面色一律经角色取 **ANSI 16 具名色**,零 RGB 硬编码 ——
/// 尊重用户终端主题(浅色/高对比/透明背景下不悲剧)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// 品牌/焦点:提示符、状态行徽标、浮窗高亮、流式游标。
    Primary,
    /// 用户命令回显(`›`)。
    Command,
    /// 系统信息、事件默认色。
    Info,
    Success,
    Error,
    Warn,
    /// 面板/围栏边框。
    Border,
    /// 次要文本、代码块内文。
    Muted,
    DiffAdd,
    DiffDel,
}

fn role_color(r: Role) -> Color {
    match r {
        Role::Primary => Color::Cyan,
        Role::Command => Color::LightGreen,
        Role::Info => Color::LightBlue,
        Role::Success => Color::Green,
        Role::Error => Color::Red,
        Role::Warn => Color::Yellow,
        Role::Border => Color::DarkGray,
        Role::Muted => Color::Gray,
        Role::DiffAdd => Color::Green,
        Role::DiffDel => Color::Red,
    }
}

/// 行内 md 扫描:`` `code` ``(Warn 色)与 `**bold**`(加粗);未闭合记号按字面。纯函数。
fn inline_md_spans(text: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut rest = text;
    loop {
        let tick = rest.find('`');
        let bold = rest.find("**");
        let (pos, is_tick) = match (tick, bold) {
            (None, None) => {
                if !rest.is_empty() {
                    spans.push(Span::raw(rest.to_owned()));
                }
                break;
            }
            (Some(t), Some(b)) if t <= b => (t, true),
            (Some(t), None) => (t, true),
            (_, Some(b)) => (b, false),
        };
        if pos > 0 {
            spans.push(Span::raw(rest[..pos].to_owned()));
        }
        if is_tick {
            match rest[pos + 1..].find('`') {
                Some(end) => {
                    let inner = &rest[pos + 1..pos + 1 + end];
                    spans.push(Span::styled(
                        inner.to_owned(),
                        Style::default().fg(role_color(Role::Warn)),
                    ));
                    rest = &rest[pos + 1 + end + 1..];
                }
                None => {
                    spans.push(Span::raw(rest[pos..].to_owned()));
                    break;
                }
            }
        } else {
            match rest[pos + 2..].find("**") {
                Some(end) => {
                    let inner = &rest[pos + 2..pos + 2 + end];
                    spans.push(Span::styled(
                        inner.to_owned(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ));
                    rest = &rest[pos + 2 + end + 2..];
                }
                None => {
                    spans.push(Span::raw(rest[pos..].to_owned()));
                    break;
                }
            }
        }
    }
    spans
}

/// 行级 md 轻渲染(iter-28,**只在静态提交时染** —— 样式定型才历史化):
/// ``` 围栏切态(围栏行 Border 色)、块内 Muted、`#` 标题加粗 Primary、余走行内扫描。
fn md_line_spans(line: &str, in_code: bool) -> (Vec<Span<'static>>, bool) {
    let trimmed = line.trim_start();
    if trimmed.starts_with("```") {
        return (
            vec![Span::styled(
                line.to_owned(),
                Style::default().fg(role_color(Role::Border)),
            )],
            !in_code,
        );
    }
    if in_code {
        return (
            vec![Span::styled(
                line.to_owned(),
                Style::default().fg(role_color(Role::Muted)),
            )],
            true,
        );
    }
    if trimmed.starts_with('#') {
        return (
            vec![Span::styled(
                line.to_owned(),
                Style::default()
                    .fg(role_color(Role::Primary))
                    .add_modifier(Modifier::BOLD),
            )],
            false,
        );
    }
    (inline_md_spans(line), false)
}

/// 呈现层折叠上限(iter-28):静态提交前超限留头 + 尾标,历史不刷屏。
/// (内核 `bound_observation` 已在源头有界,此为第二道收敛。)
const FOLD_MAX: usize = 20;

fn fold_lines(text: &str, max: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max {
        return text.to_owned();
    }
    let hidden = lines.len() - max;
    let mut out = lines[..max].join("\n");
    out.push_str(&format!("\n… (+{hidden} lines folded)"));
    out
}

/// 启动 banner(iter-28):ASCII 安全字符(单格宽),经 `splash_frame` 列渐显 ≈1s。
const SPLASH: &[&str] = &[
    r"  ____  _     _            ____          _      ",
    r" |  _ \(_) __| | __ _  ___/ ___|___   __| | ___ ",
    r" | |_) | |/ _` |/ _` |/ _ \ |   / _ \ / _` |/ _ \",
    r" |  _ <| | (_| | (_| |  __/ |__| (_) | (_| |  __/",
    r" |_| \_\_|\__,_|\__, |\___|\____\___/ \__,_|\___|",
    r"                |___/                            ",
];
const SPLASH_TICKS: usize = 14;
/// banner 最大行宽(用于居中与折行守卫)。
const SPLASH_W: usize = 48;

/// 帧序列纯函数:第 `tick`/`total` 帧显示前 (maxw·tick/total) 列。首帧零字形,末帧全幅,单调渐显。
fn splash_frame(tick: usize, total: usize) -> String {
    let maxw = SPLASH.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let cols = maxw * tick / total.max(1);
    SPLASH
        .iter()
        .map(|l| l.chars().take(cols).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// banner 在 `width` 内水平居中的左空白列数(纯函数)。
fn splash_pad(width: usize) -> usize {
    width.saturating_sub(SPLASH_W) / 2
}

/// 每行前置 `pad` 空格(动画帧居中用)。
fn indent(text: &str, pad: usize) -> String {
    let p = " ".repeat(pad);
    text.lines()
        .map(|l| format!("{p}{l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 落定 banner 块(iter-36,纯函数,防「标识乱了」):
/// 终端宽 ≥ banner 宽 → **居中** ASCII 艺术字(逐行 `trim_end` 去尾空格,故每行 ≤ width 不折行)+ 英文 tagline;
/// 窄于 banner 宽 → 退化紧凑单行标题(极窄也不折行)。
fn splash_block(width: usize) -> Vec<String> {
    if width < SPLASH_W {
        return vec!["◆ RidgeCode".to_string()];
    }
    let pad = " ".repeat(splash_pad(width));
    let mut out: Vec<String> = SPLASH
        .iter()
        .map(|l| format!("{pad}{}", l.trim_end()))
        .collect();
    let tag = "modular general-purpose agent framework";
    let tpad = " ".repeat(width.saturating_sub(tag.chars().count()) / 2);
    out.push(String::new());
    out.push(format!("{tpad}{tag}"));
    out
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
    /// 启动帧序列进度(iter-28):< SPLASH_TICKS 时 tick 驱动渐显,末帧 banner 入历史。
    splash: usize,
    /// 本任务流式 token 估算累计(iter-31):token_rx 每块 `est_tokens` 累加,Submit 清零、done 保留展示。
    stream_tokens: usize,
    /// 排队待跑的提交(iter-33):busy 时 Enter 入队,任务 done 后自动取队首接跑;中断清空。
    queued: VecDeque<String>,
    /// 交互页(iter-35):Some = 模态页开(键位模态优先级:审批 > Panel > 浮窗 > 输入)。
    panel: Option<Panel>,
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
        let text = fold_lines(&text, FOLD_MAX); // 呈现层折叠(iter-28):历史不刷屏
                                                // 块间前置一空白行(iter-31 需求 5):连续输出块视觉分栏,不再贴成一片。
        let h = commit_height(&text, width) + 1;
        terminal.insert_before(h, |buf| {
            let mut lines: Vec<Line> = vec![Line::default()];
            if text.starts_with("🤖") {
                // 终答走 md 轻渲染(iter-28):样式已定型,提交时染。
                let mut in_code = false;
                lines.extend(text.lines().map(|l| {
                    let (spans, next) = md_line_spans(l, in_code);
                    in_code = next;
                    Line::from(spans)
                }));
            } else {
                lines.extend(
                    text.lines().map(|l| {
                        Line::from(Span::styled(l.to_owned(), Style::default().fg(color)))
                    }),
                );
            }
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
        "RidgeCode  ·  inline mode: output lands in terminal history (native scroll/select) · Enter to send · Ctrl-C to interrupt · /help",
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
                    session_tokens,
                    session_turns,
                )
                .await?
                {
                    break 'main;
                }
                if !input.starts_with('/') {
                    ui.note(format!("› {input}"), role_color(Role::Command));
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
                    ui.phase = "reasoning".into();
                    ui.stream.clear();
                    ui.stream_tokens = 0;
                    task_started = Some(Instant::now());
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
                                match id.as_deref().and_then(preset_by_id) {
                                    Some(preset) if !key.trim().is_empty() => {
                                        ui.note(format!("verifying {}…", preset.id), Color::Gray);
                                        login_apply_verified(preset, key.trim(), &mut meta, &swap, &mut ui).await;
                                    }
                                    Some(_) => ui.note("enter a non-empty API key", Color::Yellow),
                                    None => ui.note("no provider selected", Color::Red),
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
                            ui.stream.clear();
                            task_started = None;
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
            Some(token) = token_rx.recv() => {
                ui.busy = true;
                ui.stream_tokens += est_tokens(&token);
                ui.stream.push_str(&token);
                // 批量排空积压 token,免逐 token 一帧。
                while let Ok(t) = token_rx.try_recv() {
                    ui.stream_tokens += est_tokens(&t);
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
                task_started = None;
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
                    Err(e) => ui.note(format!("error: {e}"), Color::Red),
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
                        ui.stream.clear();
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

/// 当前 API key 解析(供 `/models` 抓取、`/model` 热切用):env `RIDGE_API_KEY` > config 顶层内联
/// `api_key` > 顶层 `key_env`→(env 或 auth.json 密钥库,`login --default` 情形)。都无 → None。
fn current_api_key() -> Option<String> {
    if let Some(k) = std::env::var("RIDGE_API_KEY")
        .ok()
        .filter(|v| !v.is_empty())
    {
        return Some(k);
    }
    let cfg = Config::load(config_path());
    if let Some(k) = cfg
        .api_key
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return Some(k);
    }
    cfg.key_env
        .as_deref()
        .and_then(|name| resolve_key_env(name, &load_auth()))
}

/// TUI `/login` 落盘核(与 CLI `run_login` 同语义,精简版):key → auth.json(收权限),
/// 档案 + 顶层默认 → config.json。**key 不回显、不进 config**。热切由调用方做。
fn tui_login(preset: &agent::ProviderPreset, key: &str) -> Result<(), String> {
    let apath = auth_path();
    if let Some(dir) = std::path::Path::new(&apath).parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let atext = std::fs::read_to_string(&apath).unwrap_or_default();
    std::fs::write(&apath, auth_upsert(&atext, preset.key_env, key)).map_err(|e| e.to_string())?;
    secure_file(&apath);
    let cpath = config_path();
    let ctext = std::fs::read_to_string(&cpath).unwrap_or_default();
    let out = apply_login(&ctext, preset, None, None, true)?;
    std::fs::write(&cpath, out).map_err(|e| e.to_string())
}

/// TUI 登录落地(iter-38):**先校验连通** → 成功则写 auth+config(`tui_login`)+ 热切 + note ✓ + 关页;
/// 失败 note ✗ 不落盘。共用于 `/login <id> <key>` 快捷路径与登录页提交。校验期短暂阻塞在 await
/// (有效 key 通常 <2s,15s 仅超时上限)。ponytail: 需非阻塞再引入后台校验任务通道。
async fn login_apply_verified(
    preset: &agent::ProviderPreset,
    key: &str,
    meta: &mut ReplMeta,
    swap: &Arc<SwapProvider>,
    ui: &mut Ui,
) {
    match verify_provider_key(preset.kind, preset.base_url, key).await {
        Ok(n) => match tui_login(preset, key) {
            Ok(()) => {
                swap.swap(make_provider(
                    preset.kind,
                    preset.default_model,
                    preset.base_url,
                    key.to_string(),
                ));
                meta.provider = preset.kind.to_string();
                meta.model = preset.default_model.to_string();
                meta.base_url = preset.base_url.to_string();
                ui.panel = None;
                ui.note(
                    format!(
                        "✓ connected to {} ({n} models) · now active (model {})",
                        preset.label, preset.default_model
                    ),
                    Color::Green,
                );
            }
            Err(e) => ui.note(format!("verified but write failed: {e}"), Color::Red),
        },
        Err(e) => ui.note(
            format!("✗ could not connect to {}: {e}", preset.label),
            Color::Red,
        ),
    }
}

/// 热切换模型(iter-32 共用路径):密钥经 `current_api_key`(env 优先,回落 config 内联)——
/// `/model <name>` 文本命令与模型选择器浮窗同走此路,顺带修「内联 key 无法切模型」根因。
fn swap_model(swap: &Arc<SwapProvider>, meta: &mut ReplMeta, model: &str, ui: &mut Ui) {
    match current_api_key() {
        Some(key) => {
            swap.swap(make_provider(&meta.provider, model, &meta.base_url, key));
            meta.model = model.to_string();
            ui.note(format!("switched model={model}"), Color::Green);
        }
        None => ui.note(
            "no API key resolved (set RIDGE_API_KEY or api_key at config.json top level); cannot switch model",
            Color::Red,
        ),
    }
}

/// 热切换到命名 provider 档(iter-35 抽取):`/provider use` 与 Provider 页共用。
/// 密钥走档案 `key_env`(env);缺则红字提示,不切。
fn switch_provider(name: &str, meta: &mut ReplMeta, swap: &Arc<SwapProvider>, ui: &mut Ui) {
    match Config::load(config_path())
        .providers
        .into_iter()
        .find(|p| p.name == name)
    {
        Some(p) => match p.resolve_key_with(&load_auth()) {
            Some(key) => {
                swap.swap(make_provider(&p.kind, &p.model, &p.base_url, key));
                meta.provider = p.kind;
                meta.model = p.model;
                meta.base_url = p.base_url;
                ui.note(format!("switched provider {name}"), Color::Green);
            }
            None => ui.note(
                format!(
                    "no key for {} ({}); run: ridgecode login",
                    p.name, p.key_env
                ),
                Color::Red,
            ),
        },
        None => ui.note(format!("no such provider: {name}"), Color::Red),
    }
}

/// 配置页就地编辑后 live 应用(iter-35):可即时生效的键立即作用于运行态,余键仅持久化(下次启动生效)。
fn apply_config_live(
    key: &str,
    val: &str,
    meta: &mut ReplMeta,
    swap: &Arc<SwapProvider>,
    ui: &mut Ui,
) {
    match key {
        "allow_jailbreak" => agent::set_allow_jailbreak(val == "true"),
        "model" => swap_model(swap, meta, val, ui),
        "provider" | "base_url" => {
            if key == "provider" {
                meta.provider = val.to_string();
            } else {
                meta.base_url = val.to_string();
            }
            if let Some(k) = current_api_key() {
                swap.swap(make_provider(
                    &meta.provider,
                    &meta.model,
                    &meta.base_url,
                    k,
                ));
            }
        }
        "status_bar" => {
            meta.status_bar = if val.trim().is_empty() {
                DEFAULT_STATUS_BAR.to_string()
            } else {
                val.to_string()
            }
        }
        _ => {} // budget_tokens/skills_dir/skip_danger:仅持久化,下次启动生效。
    }
}

/// 交互页 Enter 动作分派(iter-35):先把选中项数据 clone 出(释放对 `ui.panel` 的不可变借用),
/// 再按 kind/编辑态改 `ui`/`meta`/热切换。Config=进/提交编辑;Models=切模型;Provider=切档;Tools/Agent=只读关页。
fn panel_enter(ui: &mut Ui, meta: &mut ReplMeta, swap: &Arc<SwapProvider>) {
    let Some(panel) = ui.panel.as_ref() else {
        return;
    };
    let kind = panel.kind;
    let editing = panel.editing.clone();
    let sel_key = panel.selected().map(|r| r.key.clone());
    let sel_val = panel.selected().map(|r| r.value.clone());
    let sel_ctx = panel.selected().and_then(|r| r.ctx);
    match (kind, editing) {
        // 配置页:提交编辑 → 持久化 + live 应用 + 刷新页(退编辑态)。
        (PanelKind::Config, Some(newval)) => {
            let Some(key) = sel_key else { return };
            let val = newval.trim().to_string();
            match persist_config(&key, &val) {
                Ok(_) => {
                    apply_config_live(&key, &val, meta, swap, ui);
                    ui.note(format!("saved {key}={val}"), Color::Green);
                    ui.panel = Some(config_panel());
                }
                Err(e) => {
                    ui.note(format!("write failed: {e}"), Color::Red);
                    if let Some(p) = ui.panel.as_mut() {
                        p.editing = None;
                    }
                }
            }
        }
        // 配置页:进入编辑 → 预填当前值(「(未设)」视为空)。
        (PanelKind::Config, None) => {
            let cur = sel_val.map(|v| if v == "(unset)" { String::new() } else { v });
            if let (Some(p), Some(cur)) = (ui.panel.as_mut(), cur) {
                p.editing = Some(cur);
            }
        }
        // 模型页:切到选中模型 + 缓存其真实上下文窗口。
        (PanelKind::Models, _) => {
            if let Some(id) = sel_key {
                if let Some(w) = sel_ctx {
                    meta.ctx_window = w;
                }
                swap_model(swap, meta, &id, ui);
                ui.panel = None;
            }
        }
        // Provider 页:切到选中档。
        (PanelKind::Provider, _) => {
            if let Some(name) = sel_key {
                switch_provider(&name, meta, swap, ui);
                ui.panel = None;
            }
        }
        // 登录页:Enter 选中 preset → 起 key 输入态(标题提示选了哪家)。Some 分支(提交 key)由主环
        // 异步处理(校验联网),不达此。
        (PanelKind::Login, None) => {
            if let (Some(p), Some(id)) = (ui.panel.as_mut(), sel_key) {
                p.editing = Some(String::new());
                p.title =
                    format!("Login · enter API key for {id} · Enter verify & connect · Esc cancel");
            }
        }
        (PanelKind::Login, Some(_)) => {}
        // 只读页:Enter 关页。
        (PanelKind::Tools, _) | (PanelKind::Agent, _) => ui.panel = None,
    }
}

/// 上下文窗口人读化:200000 → "200K",1048576 → "1.0M"(纯函数)。
fn fmt_ctx(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{}K", n / 1_000)
    } else {
        n.to_string()
    }
}

// ───────────────────── 状态双栏与 ctx%(iter-31)─────────────────────

/// ctx% 分母未知时的兜底上下文窗口(现代模型常见档)。`/models` 命中当前模型即被真实窗口覆盖。
pub(super) const DEFAULT_CTX_WINDOW: u64 = 200_000;
/// 输入框下方自定义状态条内置默认模板。config `status_bar` 留空时用它。
pub(super) const DEFAULT_STATUS_BAR: &str = " {provider} · {model} · ctx {ctx} · {tokens} tok ";

/// 实时 token 速率(tok/s,纯函数):elapsed 为 0 → 0,防除零。
fn token_rate(tokens: usize, elapsed_ms: u128) -> u64 {
    if elapsed_ms == 0 {
        0
    } else {
        (tokens as u128 * 1000 / elapsed_ms) as u64
    }
}

/// 上下文占用百分比(纯函数):window 为 0 → 0;上限 100(压缩前估算,超窗即封顶)。
fn ctx_percent(used: usize, window: usize) -> u16 {
    if window == 0 {
        0
    } else {
        (used * 100 / window).min(100) as u16
    }
}

/// 忙碌粘条文案(需求 6,纯函数):运行态 · 读秒 · token 消耗 · 速率 · 任务进度 · 待跑队列。
/// todo 空则省略进度段;`queued>0` 追加 ` · ⏳N`(iter-33)。计时/计量全由入参给定 —— 零 wall-clock,可纯测。
fn fmt_busy_bar(
    phase: &str,
    todos: &[Todo],
    elapsed_s: u64,
    tokens: usize,
    rate: u64,
    queued: usize,
) -> String {
    let mut s = format!("⚡ {phase} · ⏱ {elapsed_s}s · {tokens} tok · {rate} tok/s");
    if let Some((d, n)) = todo_progress(todos) {
        s.push_str(&format!(" · todo {d}/{n}"));
    }
    if queued > 0 {
        s.push_str(&format!(" · ⏳{queued}"));
    }
    s
}

/// 自定义底栏占位替换用变量(需求 3)。
struct StatusVars {
    provider: String,
    model: String,
    ctx: String,
    tokens: String,
    cwd: String,
}

/// 底栏模板渲染(需求 3,纯函数):替换 `{provider}{model}{ctx}{tokens}{cwd}`,
/// 未知占位原样保留(不吞字符,便于用户排错)。
fn render_status_template(tmpl: &str, v: &StatusVars) -> String {
    tmpl.replace("{provider}", &v.provider)
        .replace("{model}", &v.model)
        .replace("{ctx}", &v.ctx)
        .replace("{tokens}", &v.tokens)
        .replace("{cwd}", &v.cwd)
}

/// 当前工作目录末段名(状态栏用),取不到 → 空串。
fn cwd_name() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|x| x.to_string_lossy().to_string()))
        .unwrap_or_default()
}

/// 一帧的实时体征(iter-31):由主环据 `Instant`/token 计量算好后传入 `draw`,
/// draw 只消费数值 —— 计时逻辑不入 draw,便于纯测各格式化函数。
struct Vitals {
    elapsed_s: u64,
    task_tokens: usize,
    rate: u64,
    /// 当前 history 估算 token(ctx% 分子)。
    ctx_used: usize,
    /// 待跑排队条数(iter-33),忙碌粘条显 ⏳N。
    queued: usize,
}

#[allow(clippy::too_many_arguments)]
async fn run_command(
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
        "/help" => ui.note("/exit /reset /compact /cost /tools /login [list|<id> <key>] /model [<name>] (no arg = live model picker) /provider [list|use <name>|add <name> <kind> <model> <base_url> [key_env]] /agent /config [set key value] /jailbreak [on|off]; @path to reference a file; Ctrl-C to interrupt; scroll/select history with the terminal's native keys; approval prompt: y/Enter approve, n/Esc reject, ↑↓ scroll details.", Color::Gray),
        "/tools" => ui.panel = Some(tools_panel(&meta.tools)),
        "/reset" => { history.clear(); save_session(&session_path(), history); ui.note("context cleared", Color::Yellow); }
        "/compact" => { let n = history.len(); *history = compact_history(std::mem::take(history), 4); ui.note(format!("context compacted: {n} → {} messages", history.len()), Color::Yellow); }
        "/cost" => ui.note(format!("session total: {tokens} tokens · {turns} tasks"), Color::Gray),
        // /model 单命令(iter-37 合并):无参 → 实时模型页(↑↓ 选、Enter 切);`/model <name>` → 直接热切。
        // `/models` `/model pick` 保留为别名(旧肌肉记忆),补全表只呈现 `/model`。
        _ if input == "/model" || input == "/models" || input == "/model pick" => {
            match current_api_key() {
                Some(key) => {
                    let http = provider::http::ReqwestClient::new();
                    let fut = provider::models::fetch_models(&http, &meta.provider, &meta.base_url, &key);
                    match tokio::time::timeout(Duration::from_secs(15), fut).await {
                        Ok(Ok(list)) if !list.is_empty() => {
                            // 命中当前模型即缓存其真实上下文窗口 → 顶/底栏 ctx% 分母转真值(iter-31)。
                            if let Some(n) = list.iter().find(|m| m.id == meta.model).and_then(|m| m.context) {
                                meta.ctx_window = n;
                            }
                            ui.panel = Some(models_panel(&list, &meta.model));
                        }
                        Ok(Ok(_)) => ui.note("endpoint returned an empty model list", Color::Yellow),
                        Ok(Err(e)) => ui.note(format!("failed to fetch models: {e}"), Color::Red),
                        Err(_) => ui.note("fetching models timed out (15s)", Color::Red),
                    }
                }
                None => ui.note("no API key resolved (set RIDGE_API_KEY or api_key at config.json top level)", Color::Red),
            }
        }
        _ if input.starts_with("/model ") => swap_model(swap, meta, input[7..].trim(), ui),
        _ if input == "/jailbreak" => {
            let on = agent::allow_jailbreak();
            ui.note(if on { "jailbreak: ON ⚠ (can write outside cwd subtree; disaster commands / protected paths / read-only still blocked). Disable: /jailbreak off" } else { "jailbreak: OFF (writes limited to cwd subtree). Enable: /jailbreak on —— top status bar turns red when on" }, if on { Color::Red } else { Color::Gray });
        }
        _ if input == "/jailbreak on" => { agent::set_allow_jailbreak(true); ui.note("⚠ jailbreak ON: can write outside cwd subtree (disaster commands / protected paths / read-only still hard-blocked). Session only; to persist: /config set allow_jailbreak true", Color::Red); }
        _ if input == "/jailbreak off" => { agent::set_allow_jailbreak(false); ui.note("jailbreak OFF: writes limited back to cwd subtree", Color::Green); }
        _ if input == "/config" => ui.panel = Some(config_panel()),
        _ if input.starts_with("/config set ") => { let parts: Vec<_> = input.splitn(4, ' ').collect(); if parts.len() == 4 { match persist_config(parts[2], parts[3]) { Ok(path) => ui.note(format!("wrote {path}; takes effect next start"), Color::Green), Err(e) => ui.note(format!("write failed: {e}"), Color::Red) } } else { ui.note("usage: /config set <key> <value>", Color::Yellow); } }
        _ if input == "/provider" || input == "/provider list" => {
            let cfg = Config::load(config_path());
            if cfg.providers.is_empty() { ui.note("no provider profiles. Add: /provider add <name> <kind> <model> <base_url> [key_env]", Color::Gray); }
            else { ui.panel = Some(provider_panel()); }
        }
        _ if input.starts_with("/provider add ") => {
            match agent::parse_provider_add(input["/provider add ".len()..].trim()) {
                Ok(profile) => {
                    let path = config_path();
                    let text = std::fs::read_to_string(&path).unwrap_or_default();
                    match agent::config_add_provider(&text, &profile) {
                        Ok(out) => match std::fs::write(&path, out) {
                            Ok(_) => ui.note(format!("added provider \"{}\" → {} (switch: /provider use {}; set the API key in env var {})", profile.name, path, profile.name, profile.key_env), Color::Green),
                            Err(e) => ui.note(format!("failed to write config: {e}"), Color::Red),
                        },
                        Err(e) => ui.note(format!("config transform failed: {e}"), Color::Red),
                    }
                }
                Err(e) => ui.note(e, Color::Yellow),
            }
        }
        _ if input.starts_with("/provider use ") => switch_provider(input[14..].trim(), meta, swap, ui),
        _ if input == "/login" => ui.panel = Some(login_panel()),
        _ if input == "/login list" => {
            let ids: Vec<&str> = PROVIDER_PRESETS.iter().map(|p| p.id).collect();
            ui.note(format!("built-in providers: {}\ninteractive: /login  ·  quick: /login <id> <API_KEY> (verified; key → ~/.ridge/auth.json, not config)", ids.join(", ")), Color::Gray);
        }
        _ if input.starts_with("/login ") => {
            let rest = input["/login ".len()..].trim();
            let mut it = rest.split_whitespace();
            match (it.next(), it.next()) {
                (Some(id), Some(key)) => match preset_by_id(id) {
                    Some(preset) => login_apply_verified(preset, key, meta, swap, ui).await,
                    None => ui.note(format!("unknown provider \"{id}\"; see /login list"), Color::Yellow),
                },
                _ => ui.note("usage: /login <id> <API_KEY>, or just /login to pick interactively", Color::Yellow),
            }
        }
        _ if input == "/agent" => {
            if agents.defs.is_empty() { ui.note("no sub-agents available", Color::Gray); }
            else { ui.panel = Some(agent_panel(&agents.defs)); }
        }
        _ if input.starts_with('/') => ui.note(format!("unknown command: {input} (/help)"), Color::Yellow),
        _ => return Ok(false),
    }
    Ok(false)
}

/// 交互页模态绘制(iter-35):居中框(≤80 宽)= 搜索/编辑行 + 过滤列表(选中高亮)+ 提示行。
fn draw_panel(frame: &mut ratatui::Frame, area: Rect, panel: &Panel) {
    let w = area.width.saturating_sub(4).clamp(20, 80);
    let h = area.height.saturating_sub(2).max(6);
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    let rect = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", panel.title))
        .border_style(Style::default().fg(role_color(Role::Primary)));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // 搜索/编辑行
            Constraint::Min(1),    // 列表
            Constraint::Length(1), // 提示
        ])
        .split(inner);
    // 搜索行(编辑态显编辑缓冲;登录页的 key 输入掩码,防肩窥)。
    let (head, head_color) = match &panel.editing {
        Some(buf) if panel.kind == PanelKind::Login => (
            format!("✎ API key: {}", "•".repeat(buf.chars().count())),
            role_color(Role::Warn),
        ),
        Some(buf) => (format!("✎ new value: {buf}"), role_color(Role::Warn)),
        None => (format!("🔍 {}", panel.query), role_color(Role::Muted)),
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            head,
            Style::default().fg(head_color),
        ))),
        rows[0],
    );
    // 过滤列表:key 左对齐 + 右列值。
    let items: Vec<ListItem> = panel
        .view
        .iter()
        .map(|&i| {
            let r = &panel.rows[i];
            let line = if r.value.is_empty() {
                r.key.clone()
            } else {
                format!("{:<18} {}", r.key, r.value)
            };
            ListItem::new(line)
        })
        .collect();
    let mut state = ListState::default();
    state.select((!panel.view.is_empty()).then_some(panel.sel));
    frame.render_stateful_widget(
        List::new(items).highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(role_color(Role::Primary))
                .add_modifier(Modifier::BOLD),
        ),
        rows[1],
        &mut state,
    );
    let hint = if panel.editing.is_some() {
        if panel.kind == PanelKind::Login {
            "Enter verify & connect · Esc cancel"
        } else {
            "Enter save · Esc cancel"
        }
    } else {
        match panel.kind {
            PanelKind::Config => "↑↓ select · Enter edit · type to filter · Esc close",
            PanelKind::Models | PanelKind::Provider => {
                "↑↓ select · Enter switch · type to filter · Esc close"
            }
            PanelKind::Login => "↑↓ pick provider · Enter enter key · type to filter · Esc close",
            PanelKind::Tools | PanelKind::Agent => "↑↓ scroll · type to filter · Esc close",
        }
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(role_color(Role::Muted)),
        ))),
        rows[2],
    );
}

/// Live 视口绘制(iter-26;iter-31 双状态栏):顶状态行 + 输出尾 + [忙碌粘条] + 输入框 + 自定义底栏;
/// 审批模态覆整个视口。五槽定长布局,忙碌槽空闲时高 0(索引恒定,免条件分支乱套)。
fn draw(
    frame: &mut ratatui::Frame,
    ui: &Ui,
    meta: &ReplMeta,
    tokens: usize,
    vitals: &Vitals,
    approval: Option<&ApprovalRequest>,
) {
    let area = frame.area();
    let input_rows = input_height(&ui.input.buffer, area.width.saturating_sub(2), 3, 8);
    let busy_rows = if ui.busy { 1 } else { 0 };
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),          // [0] 顶状态行
            Constraint::Min(1),             // [1] 输出尾
            Constraint::Length(busy_rows),  // [2] 忙碌粘条(空闲高 0)
            Constraint::Length(input_rows), // [3] 输入框
            Constraint::Length(1),          // [4] 自定义底栏
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
    let ctx = ctx_percent(vitals.ctx_used, meta.ctx_window as usize);
    // 顶状态行更显眼(iter-31 需求 3):busy 时徽标转暖色示运行、加 ctx% 段。
    let badge_bg = if ui.busy { Color::Yellow } else { Color::Cyan };
    let mut top: Vec<Span> = vec![Span::styled(
        " RidgeCode ",
        Style::default()
            .fg(Color::Black)
            .bg(badge_bg)
            .add_modifier(Modifier::BOLD),
    )];
    // 地址越狱红标(iter-34):放宽 cwd 沙箱时红底黑字警示,一眼可见。
    if agent::allow_jailbreak() {
        top.push(Span::styled(
            " ⚠JAILBREAK ",
            Style::default()
                .fg(Color::Black)
                .bg(role_color(Role::Error))
                .add_modifier(Modifier::BOLD),
        ));
    }
    top.push(Span::raw(format!(
        " {} · {} · ctx {ctx}% · {} tokens{todo} · {}{}",
        meta.provider,
        meta.model,
        tokens,
        cwd_name(),
        status
    )));
    frame.render_widget(
        Paragraph::new(Line::from(top)).style(Style::default().bg(Color::DarkGray)),
        outer[0],
    );
    // 流式尾巴:只画最后 K 行(已完段落随 Superstep 静态提交进历史)。
    let k = outer[1].height as usize;
    let mut tail: Vec<Line> = stream_tail(&ui.stream, k)
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
    // 流式呼吸游标(iter-28):busy 时尾缀青色 █,按帧奇偶 BOLD/DIM 交替。
    if ui.busy {
        let cursor = Span::styled(
            "█",
            Style::default().fg(role_color(Role::Primary)).add_modifier(
                if ui.frame.is_multiple_of(2) {
                    Modifier::BOLD
                } else {
                    Modifier::DIM
                },
            ),
        );
        match tail.last_mut() {
            Some(last) => last.spans.push(cursor),
            None => tail.push(Line::from(cursor)),
        }
    }
    frame.render_widget(Paragraph::new(Text::from(tail)), outer[1]);
    // 忙碌粘条(iter-31 需求 6):输入框上方,运行态·读秒·token·速率·任务进度。仅 busy 时有高度。
    if ui.busy {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                fmt_busy_bar(
                    &ui.phase,
                    &ui.todos,
                    vitals.elapsed_s,
                    vitals.task_tokens,
                    vitals.rate,
                    vitals.queued,
                ),
                Style::default()
                    .fg(Color::Black)
                    .bg(role_color(Role::Warn))
                    .add_modifier(Modifier::BOLD),
            )))
            .style(Style::default().bg(role_color(Role::Warn))),
            outer[2],
        );
    }
    frame.render_widget(
        Paragraph::new(ui.input.buffer.as_str())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(role_color(Role::Border)))
                    .title(" Input (Enter send · Shift/Alt+Enter newline · Tab complete) "),
            )
            .wrap(Wrap { trim: false }),
        outer[3],
    );
    // 自定义底栏(iter-31 需求 3):输入框下方,config `status_bar` 模板渲染。
    let sv = StatusVars {
        provider: meta.provider.clone(),
        model: meta.model.clone(),
        ctx: format!("{ctx}%"),
        tokens: tokens.to_string(),
        cwd: cwd_name(),
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            render_status_template(&meta.status_bar, &sv),
            Style::default().fg(role_color(Role::Muted)),
        )))
        .style(Style::default().bg(Color::DarkGray)),
        outer[4],
    );
    // 真光标(iter-27;iter-30 改按显示单元格列):CJK/emoji 宽字符落点精确,不再偏左。
    // 审批 / 交互页模态开时不落输入光标(iter-35)。
    if approval.is_none() && ui.panel.is_none() {
        let (row, col) = ui.input.cursor_display_col();
        let inner = outer[3];
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
            .map(|s| str_cells(s))
            .max()
            .unwrap_or(10)
            .min(48) as u16
            + 4;
        let x = outer[3].x + 1;
        let y = outer[3].y.saturating_sub(h);
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
                        .title(" Tab/↑↓ select · Enter use · Esc close "),
                )
                .highlight_style(
                    Style::default()
                        .fg(Color::Black)
                        .bg(role_color(Role::Primary))
                        .add_modifier(Modifier::BOLD),
                ),
            rect,
            &mut state,
        );
    }
    // 交互页模态(iter-35):居中覆视口,搜索框 + 过滤列表 + 提示。审批优先级更高,故在其前画。
    if let Some(panel) = &ui.panel {
        draw_panel(frame, area, panel);
    }
    if let Some(req) = approval {
        // 审批模态覆整个 Live 视口;↑↓ 滚动看长 diff;diff 行按 +/- 语义着色(iter-28)。
        frame.render_widget(Clear, area);
        let mut lines: Vec<Line> = vec![
            Line::from(Span::styled(
                format!("⚠ Allow {} ?", req.action),
                Style::default()
                    .fg(role_color(Role::Warn))
                    .add_modifier(Modifier::BOLD),
            )),
            Line::default(),
        ];
        for l in req.detail.lines() {
            let role = if l.starts_with('+') {
                Role::DiffAdd
            } else if l.starts_with('-') {
                Role::DiffDel
            } else {
                Role::Warn
            };
            lines.push(Line::from(Span::styled(
                l.to_owned(),
                Style::default().fg(role_color(role)),
            )));
        }
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "y/Enter: approve    n/Esc: reject    ↑↓/PgUp/PgDn: scroll details",
            Style::default().fg(role_color(Role::Muted)),
        )));
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Permission required ")
                        .border_style(Style::default().fg(role_color(Role::Warn))),
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
/// 事件行配色:经语义角色取色(iter-28 收口);终答用 White(具名 ANSI,非角色)。
fn event_color(m: &str) -> Color {
    if m.starts_with("verify: PASS") {
        role_color(Role::Success)
    } else if m.starts_with("verify: FAIL") {
        role_color(Role::Error)
    } else if m.starts_with("act:") {
        role_color(Role::Warn)
    } else if m.contains("(final)") {
        Color::White
    } else {
        role_color(Role::Info)
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
    fn ctx_percent_clamps_and_guards() {
        assert_eq!(ctx_percent(0, 200_000), 0);
        assert_eq!(ctx_percent(6_000, 200_000), 3);
        assert_eq!(ctx_percent(999_999, 100), 100); // 超窗封顶
        assert_eq!(ctx_percent(500, 0), 0); // 窗口未知:防除零
    }

    #[test]
    fn busy_bar_omits_todo_when_empty_and_shows_when_present() {
        let none = fmt_busy_bar("reasoning", &[], 12, 340, 28, 0);
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
        let with = fmt_busy_bar("acting", &todos, 3, 10, 3, 0);
        assert_eq!(with, "⚡ acting · ⏱ 3s · 10 tok · 3 tok/s · todo 1/2");
    }

    /// iter-33:忙碌粘条显待跑队列深度(纯函数)。
    #[test]
    fn busy_bar_shows_queue_depth() {
        assert_eq!(
            fmt_busy_bar("reasoning", &[], 5, 100, 20, 0),
            "⚡ reasoning · ⏱ 5s · 100 tok · 20 tok/s"
        );
        assert_eq!(
            fmt_busy_bar("reasoning", &[], 5, 100, 20, 2),
            "⚡ reasoning · ⏱ 5s · 100 tok · 20 tok/s · ⏳2"
        );
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

    /// 登录页(iter-38):列全部内置 preset,每行 key 是合法 preset id,kind 为 Login。
    #[test]
    fn login_panel_lists_all_presets() {
        let p = login_panel();
        assert_eq!(p.kind, PanelKind::Login);
        assert_eq!(p.rows.len(), PROVIDER_PRESETS.len());
        for r in &p.rows {
            assert!(
                preset_by_id(&r.key).is_some(),
                "登录页行 key 非 preset id: {}",
                r.key
            );
        }
    }

    #[test]
    fn models_panel_selects_current() {
        let list = [
            mi("a", Some(128_000)),
            mi("b", Some(200_000)),
            mi("c", None),
        ];
        let p = models_panel(&list, "b");
        assert_eq!(p.kind, PanelKind::Models);
        assert_eq!(p.selected().map(|r| r.key.as_str()), Some("b")); // 当前模型高亮
        assert_eq!(p.rows[0].ctx, Some(128_000)); // 携真实窗口供选中缓存
        assert!(p.rows[2].value.contains('?')); // 缺 ctx 显 ?
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
    fn panel_action_routes_keys() {
        let press = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert_eq!(panel_action(&press(KeyCode::Up)), PanelAction::Up);
        assert_eq!(panel_action(&press(KeyCode::Down)), PanelAction::Down);
        assert_eq!(panel_action(&press(KeyCode::Enter)), PanelAction::Enter);
        assert_eq!(panel_action(&press(KeyCode::Esc)), PanelAction::Esc);
        assert_eq!(
            panel_action(&press(KeyCode::Char('x'))),
            PanelAction::Char('x')
        );
        assert_eq!(
            panel_action(&press(KeyCode::Backspace)),
            PanelAction::Backspace
        );
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
        // busy 时 Enter 不再忽略 → 入队(iter-33)
        assert_eq!(
            input_action(&press(KeyCode::Enter), true, false),
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

    /// iter-30:wcwidth 显示宽度 —— CJK/emoji 占 2 格,光标显示列按实占累加,折行按实占计。
    #[test]
    fn wcwidth_display_columns() {
        // 单字符 / 字符串单元格宽。
        assert_eq!(char_cells('a'), 1);
        assert_eq!(char_cells('你'), 2);
        assert_eq!(str_cells("ab你好"), 6); // 1+1+2+2
                                            // 光标显示列:CJK 前缀按 2 格累加,不再偏左。
        let mut s = InputState::default();
        s.insert_str("你好a"); // cursor=3(字符序),显示列应 = 2+2+1 = 5
        assert_eq!(s.cursor, 3);
        assert_eq!(s.cursor_display_col(), (0, 5));
        s.left(); // 光标移到 'a' 前(字符序 2)→ 显示列 4
        assert_eq!(s.cursor_display_col(), (0, 4));
        // 多行:换行后显示列从 0 起。
        let mut m = InputState::default();
        m.insert_str("你\nb");
        assert_eq!(m.cursor_display_col(), (1, 1));
        // 折行:CJK 按实占,不再低估行数(3 个全角 = 6 格,宽 4 → 2 行,旧口径误判 1 行)。
        assert_eq!(wrapped_rows("你你你", 4), 2);
        assert_eq!(wrapped_rows("abcd", 4), 1);
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

    /// iter-28:色角色映射 —— ANSI 16 具名色,语义正确。
    #[test]
    fn role_colors_are_ansi16() {
        assert_eq!(role_color(Role::Success), Color::Green);
        assert_eq!(role_color(Role::Error), Color::Red);
        assert_eq!(role_color(Role::DiffAdd), Color::Green);
        assert_eq!(role_color(Role::DiffDel), Color::Red);
        assert_eq!(role_color(Role::Primary), Color::Cyan);
        assert_eq!(role_color(Role::Border), Color::DarkGray);
        assert_eq!(role_color(Role::Command), Color::LightGreen);
    }

    /// iter-28:md 轻渲染 —— 围栏切态、块内 Muted、标题粗、行内 code、未闭合按字面。
    #[test]
    fn md_line_rendering() {
        let (spans, state) = md_line_spans("```rust", false);
        assert!(state);
        assert_eq!(spans.len(), 1);
        let (_, state2) = md_line_spans("```", true);
        assert!(!state2);
        let (s, st) = md_line_spans("let x = 1;", true);
        assert!(st);
        assert_eq!(s[0].style.fg, Some(role_color(Role::Muted)));
        let (h, _) = md_line_spans("# Title", false);
        assert!(h[0].style.add_modifier.contains(Modifier::BOLD));
        let (i, _) = md_line_spans("use `foo` now", false);
        assert_eq!(i[1].content.as_ref(), "foo");
        assert_eq!(i[1].style.fg, Some(role_color(Role::Warn)));
        let (b, _) = md_line_spans("a **big** b", false);
        assert!(b.iter().any(
            |sp| sp.content.as_ref() == "big" && sp.style.add_modifier.contains(Modifier::BOLD)
        ));
        // 未闭合记号按字面,内容零丢失
        let (u, _) = md_line_spans("lone `tick", false);
        assert_eq!(
            u.iter().map(|sp| sp.content.as_ref()).collect::<String>(),
            "lone `tick"
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
            models_panel(&[], "").title,
            agent_panel(&[]).title,
        ];
        for t in &titles {
            assert!(!has_cjk(t), "panel 标题应为英文: {t}");
        }
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
}
