use std::sync::mpsc;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use super::sanitize_paste;

pub(crate) struct ApprovalRequest {
    pub(crate) action: String,
    pub(crate) detail: String,
    pub(crate) reply: mpsc::SyncSender<bool>,
}

/// 审批挂起时对一次按键的**纯决策**。修「滚动即拒绝」根因 —— 此前审批态下除 `y`/`Enter`
/// 外一切键(含滚动键)都落 `_ => 拒绝`,用户想滚动看 diff 反而误拒。滚动/忽略**不消**审批请求。
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum ApprovalAction {
    Approve,
    Reject,
    Scroll(i16),
    Ignore,
}

pub(crate) fn approval_action(key: KeyCode) -> ApprovalAction {
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
pub(crate) fn apply_scroll(scroll: u16, delta: i16) -> u16 {
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
pub(crate) enum InputAction {
    Insert(char),
    Backspace,
    Left,
    Right,
    Home,
    End,
    Delete,
    NewLine,
    Submit,
    /// busy 时提交 → 入队(iter-33),当前任务毕自动接跑。
    Queue,
    /// busy 时把输入插到队首，当前任务完成后立即推进。
    PushNow,
    /// Send guidance to the active agent without interrupting its current turn.
    Steer,
    Interrupt,
    ToggleDetails,
    ToggleReasoning,
    ToggleAnswer,
    ToggleActivity,
    /// Open the non-blocking live audit/search surface without mutating input.
    OpenLiveSearch,
    OpenInputEditor,
    PasteClipboard,
    CursorUpOrHistory,
    CursorDownOrHistory,
    PopupOpen,
    PopupNext,
    PopupPrev,
    /// Tab 接受当前补全,仅写回输入框,不提交。
    PopupAccept,
    /// Enter 接受当前补全并直接提交整条输入。
    PopupSubmit,
    PopupClose,
    Ignore,
}

/// 一次原始按键事件的**去重 + 归一决策**(纯函数,可测)。跨平台键事件不一致:Windows 每键发
/// Press+Release,Unix(Kitty 未开 REPORT_EVENT_TYPES)只发 Press;而某些输入法把空格键作为
/// `Char('\u{a0}')`(no-break space)且**只发 Release**注入 —— 旧「只收 Press」逻辑会把它整个丢弃。
///
/// 规则:
/// - Press / Repeat → 收下(并记入 `pressed`);
/// - Release 若配得上先前 Press(正常松键)→ 丢弃(免 Windows 双触发),并从 `pressed` 移除;
/// - **悬空** Release(配不上任何 Press)→ 收下字符键(= 输入法注入)、旧控制字节、以及
///   **Enter**(Windows ConPTY / 部分宿主只投递 Enter 的 key-up `INPUT_RECORD`;空缓冲
///   Submit 本就是 no-op,启动残留松键不会误发任务)。其余功能键仍忽略。
///
/// 普通输入/面板下游一律以 **Press** 呈现(下游 `input_action`/`panel_action` 内部只认 Press),
/// 仅 Ctrl+Space 的 Release 留给主循环拦截即时审计；并把 no-break(U+00A0)/全角(U+3000)空格
/// **归一为普通空格**(否则显示像空格但按 `' '` 分词的命令会失败)。返回 `Some(归一后的事件)` = 处理;
/// `None` = 忽略。
fn canonical_key_code(key: &KeyEvent) -> KeyCode {
    match key.code {
        // ConPTY/legacy terminals may surface Enter as CR or LF instead of
        // KeyCode::Enter.  Normalize at the boundary so submit, queue and
        // Ctrl+Enter front-queue share one routing path.
        KeyCode::Char('\r' | '\n') => KeyCode::Enter,
        // Ctrl-M is the byte-level CR spelling used by a few terminal/input
        // stacks for Enter.  Keep Ctrl-J as the explicit multiline shortcut.
        KeyCode::Char('m' | 'M') if key.modifiers.contains(KeyModifiers::CONTROL) => KeyCode::Enter,
        // ConPTY, legacy Win32 input and a few PTYs expose Backspace as the
        // raw BS/DEL bytes instead of KeyCode::Backspace.
        KeyCode::Char('\x08' | '\x7f') => KeyCode::Backspace,
        other => other,
    }
}

pub(crate) fn normalize_key_event(ev: &KeyEvent) -> KeyEvent {
    KeyEvent::new_with_kind(canonical_key_code(ev), ev.modifiers, ev.kind)
}

/// Ctrl+Space has a useful press/release pair on Windows and on terminals
/// that enable Kitty `REPORT_EVENT_TYPES`. Keep that one release event for
/// momentary live-audit handling; every other release remains deduplicated.
fn is_legacy_function_release(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Enter
            | KeyCode::Esc
            | KeyCode::Tab
            | KeyCode::Backspace
            | KeyCode::Delete
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::PageUp
            | KeyCode::PageDown
    )
}

pub(crate) fn is_momentary_hold_key(key: &KeyEvent) -> bool {
    let code = canonical_key_code(key);
    (key.modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char(' '))
        || (key
            .modifiers
            .contains(KeyModifiers::CONTROL | KeyModifiers::SHIFT)
            && matches!(code, KeyCode::Char('2' | '@')))
}

pub(crate) fn decide_key(
    pressed: &mut std::collections::HashSet<KeyCode>,
    ev: &KeyEvent,
) -> Option<KeyEvent> {
    let legacy_control_release = ev.kind == KeyEventKind::Release
        && matches!(ev.code, KeyCode::Char('\x08' | '\x7f' | '\r' | '\n'));
    let ev = normalize_key_event(ev);
    let process = match ev.kind {
        KeyEventKind::Press | KeyEventKind::Repeat => {
            pressed.insert(ev.code);
            true
        }
        KeyEventKind::Release => {
            if is_momentary_hold_key(&ev) {
                pressed.remove(&ev.code)
            } else if pressed.remove(&ev.code) {
                false // 正常松键:对应的 Press 已处理过
            } else {
                matches!(ev.code, KeyCode::Char(_))
                    || legacy_control_release
                    || is_legacy_function_release(ev.code)
                // 悬空 Release:字符/旧控制字节,以及 Windows ConPTY 常只给
                // key-up 的 Enter/Esc/方向键等功能键。
            }
        }
    };
    if !process {
        return None;
    }
    let code = match ev.code {
        KeyCode::Char('\u{a0}') | KeyCode::Char('\u{3000}') => KeyCode::Char(' '),
        other => other,
    };
    let kind = if ev.kind == KeyEventKind::Release && is_momentary_hold_key(&ev) {
        KeyEventKind::Release
    } else {
        KeyEventKind::Press
    };
    Some(KeyEvent::new_with_kind(code, ev.modifiers, kind))
}

#[derive(Debug)]
pub(crate) enum TerminalEventAction {
    Key(KeyEvent),
    Paste(String),
    Mouse(MouseEvent),
    Redraw,
}

/// Classify terminal events before the main loop applies UI state changes.
/// Resize/focus events redraw; mouse events stay typed so the main loop can
/// scroll/select instead of silently discarding them.
pub(crate) fn terminal_event_action(event: Event) -> TerminalEventAction {
    match event {
        Event::Key(key) => TerminalEventAction::Key(key),
        Event::Paste(text) => TerminalEventAction::Paste(sanitize_paste(&text)),
        Event::Mouse(mouse) => TerminalEventAction::Mouse(mouse),
        _ => TerminalEventAction::Redraw,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MouseAction {
    Scroll(i8),
    Select { column: u16, row: u16 },
    Close,
    Ignore,
}

pub(crate) fn mouse_action(event: &MouseEvent) -> MouseAction {
    match event.kind {
        MouseEventKind::ScrollUp => MouseAction::Scroll(1),
        MouseEventKind::ScrollDown => MouseAction::Scroll(-1),
        MouseEventKind::Down(MouseButton::Left) => MouseAction::Select {
            column: event.column,
            row: event.row,
        },
        MouseEventKind::Down(MouseButton::Right) => MouseAction::Close,
        _ => MouseAction::Ignore,
    }
}

pub(crate) fn input_action(key: &KeyEvent, busy: bool, popup_open: bool) -> InputAction {
    if key.kind != KeyEventKind::Press {
        return InputAction::Ignore;
    }
    let code = canonical_key_code(key);
    // Escape is a takeover signal only for an un-covered busy surface. Popup,
    // approval, and panel precedence is resolved by the main loop before this
    // classifier is called, so those surfaces keep their close/reject meaning.
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c' | 'C'))
        || busy && key.modifiers.is_empty() && code == KeyCode::Esc
    {
        return InputAction::Interrupt;
    }
    if popup_open {
        return popup_action(key, code);
    }
    if let Some(action) = global_attention_action(key) {
        return action;
    }
    if busy && key.modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Enter {
        if key.modifiers.contains(KeyModifiers::SHIFT) {
            return InputAction::Steer;
        }
        return InputAction::PushNow;
    }
    normal_input_action(key, code, busy)
}

fn popup_action(key: &KeyEvent, code: KeyCode) -> InputAction {
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('o' | 'O' | 'a' | 'A'))
    {
        return InputAction::Ignore;
    }
    // 浮窗态:↑↓选、Tab 接受但不提交、Enter 接受并提交、Esc 关;
    // 字符/退格穿透继续编辑(主环先关浮窗)。
    match code {
        KeyCode::Tab => InputAction::PopupAccept,
        KeyCode::Down => InputAction::PopupNext,
        KeyCode::Up => InputAction::PopupPrev,
        KeyCode::Enter => InputAction::PopupSubmit,
        KeyCode::Char(c) => InputAction::Insert(c),
        KeyCode::Backspace => InputAction::Backspace,
        _ => InputAction::PopupClose,
    }
}

fn global_attention_action(key: &KeyEvent) -> Option<InputAction> {
    if !key.modifiers.contains(KeyModifiers::CONTROL) {
        return None;
    }
    match key.code {
        KeyCode::Char('o' | 'O') => Some(InputAction::ToggleDetails),
        KeyCode::Char('r' | 'R') => Some(InputAction::ToggleReasoning),
        KeyCode::Char('a' | 'A') => Some(InputAction::ToggleAnswer),
        KeyCode::Char('t' | 'T') => Some(InputAction::ToggleActivity),
        KeyCode::Char('f' | 'F') => Some(InputAction::OpenLiveSearch),
        _ => None,
    }
}

fn normal_input_action(key: &KeyEvent, code: KeyCode, busy: bool) -> InputAction {
    match code {
        KeyCode::Enter
            if key.modifiers.contains(KeyModifiers::SHIFT)
                || key.modifiers.contains(KeyModifiers::ALT) =>
        {
            InputAction::NewLine
        }
        KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => InputAction::NewLine,
        KeyCode::Char('e' | 'E') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            InputAction::OpenInputEditor
        }
        KeyCode::Char('v' | 'V') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            InputAction::PasteClipboard
        }
        // busy 时 Enter → 入队(iter-33),空闲 → 立即提交。
        KeyCode::Enter if busy => InputAction::Queue,
        KeyCode::Enter => InputAction::Submit,
        KeyCode::Tab => InputAction::PopupOpen,
        KeyCode::Char(c) => InputAction::Insert(c),
        KeyCode::Backspace => InputAction::Backspace,
        KeyCode::Delete => InputAction::Delete,
        KeyCode::Left => InputAction::Left,
        KeyCode::Right => InputAction::Right,
        KeyCode::Home => InputAction::Home,
        KeyCode::End => InputAction::End,
        KeyCode::Up => InputAction::CursorUpOrHistory,
        KeyCode::Down => InputAction::CursorDownOrHistory,
        _ => InputAction::Ignore,
    }
}

