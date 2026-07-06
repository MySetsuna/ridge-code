//! 多行文本输入组件，用于交互式界面的任务输入。

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

/// 多行文本输入框。
pub struct TextArea {
    /// 输入的文本内容（按行存储）。
    lines: Vec<String>,
    /// 光标行号。
    cursor_row: usize,
    /// 光标列号。
    cursor_col: usize,
    /// 输入历史记录。
    history: Vec<String>,
    /// 历史记录索引（None 表示不在历史浏览模式）。
    history_index: Option<usize>,
    /// 临时保存当前输入（浏览历史时）。
    saved_input: String,
    /// 是否已提交。
    submitted: bool,
    /// 提交的内容。
    submitted_content: Option<String>,
}

impl TextArea {
    /// 创建新的空文本输入框。
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_row: 0,
            cursor_col: 0,
            history: Vec::new(),
            history_index: None,
            saved_input: String::new(),
            submitted: false,
            submitted_content: None,
        }
    }

    /// 从历史记录创建，预填充内容。
    pub fn with_history(history: Vec<String>) -> Self {
        Self {
            lines: vec![String::new()],
            cursor_row: 0,
            cursor_col: 0,
            history,
            history_index: None,
            saved_input: String::new(),
            submitted: false,
            submitted_content: None,
        }
    }

    /// 处理键盘事件。
    /// 返回 true 表示有状态变化需要重绘。
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        // 如果已提交，忽略后续输入
        if self.submitted {
            return false;
        }

        match key.code {
            // Enter 提交（非 Shift+Enter）
            KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.submit();
                true
            }
            // Shift+Enter 换行
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.insert_newline();
                true
            }
            // 普通字符输入
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert_char(c);
                true
            }
            // Ctrl+C 中断
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.clear();
                true
            }
            // Ctrl+A 跳到行首
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor_col = 0;
                true
            }
            // Ctrl+E 跳到行尾
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor_col = self.lines[self.cursor_row].chars().count();
                true
            }
            // Ctrl+U 清空当前行
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.lines[self.cursor_row].clear();
                self.cursor_col = 0;
                true
            }
            // Ctrl+K 删除到行尾
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let byte_idx = Self::char_to_byte_idx(&self.lines[self.cursor_row], self.cursor_col);
                self.lines[self.cursor_row].truncate(byte_idx);
                true
            }
            // Ctrl+L 清屏（清空输入）
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.clear();
                true
            }
            // Ctrl+R 搜索历史
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.history_prev();
                true
            }
            // Backspace 删除前一个字符
            KeyCode::Backspace => {
                self.delete_backward();
                true
            }
            // Delete 删除后一个字符
            KeyCode::Delete => {
                self.delete_forward();
                true
            }
            // 左箭头
            KeyCode::Left => {
                self.move_cursor_left();
                true
            }
            // 右箭头
            KeyCode::Right => {
                self.move_cursor_right();
                true
            }
            // 上箭头 - 历史上一条 或 上移光标
            KeyCode::Up => {
                if self.lines.len() == 1 {
                    // 单行时浏览历史
                    self.history_prev();
                } else if self.cursor_row > 0 {
                    // 多行时上移光标
                    self.cursor_row -= 1;
                    let max_col = self.lines[self.cursor_row].chars().count();
                    self.cursor_col = self.cursor_col.min(max_col);
                }
                true
            }
            // 下箭头 - 历史下一条 或 下移光标
            KeyCode::Down => {
                if self.lines.len() == 1 {
                    self.history_next();
                } else if self.cursor_row < self.lines.len() - 1 {
                    self.cursor_row += 1;
                    let max_col = self.lines[self.cursor_row].chars().count();
                    self.cursor_col = self.cursor_col.min(max_col);
                }
                true
            }
            // Home 跳到行首
            KeyCode::Home => {
                self.cursor_col = 0;
                true
            }
            // End 跳到行尾
            KeyCode::End => {
                self.cursor_col = self.lines[self.cursor_row].chars().count();
                true
            }
            _ => false,
        }
    }

    /// 插入字符。
    fn insert_char(&mut self, c: char) {
        let line = &mut self.lines[self.cursor_row];
        let byte_idx = Self::char_to_byte_idx(line, self.cursor_col);
        line.insert(byte_idx, c);
        self.cursor_col += 1;
    }

    /// 插入换行。
    fn insert_newline(&mut self) {
        let line = self.lines[self.cursor_row].clone();
        let byte_idx = Self::char_to_byte_idx(&line, self.cursor_col);
        let before = line[..byte_idx].to_string();
        let after = line[byte_idx..].to_string();
        self.lines[self.cursor_row] = before;
        self.lines.insert(self.cursor_row + 1, after);
        self.cursor_row += 1;
        self.cursor_col = 0;
    }

    /// 将字符索引转换为字节索引。
    fn char_to_byte_idx(line: &str, char_idx: usize) -> usize {
        line.char_indices()
            .nth(char_idx)
            .map(|(i, _)| i)
            .unwrap_or(line.len())
    }

    /// 将字节索引转换为字符索引。
    fn byte_to_char_idx(line: &str, byte_idx: usize) -> usize {
        line[..byte_idx].chars().count()
    }

    /// 向前删除字符。
    fn delete_backward(&mut self) {
        if self.cursor_col > 0 {
            let line = &mut self.lines[self.cursor_row];
            // 找到前一个字符的字节位置
            let byte_idx = Self::char_to_byte_idx(line, self.cursor_col - 1);
            let ch = line[byte_idx..].chars().next().unwrap();
            line.drain(byte_idx..byte_idx + ch.len_utf8());
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            // 合并到上一行
            let current_line = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].chars().count();
            self.lines[self.cursor_row].push_str(&current_line);
        }
    }

    /// 向后删除字符。
    fn delete_forward(&mut self) {
        let char_len = self.lines[self.cursor_row].chars().count();
        if self.cursor_col < char_len {
            let line = &mut self.lines[self.cursor_row];
            let byte_idx = Self::char_to_byte_idx(line, self.cursor_col);
            let ch = line[byte_idx..].chars().next().unwrap();
            line.drain(byte_idx..byte_idx + ch.len_utf8());
        } else if self.cursor_row < self.lines.len() - 1 {
            // 合并下一行
            let next_line = self.lines.remove(self.cursor_row + 1);
            self.lines[self.cursor_row].push_str(&next_line);
        }
    }

    /// 光标左移。
    fn move_cursor_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].chars().count();
        }
    }

    /// 光标右移。
    fn move_cursor_right(&mut self) {
        let char_len = self.lines[self.cursor_row].chars().count();
        if self.cursor_col < char_len {
            self.cursor_col += 1;
        } else if self.cursor_row < self.lines.len() - 1 {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
    }

    /// 浏览历史记录（上一条）。
    fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        if self.history_index.is_none() {
            self.saved_input = self.get_text();
            self.history_index = Some(self.history.len());
        }
        if let Some(idx) = self.history_index.as_mut() {
            if *idx > 0 {
                *idx -= 1;
                let content = self.history[*idx].clone();
                self.set_text(content);
            }
        }
    }

    /// 浏览历史记录（下一条）。
    fn history_next(&mut self) {
        if self.history_index.is_none() {
            return;
        }
        if let Some(idx) = self.history_index.as_mut() {
            *idx += 1;
            if *idx >= self.history.len() {
                // 恢复之前保存的输入
                let content = self.saved_input.clone();
                self.set_text(content);
                self.history_index = None;
            } else {
                let content = self.history[*idx].clone();
                self.set_text(content);
            }
        }
    }

    /// 提交输入。
    fn submit(&mut self) {
        let text = self.get_text().trim().to_string();
        if !text.is_empty() {
            self.history.push(text.clone());
        }
        self.submitted = true;
        self.submitted_content = Some(text);
    }

    /// 清空输入。
    fn clear(&mut self) {
        self.lines = vec![String::new()];
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.history_index = None;
    }

    /// 获取输入的文本。
    pub fn get_text(&self) -> String {
        self.lines.join("\n")
    }

    /// 设置输入的文本。
    pub fn set_text(&mut self, text: String) {
        self.lines = text.split('\n').map(String::from).collect();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_row = self.lines.len() - 1;
        self.cursor_col = self.lines[self.cursor_row].len();
    }

    /// 检查是否已提交。
    pub fn is_submitted(&self) -> bool {
        self.submitted
    }

    /// 获取提交的内容（消费提交状态）。
    pub fn take_submitted(&mut self) -> Option<String> {
        if self.submitted {
            self.submitted = false;
            self.submitted_content.take()
        } else {
            None
        }
    }

    /// 重置为未提交状态。
    pub fn reset(&mut self) {
        self.lines = vec![String::new()];
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.submitted = false;
        self.submitted_content = None;
        self.history_index = None;
    }

    /// 渲染文本输入框。
    pub fn render(&self, f: &mut Frame, area: Rect, title: &str) {
        let input_style = Style::default().fg(Color::White);
        let cursor_style = Style::default()
            .fg(Color::Black)
            .bg(Color::White)
            .add_modifier(Modifier::BOLD);

        // 构建带光标的文本
        let mut lines: Vec<Line> = Vec::new();
        for (row_idx, line) in self.lines.iter().enumerate() {
            if row_idx == self.cursor_row {
                // 将字符索引转换为字节索引
                let byte_idx = Self::char_to_byte_idx(line, self.cursor_col);
                let char_len = line.chars().count();

                let before: String = line[..byte_idx].to_string();
                let at_cursor: String = line[byte_idx..]
                    .chars()
                    .next()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| " ".to_string());
                let after: String = if self.cursor_col + 1 < char_len {
                    let next_byte = Self::char_to_byte_idx(line, self.cursor_col + 1);
                    line[next_byte..].to_string()
                } else {
                    String::new()
                };

                lines.push(Line::from(vec![
                    Span::styled(before, input_style),
                    Span::styled(at_cursor, cursor_style),
                    Span::styled(after, input_style),
                ]));
            } else {
                lines.push(Line::from(Span::styled(line.clone(), input_style)));
            }
        }

        let paragraph = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .wrap(Wrap { trim: false });

        f.render_widget(paragraph, area);
    }
}