/// Keep audit attention shortcuts global while a panel is being browsed.
///
/// Panel text/search editing owns ordinary characters, but Ctrl+R/Ctrl+A/Ctrl+O/Ctrl+T
/// are semantic attention changes, not query input.  The caller disables this
/// bridge for editor fields and completion popups so those modal states retain
/// their existing precedence.
pub(crate) fn panel_attention_action(
    key: &KeyEvent,
    browsing_panel: bool,
    popup_open: bool,
) -> Option<InputAction> {
    if !browsing_panel || popup_open {
        return None;
    }
    match input_action(key, false, false) {
        action @ (InputAction::ToggleDetails
        | InputAction::ToggleReasoning
        | InputAction::ToggleAnswer
        | InputAction::ToggleActivity) => Some(action),
        _ => None,
    }
}

/// Queue inspection is a global, non-destructive intervention shortcut.
pub(crate) fn queue_panel_toggle_action(key: &KeyEvent) -> bool {
    key.kind == KeyEventKind::Press
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('q' | 'Q'))
}

/// Open/close the current mixed-stream inspector without cancelling the task.
/// Ctrl+I is deliberately separate from Tab, which remains completion select;
/// Alt+I is the byte-safe fallback for terminals that encode Ctrl+I as Tab.
pub(crate) fn live_history_toggle_action(
    key: &KeyEvent,
    popup_open: bool,
    has_history: bool,
) -> bool {
    key.kind == KeyEventKind::Press
        && !popup_open
        && has_history
        && (key.modifiers.contains(KeyModifiers::CONTROL)
            || key.modifiers.contains(KeyModifiers::ALT))
        && matches!(key.code, KeyCode::Char('i' | 'I'))
}

/// Live 工具焦点快捷键:仅在无浮窗且确有工具块时拦截 Alt+↑/↓,避免破坏输入编辑回退。
/// Ctrl-C requires a second press within the window to terminate the TUI.
pub(crate) fn is_second_ctrl_c(
    previous: Option<std::time::Instant>,
    now: std::time::Instant,
) -> bool {
    previous
        .and_then(|at| now.checked_duration_since(at))
        .is_some_and(|elapsed| elapsed <= std::time::Duration::from_secs(2))
}

pub(crate) fn tool_focus_action(key: &KeyEvent, popup_open: bool, has_tools: bool) -> Option<i8> {
    if key.kind != KeyEventKind::Press
        || popup_open
        || !has_tools
        || !key.modifiers.contains(KeyModifiers::ALT)
    {
        return None;
    }
    match key.code {
        KeyCode::Up => Some(-1),
        KeyCode::Down => Some(1),
        _ => None,
    }
}

/// HOLD 下跨 Answer/Reasoning/Tool 语义块移动焦点；Tab 仍专属补全。
pub(crate) fn semantic_focus_action(
    key: &KeyEvent,
    popup_open: bool,
    inspecting: bool,
    has_blocks: bool,
) -> Option<i8> {
    if key.kind != KeyEventKind::Press
        || popup_open
        || !inspecting
        || !has_blocks
        || !key.modifiers.contains(KeyModifiers::ALT)
    {
        return None;
    }
    match key.code {
        KeyCode::Left => Some(-1),
        KeyCode::Right => Some(1),
        _ => None,
    }
}