impl Default for TextArea {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn make_key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: ratatui::crossterm::event::KeyEventKind::Press,
            state: ratatui::crossterm::event::KeyEventState::NONE,
        }
    }

    #[test]
    fn test_basic_input() {
        let mut ta = TextArea::new();
        ta.handle_key(make_key(KeyCode::Char('h'), KeyModifiers::NONE));
        ta.handle_key(make_key(KeyCode::Char('i'), KeyModifiers::NONE));
        assert_eq!(ta.get_text(), "hi");
    }

    #[test]
    fn test_enter_submits() {
        let mut ta = TextArea::new();
        ta.handle_key(make_key(KeyCode::Char('t'), KeyModifiers::NONE));
        ta.handle_key(make_key(KeyCode::Char('e'), KeyModifiers::NONE));
        ta.handle_key(make_key(KeyCode::Char('s'), KeyModifiers::NONE));
        ta.handle_key(make_key(KeyCode::Char('t'), KeyModifiers::NONE));
        ta.handle_key(make_key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(ta.is_submitted());
        assert_eq!(ta.take_submitted(), Some("test".to_string()));
    }

    #[test]
    fn test_shift_enter_newline() {
        let mut ta = TextArea::new();
        ta.handle_key(make_key(KeyCode::Char('a'), KeyModifiers::NONE));
        ta.handle_key(make_key(KeyCode::Enter, KeyModifiers::SHIFT));
        ta.handle_key(make_key(KeyCode::Char('b'), KeyModifiers::NONE));
        assert_eq!(ta.get_text(), "a\nb");
    }

    #[test]
    fn test_backspace() {
        let mut ta = TextArea::new();
        ta.handle_key(make_key(KeyCode::Char('a'), KeyModifiers::NONE));
        ta.handle_key(make_key(KeyCode::Char('b'), KeyModifiers::NONE));
        ta.handle_key(make_key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(ta.get_text(), "a");
    }

    #[test]
    fn test_backspace_at_line_start() {
        let mut ta = TextArea::new();
        ta.handle_key(make_key(KeyCode::Char('a'), KeyModifiers::NONE));
        ta.handle_key(make_key(KeyCode::Enter, KeyModifiers::SHIFT));
        // 光标在第二行开头，backspace 应合并到上一行
        ta.handle_key(make_key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(ta.get_text(), "a");
    }

    #[test]
    fn test_history_navigation() {
        let mut ta = TextArea::with_history(vec!["first".into(), "second".into()]);
        // 单行空输入时 Up 浏览历史
        ta.handle_key(make_key(KeyCode::Up, KeyModifiers::NONE));
        eprintln!("After 1st Up: text='{}', idx={:?}", ta.get_text(), ta.history_index);
        assert_eq!(ta.get_text(), "second");
        assert_eq!(ta.history_index, Some(1));

        ta.handle_key(make_key(KeyCode::Up, KeyModifiers::NONE));
        eprintln!("After 2nd Up: text='{}', idx={:?}", ta.get_text(), ta.history_index);
        assert_eq!(ta.get_text(), "first");
        assert_eq!(ta.history_index, Some(0));

        ta.handle_key(make_key(KeyCode::Down, KeyModifiers::NONE));
        eprintln!("After 1st Down: text='{}', idx={:?}", ta.get_text(), ta.history_index);
        assert_eq!(ta.get_text(), "second");
        assert_eq!(ta.history_index, Some(1));

        ta.handle_key(make_key(KeyCode::Down, KeyModifiers::NONE));
        eprintln!("After 2nd Down: text='{}', idx={:?}", ta.get_text(), ta.history_index);
        assert_eq!(ta.get_text(), "");
        assert_eq!(ta.history_index, None);
    }

    #[test]
    fn test_ctrl_u_clears_line() {
        let mut ta = TextArea::new();
        ta.handle_key(make_key(KeyCode::Char('a'), KeyModifiers::NONE));
        ta.handle_key(make_key(KeyCode::Char('b'), KeyModifiers::NONE));
        ta.handle_key(make_key(KeyCode::Char('c'), KeyModifiers::NONE));
        ta.handle_key(make_key(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(ta.get_text(), "");
    }
}