/// 展开工具详情的局部滚动:Alt+PageUp/Alt+PageDown,仅单焦点详情可滚时拦截。
pub(crate) fn tool_detail_scroll_action(
    key: &KeyEvent,
    popup_open: bool,
    has_scrollable_details: bool,
) -> Option<i8> {
    if key.kind != KeyEventKind::Press
        || popup_open
        || !has_scrollable_details
        || !key.modifiers.contains(KeyModifiers::ALT)
    {
        return None;
    }
    match key.code {
        KeyCode::PageUp => Some(1),
        KeyCode::PageDown => Some(-1),
        _ => None,
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum LiveScrollAction {
    Older,
    Newer,
    OlderPage,
    NewerPage,
    Follow,
}

/// Toggle a non-destructive hold/follow mode for the live viewport.
pub(crate) fn live_hold_toggle_action(key: &KeyEvent, popup_open: bool, has_output: bool) -> bool {
    if key.kind != KeyEventKind::Press || popup_open || !has_output {
        return false;
    }
    is_momentary_hold_key(key)
}

/// Release half of the optional momentary live-audit gesture. Terminals that
/// do not report releases simply keep the existing press-to-toggle behavior.
pub(crate) fn live_hold_release_action(key: &KeyEvent, popup_open: bool) -> bool {
    key.kind == KeyEventKind::Release && !popup_open && is_momentary_hold_key(key)
}

/// In the held Inspector, plain Space activates the focused semantic block.
/// HOLD gates this shortcut so ordinary prompt typing keeps its usual meaning.
pub(crate) fn live_semantic_toggle_action(
    key: &KeyEvent,
    popup_open: bool,
    inspecting: bool,
    has_semantic_block: bool,
) -> bool {
    key.kind == KeyEventKind::Press
        && !popup_open
        && inspecting
        && has_semantic_block
        && key.modifiers.is_empty()
        && matches!(canonical_key_code(key), KeyCode::Char(' '))
}

/// Live Answer/Reasoning inspection: plain PageUp/PageDown page the live viewport;
/// Alt+PageUp/PageDown keep the smaller detail-scroll step. Modal input keeps priority.
pub(crate) fn live_scroll_action(
    key: &KeyEvent,
    popup_open: bool,
    tool_details_scrollable: bool,
    has_output: bool,
) -> Option<LiveScrollAction> {
    if key.kind != KeyEventKind::Press || popup_open || !has_output {
        return None;
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        if tool_details_scrollable {
            return None;
        }
        return match key.code {
            KeyCode::PageUp => Some(LiveScrollAction::Older),
            KeyCode::PageDown => Some(LiveScrollAction::Newer),
            KeyCode::End => Some(LiveScrollAction::Follow),
            _ => None,
        };
    }
    if key.modifiers == KeyModifiers::NONE {
        return match key.code {
            KeyCode::PageUp => Some(LiveScrollAction::OlderPage),
            KeyCode::PageDown => Some(LiveScrollAction::NewerPage),
            _ => None,
        };
    }
    None
}

/// 默认输入/状态 chrome 占五行；窄终端仍至少保留一行 Live 输出。
pub(crate) fn live_page_rows(terminal_height: u16) -> usize {
    terminal_height.saturating_sub(5).max(1) as usize
}

/// 首逻辑行内 Up 的回退决策(iter-48 G5,修「光标卡首行」):`move_up` 失败(已在首逻辑行)时,
/// 若首行折成**多视觉行**且光标不在行首 → 先跳行首(true),免历史召回突变替换长草稿;
/// 行首 / 单视觉行 → 照常召回(false)。纯函数。
pub(crate) fn up_fallback_is_home(buffer: &str, cursor: usize, width: u16) -> bool {
    let first = buffer.split('\n').next().unwrap_or("");
    cursor > 0 && super::render::line_visual_rows(first, width.max(1) as usize) > 1
}

// ───────────────────────── 输入状态机(iter-27)─────────────────────────

/// 多行输入编辑器:单 String 缓冲 + 字符光标 + 会话内历史召回。全纯方法、离线可测。
/// 光标按**逻辑行**('\n')计,折行内微移不做(ponytail:要所见即所得再算折行几何)。
#[derive(Default)]
pub(crate) struct InputState {
    pub(crate) buffer: String,
    /// 光标 = 字符偏移(非字节;`byte_at` 换算)。
    pub(crate) cursor: usize,
    pub(crate) history: Vec<String>,
    pub(crate) hist_idx: Option<usize>,
    /// false = global pre-session history; true = current conversation history.
    pub(crate) session_mode: bool,
    /// 召回历史前暂存的未提交草稿(Down 到底还原)。
    pub(crate) draft: String,
}

impl InputState {
    pub(crate) fn set_history(&mut self, history: Vec<String>, session_mode: bool) {
        self.history = history;
        self.hist_idx = None;
        self.draft.clear();
        self.session_mode = session_mode;
    }

    pub(crate) fn begin_session(&mut self) {
        self.history.clear();
        self.hist_idx = None;
        self.draft.clear();
        self.session_mode = true;
    }

    pub(crate) fn drop_last_history_if(&mut self, value: &str) {
        if self.history.last().is_some_and(|last| last == value) {
            self.history.pop();
        }
    }

    pub(crate) fn push_history(&mut self, value: &str) {
        if value.trim().is_empty() || self.history.last().is_some_and(|last| last == value) {
            return;
        }
        self.history.push(value.to_string());
    }

    pub(crate) fn byte_at(&self, char_idx: usize) -> usize {
        self.buffer
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(self.buffer.len())
    }
    pub(crate) fn insert(&mut self, c: char) {
        let b = self.byte_at(self.cursor);
        self.buffer.insert(b, c);
        self.cursor += 1;
    }
    pub(crate) fn insert_str(&mut self, s: &str) {
        let b = self.byte_at(self.cursor);
        self.buffer.insert_str(b, s);
        self.cursor += s.chars().count();
    }
    pub(crate) fn backspace(&mut self) {
        if self.cursor > 0 {
            let b = self.byte_at(self.cursor - 1);
            self.buffer.remove(b);
            self.cursor -= 1;
        }
    }
    pub(crate) fn delete(&mut self) {
        if self.cursor < self.buffer.chars().count() {
            let b = self.byte_at(self.cursor);
            self.buffer.remove(b);
        }
    }
    pub(crate) fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }
    pub(crate) fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.buffer.chars().count());
    }
    pub(crate) fn home(&mut self) {
        let (_, col) = self.row_col();
        self.cursor -= col;
    }
    pub(crate) fn end(&mut self) {
        // End is a hot interactive path; scan from the logical cursor without
        // materializing the whole UTF-8 buffer as `Vec<char>`.
        let mut cursor = self.cursor;
        for ch in self.buffer.chars().skip(self.cursor) {
            if ch == '\n' {
                break;
            }
            cursor += 1;
        }
        self.cursor = cursor;
    }
    /// 光标所在 (逻辑行, 字符列)。
    pub(crate) fn row_col(&self) -> (usize, usize) {
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
    pub(crate) fn rows(&self) -> usize {
        self.buffer.chars().filter(|c| *c == '\n').count() + 1
    }
    pub(crate) fn is_long(&self) -> bool {
        self.rows() > 4 || self.buffer.chars().count() > 240
    }
    pub(crate) fn line_len(&self, row: usize) -> usize {
        self.buffer
            .split('\n')
            .nth(row)
            .map(|l| l.chars().count())
            .unwrap_or(0)
    }
    pub(crate) fn cursor_to(&mut self, row: usize, col: usize) {
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
    pub(crate) fn move_up(&mut self) -> bool {
        let (row, col) = self.row_col();
        if row == 0 {
            return false;
        }
        self.cursor_to(row - 1, col);
        true
    }
    /// 下移一逻辑行;已在末行 → false(调用方转历史前进/还原草稿)。
    pub(crate) fn move_down(&mut self) -> bool {
        let (row, col) = self.row_col();
        if row + 1 >= self.rows() {
            return false;
        }
        self.cursor_to(row + 1, col);
        true
    }
    pub(crate) fn recall_prev(&mut self) {
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
    pub(crate) fn recall_next(&mut self) {
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
    pub(crate) fn take(&mut self) -> String {
        let s = std::mem::take(&mut self.buffer);
        self.cursor = 0;
        self.hist_idx = None;
        self.draft.clear();
        self.push_history(s.trim());
        s
    }
}

// ───────────────────────── 补全浮窗(iter-27)─────────────────────────

/// 斜杠命令静态表(补全数据源,与 `run_command` 分支对齐;有序稳态)。
pub(crate) const SLASH_COMMANDS: &[&str] = &[
    "/agent",
    "/activity",
    "/answer",
    "/answers",
    "/commands",
    "/compact",
    "/config",
    "/cost",
    "/exit",
    "/effort",
    "/find",
    "/goal",
    "/help",
    "/history",
    "/inspect",
    "/jailbreak",
    "/login",
    "/mcp",
    "/model",
    "/provider",
    "/quit",
    "/queue",
    "/reasoning",
    "/reset",
    "/skills",
    "/steer",
    "/tools",
];

/// 动态斜杠命令名(iter-39,含前导 `/`):启动从命令表填一次,供补全浮窗与静态表并列。
/// 进程全局 set-once(与 jailbreak AtomicBool 先例一致);未设(如单测)→ 空,补全只用静态表。
pub(crate) static DYNAMIC_COMMANDS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
pub(crate) fn set_dynamic_commands(cmds: &[agent::SlashCommand]) {
    let _ = DYNAMIC_COMMANDS.set(cmds.iter().map(|c| format!("/{}", c.name)).collect());
}
pub(crate) fn dynamic_commands() -> &'static [String] {
    DYNAMIC_COMMANDS.get().map(|v| v.as_slice()).unwrap_or(&[])
}

pub(crate) struct Popup {
    pub(crate) items: Vec<String>,
    pub(crate) selected: usize,
    /// 被补全词的起始**字符**偏移(应用时替换 [anchor, cursor))。
    pub(crate) anchor: usize,
}

/// 光标前当前词(空白定界):(起始字符偏移, 词)。
pub(crate) fn current_word(buffer: &str, cursor: usize) -> (usize, String) {
    let chars: Vec<char> = buffer.chars().collect();
    let end = cursor.min(chars.len());
    let mut start = end;
    while start > 0 && !chars[start - 1].is_whitespace() {
        start -= 1;
    }
    (start, chars[start..end].iter().collect())
}

/// 前缀过滤 + 排序(有序稳态)。
pub(crate) fn filter_prefix<'a>(
    cands: impl IntoIterator<Item = &'a str>,
    prefix: &str,
) -> Vec<String> {
    let mut v: Vec<String> = cands
        .into_iter()
        .filter(|c| c.starts_with(prefix))
        .map(str::to_owned)
        .collect();
    v.sort();
    v
}

/// `@` 路径候选:词的目录部分单层 `read_dir`(不递归,防 IO 卡 UI),前缀过滤,目录带 `/`。
pub(crate) fn path_candidates(part: &str) -> Vec<String> {
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

/// Tab/输入触发:行首 `/` 词补命令;词内 `@` 补路径(候选带回词前缀,应用时整词替换)。
pub(crate) fn build_popup(input: &InputState) -> Option<Popup> {
    let (anchor, word) = current_word(&input.buffer, input.cursor);
    if word.starts_with('/') && anchor == 0 {
        let items = filter_prefix(
            SLASH_COMMANDS
                .iter()
                .copied()
                .chain(dynamic_commands().iter().map(String::as_str)),
            &word,
        );
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
pub(crate) fn apply_completion(input: &mut InputState, popup: &Popup) {
    let sel = popup.items[popup.selected].clone();
    let start_b = input.byte_at(popup.anchor);
    let end_b = input.byte_at(input.cursor);
    input.buffer.replace_range(start_b..end_b, &sel);
    input.cursor = popup.anchor + sel.chars().count();
}

// ───────────────────────── 交互页 Panel(iter-35)─────────────────────────
