use std::collections::VecDeque;

use ratatui::style::Color;

const MAX_LIVE_BLOCKS: usize = 64;
const MAX_LIVE_TEXT_CHARS: usize = 32_768;
const MAX_TOOL_DETAIL_LINES: usize = 20;
const TOOL_DETAIL_SCROLL_STEP: usize = 4;
const LIVE_SCROLL_STEP: usize = 4;
const MAX_LIVE_INSPECT_OFFSET: usize = 512;
pub(crate) const MAX_TOOL_HISTORY: usize = 64;
/// Answer 已占用 Live 视口时仍保留一行实际 reasoning；纯思考阶段不钳位。
const LIVE_REASONING_ROWS: usize = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LiveLineKind {
    Answer,
    Reasoning,
    ToolSummary,
    ToolDetail,
    Splash,
}

/// 最近实际收到的流通道；仅由已存在的 LiveBlock 推导，不保存第二份流状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LiveChannel {
    Answer,
    Reasoning,
    Tool,
}

#[derive(Clone, Debug)]
pub(crate) struct LiveLine<'a> {
    pub(crate) text: &'a str,
    pub(crate) color: Color,
    pub(crate) kind: LiveLineKind,
    pub(crate) marker: Option<&'static str>,
    /// Markdown fenced-code state immediately before this Answer line.
    /// Render-only metadata keeps a clipped tail faithful to the actual stream.
    pub(crate) fence_before: bool,
    /// A hidden prefix exists before this visible Reasoning tail.
    /// Render-only metadata keeps truncation visible without consuming a row.
    pub(crate) continuation_before: bool,
}

impl<'a> LiveLine<'a> {
    fn new(text: &'a str, color: Color, kind: LiveLineKind) -> Self {
        Self {
            text,
            color,
            kind,
            marker: None,
            fence_before: false,
            continuation_before: false,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ToolBlock {
    id: u64,
    summary: String,
    summary_color: Color,
    details: Vec<(String, Color)>,
    expanded: bool,
    /// Rows scrolled upward from the newest detail tail; zero means latest.
    detail_scroll: usize,
}

impl ToolBlock {
    pub(crate) fn from_lines(lines: Vec<(String, Color)>) -> Option<Self> {
        let mut remaining = lines
            .into_iter()
            .map(|(text, color)| (super::render::sanitize_display_text(&text), color));
        let (summary, summary_color) = remaining.next()?;
        let mut details = remaining
            .by_ref()
            .take(MAX_TOOL_DETAIL_LINES)
            .collect::<Vec<_>>();
        if remaining.next().is_some() {
            details.pop();
            details.push(("  [more details in trace]".to_owned(), Color::DarkGray));
        }
        Some(Self {
            id: 0,
            summary,
            summary_color,
            details,
            expanded: false,
            detail_scroll: 0,
        })
    }

    pub(crate) fn toggle(&mut self) -> bool {
        self.expanded = !self.expanded;
        if !self.expanded {
            self.detail_scroll = 0;
        }
        self.expanded
    }

    fn scroll_details(&mut self, delta: i8) -> bool {
        if !self.expanded || self.details.len() < 2 {
            return false;
        }
        let before = self.detail_scroll;
        if delta > 0 {
            self.detail_scroll = self
                .detail_scroll
                .saturating_add(TOOL_DETAIL_SCROLL_STEP)
                .min(self.details.len().saturating_sub(1));
        } else if delta < 0 {
            self.detail_scroll = self.detail_scroll.saturating_sub(TOOL_DETAIL_SCROLL_STEP);
        }
        self.detail_scroll != before
    }

    fn has_scrollable_details(&self) -> bool {
        self.expanded && self.details.len() > 1
    }

    pub(crate) fn summary(&self) -> &str {
        &self.summary
    }

    pub(crate) fn details_text(&self) -> String {
        self.details
            .iter()
            .map(|(text, _)| text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[cfg(test)]
    pub(crate) fn live_lines(&self) -> Vec<LiveLine<'_>> {
        let mut lines = vec![LiveLine::new(
            self.summary.as_str(),
            self.summary_color,
            LiveLineKind::ToolSummary,
        )];
        if self.expanded {
            lines.extend(self.details.iter().map(|(text, color)| {
                LiveLine::new(text.as_str(), *color, LiveLineKind::ToolDetail)
            }));
        } else if !self.details.is_empty() {
            lines.push(LiveLine::new(
                "  [Ctrl+O details]",
                Color::DarkGray,
                LiveLineKind::ToolDetail,
            ));
        }
        lines
    }

    fn append_live_tail<'a>(
        &'a self,
        target: &mut VecDeque<LiveLine<'a>>,
        max_rows: usize,
        focused: bool,
    ) {
        if max_rows == 0 {
            return;
        }
        if focused {
            let mut summary = LiveLine::new(
                self.summary.as_str(),
                self.summary_color,
                LiveLineKind::ToolSummary,
            );
            summary.marker = Some("▸ ");
            append_tail(target, std::iter::once(summary), max_rows);
            let detail_rows = max_rows.saturating_sub(1);
            if detail_rows == 0 {
                return;
            }
            if self.expanded {
                let max_offset = self.details.len().saturating_sub(detail_rows);
                let offset = self.detail_scroll.min(max_offset);
                let end = self.details.len().saturating_sub(offset);
                let start = end.saturating_sub(detail_rows);
                append_tail(
                    target,
                    self.details
                        .iter()
                        .skip(start)
                        .take(end.saturating_sub(start))
                        .map(|(text, color)| {
                            LiveLine::new(text.as_str(), *color, LiveLineKind::ToolDetail)
                        }),
                    max_rows,
                );
            } else if !self.details.is_empty() {
                append_tail(
                    target,
                    std::iter::once(LiveLine::new(
                        "  [Ctrl+O details]",
                        Color::DarkGray,
                        LiveLineKind::ToolDetail,
                    )),
                    max_rows,
                );
            }
            return;
        }
        // Live inspection belongs to the focused block.  A block that loses
        // focus returns to its compact projection, so an expanded detail tail
        // cannot evict the summary of the tool that produced it.
        let summary = LiveLine::new(
            self.summary.as_str(),
            self.summary_color,
            LiveLineKind::ToolSummary,
        );
        append_tail(target, std::iter::once(summary), max_rows);
        if !self.details.is_empty() {
            append_tail(
                target,
                std::iter::once(LiveLine::new(
                    "  [Ctrl+O details]",
                    Color::DarkGray,
                    LiveLineKind::ToolDetail,
                )),
                max_rows,
            );
        }
    }

    pub(crate) fn commit_lines(&self) -> Vec<(String, Color)> {
        let mut lines = vec![(self.summary.clone(), self.summary_color)];
        if self.expanded {
            lines.extend(self.details.iter().cloned());
        }
        lines
    }
}

#[derive(Clone, Debug)]
struct AnswerBlock {
    text: String,
    /// Byte offsets of lines whose trimmed content starts a fenced code block.
    /// Keeping these offsets moves fence scanning out of every live redraw.
    fence_starts: Vec<usize>,
    last_line_start: usize,
}

#[derive(Clone, Debug)]
enum LiveBlock {
    Answer(AnswerBlock),
    Reasoning(String),
    Tool(ToolBlock),
}

#[derive(Clone, Debug, Default)]
pub(crate) struct LiveTranscript {
    blocks: VecDeque<LiveBlock>,
    splash: Option<String>,
    next_tool_id: u64,
    focused_tool: Option<u64>,
    /// 用户用 Alt+↑/↓ 选定旧工具后，暂时阻止新工具夺走焦点。
    focus_pinned: bool,
    reasoning_expanded: bool,
    /// Rows behind the newest live tail; zero keeps the default Follow view.
    inspect_offset: usize,
    answer_chars: usize,
    reasoning_chars: usize,
}

impl LiveTranscript {
    pub(crate) fn push_answer(&mut self, text: &str) -> Vec<ToolBlock> {
        self.splash = None;
        let text = super::render::sanitize_display_text(text);
        if text.is_empty() {
            return Vec::new();
        }
        match self.blocks.back_mut() {
            Some(LiveBlock::Answer(current)) => {
                append_answer_bounded(current, &mut self.answer_chars, &text)
            }
            _ => {
                // A newly opened Answer phase restores the default readable
                // projection; Ctrl+R remains the explicit inspection escape.
                self.reasoning_expanded = false;
                self.inspect_offset = 0;
                self.answer_chars = 0;
                let mut current = AnswerBlock {
                    text: String::new(),
                    fence_starts: Vec::new(),
                    last_line_start: 0,
                };
                append_answer_bounded(&mut current, &mut self.answer_chars, &text);
                self.blocks.push_back(LiveBlock::Answer(current));
            }
        }
        self.trim_blocks()
    }

    pub(crate) fn push_reasoning(&mut self, text: &str) -> Vec<ToolBlock> {
        self.splash = None;
        let text = super::render::sanitize_display_text(text);
        if text.is_empty() {
            return Vec::new();
        }
        match self.blocks.back_mut() {
            Some(LiveBlock::Reasoning(current)) => {
                append_bounded(current, &mut self.reasoning_chars, &text)
            }
            _ => {
                self.reasoning_chars = 0;
                let mut current = String::new();
                append_bounded(&mut current, &mut self.reasoning_chars, &text);
                self.blocks.push_back(LiveBlock::Reasoning(current));
            }
        }
        self.trim_blocks()
    }

    pub(crate) fn push_tool(&mut self, mut block: ToolBlock) -> Vec<ToolBlock> {
        self.splash = None;
        block.id = self.next_tool_id;
        self.next_tool_id = self.next_tool_id.wrapping_add(1);
        if !self.focus_pinned {
            self.focused_tool = Some(block.id);
        }
        self.blocks.push_back(LiveBlock::Tool(block));
        self.trim_blocks()
    }

    pub(crate) fn clear_streams(&mut self) {
        self.blocks
            .retain(|block| matches!(block, LiveBlock::Tool(_)));
        self.answer_chars = 0;
        self.reasoning_chars = 0;
        self.reasoning_expanded = false;
        self.inspect_offset = 0;
        self.focus_pinned = false;
        if !self.has_tools() {
            self.focused_tool = None;
        }
        self.splash = None;
    }

    pub(crate) fn set_splash(&mut self, text: String) {
        self.blocks.clear();
        self.answer_chars = 0;
        self.reasoning_chars = 0;
        self.reasoning_expanded = false;
        self.inspect_offset = 0;
        self.focused_tool = None;
        self.focus_pinned = false;
        self.splash = Some(super::render::sanitize_display_text(&text));
    }

    pub(crate) fn drain_tools(&mut self) -> Vec<ToolBlock> {
        let mut retained = VecDeque::new();
        let mut tools = Vec::new();
        while let Some(block) = self.blocks.pop_front() {
            match block {
                LiveBlock::Tool(tool) => tools.push(tool),
                other => retained.push_back(other),
            }
        }
        self.blocks = retained;
        self.focused_tool = None;
        self.focus_pinned = false;
        tools
    }

    pub(crate) fn drain_reasoning(&mut self) -> Vec<String> {
        let mut retained = VecDeque::new();
        let mut reasoning = Vec::new();
        while let Some(block) = self.blocks.pop_front() {
            match block {
                LiveBlock::Reasoning(text) => reasoning.push(text),
                other => retained.push_back(other),
            }
        }
        self.blocks = retained;
        self.reasoning_chars = 0;
        reasoning
    }

    pub(crate) fn has_tools(&self) -> bool {
        self.blocks
            .iter()
            .any(|block| matches!(block, LiveBlock::Tool(_)))
    }

    pub(crate) fn has_reasoning(&self) -> bool {
        self.blocks
            .iter()
            .any(|block| matches!(block, LiveBlock::Reasoning(_)))
    }

    pub(crate) fn has_inspectable_output(&self) -> bool {
        self.blocks
            .iter()
            .any(|block| matches!(block, LiveBlock::Answer(_) | LiveBlock::Reasoning(_)))
    }

    pub(crate) fn scroll_live(&mut self, delta: i8) -> bool {
        if !self.has_inspectable_output() {
            return false;
        }
        let before = self.inspect_offset;
        if delta > 0 {
            self.inspect_offset = self
                .inspect_offset
                .saturating_add(LIVE_SCROLL_STEP)
                .min(MAX_LIVE_INSPECT_OFFSET);
        } else if delta < 0 {
            self.inspect_offset = self.inspect_offset.saturating_sub(LIVE_SCROLL_STEP);
        }
        self.inspect_offset != before
    }

    pub(crate) fn follow_live(&mut self) -> bool {
        let changed = self.inspect_offset != 0;
        self.inspect_offset = 0;
        changed
    }

    pub(crate) fn is_inspecting(&self) -> bool {
        self.inspect_offset != 0
    }

    /// 顶栏焦点 chip 只读取当前已净化的工具摘要，不复制工具详情或引入新状态。
    pub(crate) fn focused_tool_summary(&self) -> Option<&str> {
        let focused = self.focused_tool?;
        self.blocks.iter().find_map(|block| match block {
            LiveBlock::Tool(tool) if tool.id == focused => Some(tool.summary()),
            _ => None,
        })
    }

    /// 顶部 chrome 的真实通道 badge：只看最后一个 LiveBlock，不推断模型隐藏状态。
    pub(crate) fn active_channel(&self) -> Option<LiveChannel> {
        self.blocks.back().map(|block| match block {
            LiveBlock::Answer(_) => LiveChannel::Answer,
            LiveBlock::Reasoning(_) => LiveChannel::Reasoning,
            LiveBlock::Tool(_) => LiveChannel::Tool,
        })
    }

    pub(crate) fn move_tool_focus(&mut self, delta: i8) -> bool {
        let ids: Vec<u64> = self
            .blocks
            .iter()
            .filter_map(|block| match block {
                LiveBlock::Tool(tool) => Some(tool.id),
                _ => None,
            })
            .collect();
        if ids.is_empty() {
            return false;
        }
        let current = self
            .focused_tool
            .and_then(|id| ids.iter().position(|candidate| *candidate == id))
            .unwrap_or(ids.len() - 1);
        let next = if delta < 0 {
            current.saturating_sub(1)
        } else {
            current.saturating_add(1).min(ids.len() - 1)
        };
        let changed = self.focused_tool != Some(ids[next]);
        self.focused_tool = Some(ids[next]);
        self.focus_pinned = next + 1 < ids.len();
        changed
    }

    pub(crate) fn scroll_tool_details(&mut self, delta: i8) -> bool {
        let Some(focused_id) = self.focused_tool else {
            return false;
        };
        self.blocks
            .iter_mut()
            .find_map(|block| match block {
                LiveBlock::Tool(tool) if tool.id == focused_id => Some(tool.scroll_details(delta)),
                _ => None,
            })
            .unwrap_or(false)
    }

    pub(crate) fn has_scrollable_tool_details(&self) -> bool {
        let Some(focused_id) = self.focused_tool else {
            return false;
        };
        self.blocks.iter().any(|block| {
            matches!(block, LiveBlock::Tool(tool) if tool.id == focused_id && tool.has_scrollable_details())
        })
    }

    pub(crate) fn toggle_details(&mut self) -> bool {
        let target = self.focused_tool.or_else(|| {
            self.blocks.iter().rev().find_map(|block| match block {
                LiveBlock::Tool(tool) => Some(tool.id),
                _ => None,
            })
        });
        let Some(target) = target else {
            return false;
        };
        for block in &mut self.blocks {
            if let LiveBlock::Tool(tool) = block {
                if tool.id == target {
                    self.focused_tool = Some(target);
                    return tool.toggle();
                }
            }
        }
        false
    }

    pub(crate) fn toggle_reasoning(&mut self) -> bool {
        if !self.has_reasoning() {
            return false;
        }
        self.reasoning_expanded = !self.reasoning_expanded;
        self.reasoning_expanded
    }

    pub(crate) fn is_reasoning_expanded(&self) -> bool {
        self.reasoning_expanded
    }

    pub(crate) fn visible_lines<'a>(&'a self, max_rows: usize) -> Vec<LiveLine<'a>> {
        if max_rows == 0 {
            return Vec::new();
        }
        let requested_rows = max_rows;
        let inspect_offset = self.inspect_offset.min(MAX_LIVE_INSPECT_OFFSET);
        let max_rows = requested_rows.saturating_add(inspect_offset);
        if let Some(splash) = &self.splash {
            return tail_lines(
                text_lines(splash, Color::White, LiveLineKind::Splash),
                max_rows,
            );
        }

        // Keep only rows that can reach this frame.  A long-running task may
        // retain 64 blocks, but the viewport is bounded; materializing every
        // collapsed/expanded tool row on every spinner frame needlessly turns
        // redraw cost into O(blocks × detail).
        let mut answers = VecDeque::with_capacity(max_rows);
        let mut reasoning = VecDeque::with_capacity(max_rows);
        let mut other = VecDeque::with_capacity(max_rows);
        let focused_id = self.focused_tool;
        let last_answer_index = self
            .blocks
            .iter()
            .rposition(|block| matches!(block, LiveBlock::Answer(_)));
        let has_answer = last_answer_index.is_some();
        let focused_tool_expanded = focused_id.is_some_and(|focused_id| {
            self.blocks.iter().any(
                |block| matches!(block, LiveBlock::Tool(tool) if tool.id == focused_id && tool.expanded),
            )
        });
        let mut last_answer_text = None;
        let mut answer_fence = false;
        let mut reasoning_truncated = false;
        for (block_index, block) in self.blocks.iter().enumerate() {
            match block {
                LiveBlock::Answer(answer) => {
                    if Some(block_index) == last_answer_index {
                        last_answer_text = Some(answer.text.as_str());
                    }
                    answer_fence = append_answer_tail(
                        &mut answers,
                        answer,
                        Color::White,
                        Some("🤖 "),
                        answer_fence,
                        max_rows,
                    );
                }
                LiveBlock::Reasoning(text) => {
                    // Before the first Answer, keep the actual Reasoning/Tool
                    // block order; once Answer exists, the dedicated lanes
                    // intentionally enforce Answer-first budgeting.
                    let target = if has_answer {
                        &mut reasoning
                    } else {
                        &mut other
                    };
                    reasoning_truncated |= append_text_tail(
                        target,
                        text,
                        Color::DarkGray,
                        LiveLineKind::Reasoning,
                        Some("💭 "),
                        max_rows,
                    );
                }
                LiveBlock::Tool(tool) => {
                    if focused_id != Some(tool.id) {
                        tool.append_live_tail(&mut other, max_rows, false);
                    }
                }
            }
        }

        // Default view keeps Answer readable and leaves one actual reasoning row.
        // Ctrl+R opts into an inspection view: reasoning gets the remaining rows,
        // while Answer keeps one row and a focused tool keeps its summary.
        let reserve_reasoning = !self.reasoning_expanded
            && !answers.is_empty()
            && !focused_tool_expanded
            && !reasoning.is_empty()
            && max_rows > LIVE_REASONING_ROWS + usize::from(focused_id.is_some());
        let answer_budget = if self.reasoning_expanded {
            let focus_reservation = usize::from(focused_id.is_some() && max_rows > 1);
            usize::from(!answers.is_empty()).min(max_rows.saturating_sub(focus_reservation))
        } else {
            let reserved = usize::from(focused_id.is_some()) + usize::from(reserve_reasoning);
            max_rows.saturating_sub(reserved)
        };
        let answers = pin_answer_header(
            into_tail(answers, answer_budget),
            last_answer_text,
            answer_budget,
        );
        let mut focused = VecDeque::with_capacity(max_rows);
        if let Some(focused_id) = focused_id {
            if let Some(LiveBlock::Tool(tool)) = self
                .blocks
                .iter()
                .find(|block| matches!(block, LiveBlock::Tool(tool) if tool.id == focused_id))
            {
                let focus_budget = if self.reasoning_expanded && tool.expanded {
                    // Ctrl+O must remain observable during Ctrl+R inspection:
                    // keep the focused summary, borrow remaining rows for its
                    // bounded details, and leave one row for actual reasoning
                    // whenever the viewport has room.
                    let available = max_rows.saturating_sub(answers.len());
                    let reasoning_floor = usize::from(!reasoning.is_empty());
                    if available <= 1 {
                        available
                    } else {
                        available.saturating_sub(reasoning_floor).max(1)
                    }
                } else if self.reasoning_expanded {
                    1
                } else {
                    max_rows
                        .saturating_sub(answers.len())
                        .saturating_sub(usize::from(reserve_reasoning))
                        .max(1)
                };
                tool.append_live_tail(&mut focused, focus_budget, true);
            }
        }
        let focus_rows = focused.len();
        let remaining = max_rows.saturating_sub(answers.len() + focus_rows);
        let reasoning_rows = reasoning.len();
        let mut visible = if self.reasoning_expanded && !reasoning.is_empty() {
            into_tail(reasoning, remaining)
        } else if reserve_reasoning {
            let reasoning = into_tail(reasoning, remaining.min(LIVE_REASONING_ROWS));
            let mut visible = into_tail(other, remaining.saturating_sub(reasoning.len()));
            visible.extend(reasoning);
            visible
        } else {
            // No Answer: retain the full reasoning tail, then let normal tail
            // clipping arbitrate against non-focused tool rows.
            let mut combined = reasoning;
            combined.extend(other);
            into_tail(combined, remaining)
        };
        visible.extend(focused);
        visible.extend(answers);
        let effective_offset = inspect_offset.min(visible.len().saturating_sub(requested_rows));
        if effective_offset > 0 {
            let end = visible.len().saturating_sub(effective_offset);
            let start = end.saturating_sub(requested_rows);
            visible = visible
                .into_iter()
                .skip(start)
                .take(end.saturating_sub(start))
                .collect();
            reasoning_truncated = true;
        }
        reasoning_truncated |= visible
            .iter()
            .filter(|line| line.kind == LiveLineKind::Reasoning)
            .count()
            < reasoning_rows;
        mark_reasoning_continuation(&mut visible, reasoning_truncated);
        ensure_marker(&mut visible, LiveLineKind::Reasoning, "💭 ");
        ensure_marker(&mut visible, LiveLineKind::Answer, "🤖 ");
        visible
    }

    fn trim_blocks(&mut self) -> Vec<ToolBlock> {
        let mut evicted = Vec::new();
        while self.blocks.len() > MAX_LIVE_BLOCKS {
            let Some(index) = self
                .blocks
                .iter()
                .position(|block| matches!(block, LiveBlock::Tool(_)))
            else {
                self.blocks.pop_front();
                continue;
            };
            if let Some(LiveBlock::Tool(tool)) = self.blocks.remove(index) {
                evicted.push(tool);
            }
        }
        if self.focused_tool.is_some_and(|id| {
            !self
                .blocks
                .iter()
                .any(|block| matches!(block, LiveBlock::Tool(tool) if tool.id == id))
        }) {
            self.focused_tool = self.blocks.iter().rev().find_map(|block| match block {
                LiveBlock::Tool(tool) => Some(tool.id),
                _ => None,
            });
            self.focus_pinned = false;
        }
        evicted
    }
}

fn append_bounded(target: &mut String, char_count: &mut usize, text: &str) {
    target.push_str(text);
    *char_count += text.chars().count();
    if *char_count > MAX_LIVE_TEXT_CHARS {
        let skip = *char_count - MAX_LIVE_TEXT_CHARS;
        let start = target
            .char_indices()
            .nth(skip)
            .map(|(index, _)| index)
            .unwrap_or(0);
        target.drain(..start);
        *char_count = MAX_LIVE_TEXT_CHARS;
    }
}

fn append_answer_bounded(target: &mut AnswerBlock, char_count: &mut usize, text: &str) {
    let old_len = target.text.len();
    let old_last_line_start = target.last_line_start;
    target.text.push_str(text);
    *char_count += text.chars().count();

    if *char_count > MAX_LIVE_TEXT_CHARS {
        let skip = *char_count - MAX_LIVE_TEXT_CHARS;
        let start = target
            .text
            .char_indices()
            .nth(skip)
            .map(|(index, _)| index)
            .unwrap_or(0);
        target.text.drain(..start);
        target.fence_starts.clear();
        *char_count = MAX_LIVE_TEXT_CHARS;
        rebuild_fence_starts(target);
    } else {
        target
            .fence_starts
            .retain(|&start| start < old_last_line_start);
        append_fence_starts(target, old_last_line_start, old_len);
        target.last_line_start = target.text[old_len..]
            .rfind('\n')
            .map_or(old_last_line_start, |index| old_len + index + 1);
    }
}

fn append_fence_starts(target: &mut AnswerBlock, line_start: usize, appended_start: usize) {
    let appended = &target.text[appended_start..];
    let Some(first_newline) = appended.find('\n') else {
        if is_fence_line(&target.text[line_start..]) {
            target.fence_starts.push(line_start);
        }
        return;
    };

    let first_line_end = appended_start + first_newline;
    if is_fence_line(&target.text[line_start..first_line_end]) {
        target.fence_starts.push(line_start);
    }

    let mut line_start = first_line_end + 1;
    for line in target.text[line_start..].split('\n') {
        if is_fence_line(line) {
            target.fence_starts.push(line_start);
        }
        line_start += line.len() + 1;
    }
}

fn rebuild_fence_starts(target: &mut AnswerBlock) {
    target.last_line_start = target.text.rfind('\n').map_or(0, |index| index + 1);
    let mut line_start = 0;
    for line in target.text.split('\n') {
        if is_fence_line(line) {
            target.fence_starts.push(line_start);
        }
        line_start += line.len() + 1;
    }
}

fn is_fence_line(line: &str) -> bool {
    line.trim_start().starts_with("```")
}

fn text_lines<'a>(text: &'a str, color: Color, kind: LiveLineKind) -> Vec<LiveLine<'a>> {
    text.split('\n')
        .map(|line| LiveLine::new(line, color, kind))
        .collect()
}

fn append_text_tail<'a>(
    target: &mut VecDeque<LiveLine<'a>>,
    text: &'a str,
    color: Color,
    kind: LiveLineKind,
    marker: Option<&'static str>,
    max_rows: usize,
) -> bool {
    if max_rows == 0 {
        return false;
    }
    let mut tail = VecDeque::with_capacity(max_rows);
    let mut lines = text.split('\n').rev();
    for line in lines.by_ref().take(max_rows) {
        tail.push_front(LiveLine::new(line, color, kind));
    }
    let text_truncated = lines.next().is_some();
    let target_truncated = target.len() + tail.len() > max_rows;
    if let (Some(marker), Some(first)) = (marker, tail.front_mut()) {
        first.marker = Some(marker);
    }
    append_tail(target, tail, max_rows);
    text_truncated || target_truncated
}

/// Answer 尾部渲染需知道被裁掉的围栏上下文；单次有界扫描只生成最多 `max_rows` 行，
/// 不物化完整 Markdown 文档，也不把解析状态写入模型内容。
fn append_answer_tail<'a>(
    target: &mut VecDeque<LiveLine<'a>>,
    answer: &'a AnswerBlock,
    color: Color,
    marker: Option<&'static str>,
    fence_before: bool,
    max_rows: usize,
) -> bool {
    if max_rows == 0 {
        return fence_before;
    }
    let mut tail = VecDeque::with_capacity(max_rows);
    for (start, end) in tail_ranges(&answer.text, max_rows) {
        let line = &answer.text[start..end];
        let fence_count = answer
            .fence_starts
            .partition_point(|&fence_start| fence_start < start);
        let mut rendered = LiveLine::new(line, color, LiveLineKind::Answer);
        rendered.fence_before = fence_before ^ (fence_count % 2 != 0);
        if tail.len() == max_rows {
            tail.pop_front();
        }
        tail.push_back(rendered);
    }
    if let (Some(marker), Some(first)) = (marker, tail.front_mut()) {
        first.marker = Some(marker);
    }
    append_tail(target, tail, max_rows);
    fence_before ^ !answer.fence_starts.len().is_multiple_of(2)
}

fn tail_ranges(text: &str, max_rows: usize) -> Vec<(usize, usize)> {
    if max_rows == 0 {
        return Vec::new();
    }
    let mut ranges = VecDeque::with_capacity(max_rows);
    let mut end = text.len();
    for (index, character) in text.char_indices().rev() {
        if character == '\n' {
            ranges.push_front((index + 1, end));
            end = index;
            if ranges.len() == max_rows {
                break;
            }
        }
    }
    if ranges.len() < max_rows {
        ranges.push_front((0, end));
    }
    ranges.into_iter().collect()
}

fn pin_answer_header<'a>(
    answers: Vec<LiveLine<'a>>,
    text: Option<&'a str>,
    max_rows: usize,
) -> Vec<LiveLine<'a>> {
    if max_rows < 3 {
        return answers;
    }
    let Some(text) = text else {
        return answers;
    };
    let Some(header) = text.split('\n').next() else {
        return answers;
    };
    if header.is_empty() || text.split('\n').nth(max_rows - 2).is_none() {
        return answers;
    }

    let header_fence_before = answers
        .first()
        .map(|line| line.fence_before)
        .unwrap_or(false);
    let mut tail = answers
        .into_iter()
        .rev()
        .take(max_rows - 2)
        .collect::<Vec<_>>();
    tail.reverse();
    let mut anchored = Vec::with_capacity(max_rows);
    let mut first = LiveLine::new(header, Color::White, LiveLineKind::Answer);
    first.fence_before = header_fence_before;
    first.marker = Some("🤖 ");
    anchored.push(first);
    let mut continuation = LiveLine::new(
        "  … answer continues",
        Color::DarkGray,
        LiveLineKind::Answer,
    );
    continuation.fence_before = if header.trim_start().starts_with("```") {
        !header_fence_before
    } else {
        header_fence_before
    };
    anchored.push(continuation);
    anchored.extend(tail);
    anchored
}

fn append_tail<'a, I>(target: &mut VecDeque<LiveLine<'a>>, lines: I, max_rows: usize)
where
    I: IntoIterator<Item = LiveLine<'a>>,
{
    if max_rows == 0 {
        return;
    }
    for line in lines {
        if target.len() == max_rows {
            target.pop_front();
        }
        target.push_back(line);
    }
}

fn into_tail<'a>(mut lines: VecDeque<LiveLine<'a>>, max_rows: usize) -> Vec<LiveLine<'a>> {
    while lines.len() > max_rows {
        lines.pop_front();
    }
    lines.into_iter().collect()
}

fn tail_lines<'a>(lines: Vec<LiveLine<'a>>, max_rows: usize) -> Vec<LiveLine<'a>> {
    into_tail(lines.into_iter().collect(), max_rows)
}

fn ensure_marker(lines: &mut [LiveLine<'_>], kind: LiveLineKind, marker: &'static str) {
    if lines.iter().any(|line| line.kind == kind)
        && !lines
            .iter()
            .any(|line| line.kind == kind && line.marker.is_some())
    {
        if let Some(line) = lines.iter_mut().find(|line| line.kind == kind) {
            line.marker = Some(marker);
        }
    }
}

fn mark_reasoning_continuation(lines: &mut [LiveLine<'_>], truncated: bool) {
    if !truncated {
        return;
    }
    if let Some(line) = lines
        .iter_mut()
        .find(|line| line.kind == LiveLineKind::Reasoning)
    {
        line.continuation_before = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_details_toggle() {
        let mut tool = ToolBlock::from_lines(vec![
            ("tool".into(), Color::Cyan),
            ("- old".into(), Color::Red),
            ("+ new".into(), Color::Green),
        ])
        .expect("tool");
        assert!(tool
            .live_lines()
            .iter()
            .any(|line| line.text.contains("[Ctrl+O details]")));
        assert!(tool.toggle());
        let expanded = tool.live_lines();
        assert!(expanded.iter().any(|line| line.text == "- old"));
        assert!(expanded.iter().any(|line| line.text == "+ new"));
    }

    #[test]
    fn answer_gets_reserved_tail_rows() {
        let mut transcript = LiveTranscript::default();
        transcript.push_reasoning("reasoning");
        transcript.push_tool(
            ToolBlock::from_lines(vec![
                ("tool".into(), Color::Cyan),
                ("detail".into(), Color::Gray),
            ])
            .expect("tool"),
        );
        transcript.push_answer("final answer");
        let lines = transcript.visible_lines(2);
        assert_eq!(lines.last().map(|line| line.text), Some("final answer"));
        assert_eq!(lines.last().and_then(|line| line.marker), Some("🤖 "));

        let mut reasoning = LiveTranscript::default();
        reasoning.push_reasoning("thinking");
        assert_eq!(
            reasoning
                .visible_lines(1)
                .first()
                .and_then(|line| line.marker),
            Some("💭 ")
        );
    }

    #[test]
    fn focused_collapsed_tool_preserves_reasoning_row_in_default_view() {
        let mut transcript = LiveTranscript::default();
        transcript.push_reasoning("r0\nr1");
        transcript.push_tool(
            ToolBlock::from_lines(vec![
                ("tool summary".into(), Color::Cyan),
                ("tool detail".into(), Color::Gray),
            ])
            .expect("tool"),
        );
        transcript.push_answer("a0\na1");

        let lines = transcript.visible_lines(4);
        assert_eq!(lines.len(), 4);
        assert!(lines
            .iter()
            .any(|line| line.kind == LiveLineKind::Reasoning && line.text == "r1"));
        assert!(lines
            .iter()
            .any(|line| line.text == "tool summary" && line.marker == Some("▸ ")));
        assert!(lines.iter().any(|line| line.text == "a0"));
        assert!(lines.iter().any(|line| line.text == "a1"));
        assert!(!lines
            .iter()
            .any(|line| line.text.contains("[Ctrl+O details]")));
    }

    #[test]
    fn answer_keeps_one_actual_reasoning_row_visible() {
        let mut transcript = LiveTranscript::default();
        transcript.push_reasoning("r0\nr1\nr2");
        transcript.push_answer("a0\na1\na2");

        let lines = transcript.visible_lines(3);
        assert_eq!(lines.len(), 3);
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.kind == LiveLineKind::Reasoning)
                .count(),
            LIVE_REASONING_ROWS
        );
        assert!(lines.iter().any(|line| line.text == "r2"));
        assert!(lines.iter().any(|line| line.text == "a1"));
        assert!(lines.iter().any(|line| line.text == "a2"));
        assert!(lines.iter().any(|line| line.marker == Some("💭 ")));
        assert!(lines.iter().any(|line| line.marker == Some("🤖 ")));
    }

    #[test]
    fn answer_arrival_collapses_reasoning_inspection() {
        let mut transcript = LiveTranscript::default();
        transcript.push_reasoning("r0\nr1\nr2");
        assert!(transcript.toggle_reasoning());
        transcript.push_answer("answer");

        assert!(!transcript.is_reasoning_expanded());
        let lines = transcript.visible_lines(4);
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.kind == LiveLineKind::Reasoning)
                .count(),
            LIVE_REASONING_ROWS
        );
        assert_eq!(lines.last().map(|line| line.text), Some("answer"));
        assert!(transcript.toggle_reasoning());
    }

    #[test]
    fn expanded_reasoning_keeps_answer_visible() {
        let mut transcript = LiveTranscript::default();
        transcript.push_reasoning("r0\nr1\nr2\nr3");
        transcript.push_answer("answer");

        assert!(transcript.toggle_reasoning());
        let lines = transcript.visible_lines(4);
        assert_eq!(lines.len(), 4);
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.kind == LiveLineKind::Reasoning)
                .count(),
            3
        );
        assert_eq!(lines.last().map(|line| line.text), Some("answer"));
        assert!(lines.iter().any(|line| line.text == "r1"));
        assert!(lines.iter().any(|line| line.text == "r3"));
        assert!(!transcript.toggle_reasoning());
    }

    #[test]
    fn reasoning_toggle_is_noop_without_actual_reasoning() {
        let mut transcript = LiveTranscript::default();
        assert!(!transcript.toggle_reasoning());
        assert!(!transcript.is_reasoning_expanded());

        transcript.push_answer("answer");
        assert!(!transcript.toggle_reasoning());
        assert!(!transcript.is_reasoning_expanded());

        transcript.push_reasoning("thinking");
        assert!(transcript.toggle_reasoning());
        assert!(transcript.is_reasoning_expanded());
    }

    #[test]
    fn expanded_tool_details_remain_visible_during_reasoning_inspection() {
        let mut transcript = LiveTranscript::default();
        transcript.push_reasoning("r0\nr1\nr2\nr3");
        transcript.push_tool(
            ToolBlock::from_lines(vec![
                ("tool".into(), Color::Cyan),
                ("detail 0".into(), Color::Gray),
                ("detail 1".into(), Color::Gray),
            ])
            .expect("tool"),
        );
        transcript.push_answer("answer");

        assert!(transcript.toggle_details());
        assert!(transcript.toggle_reasoning());
        let lines = transcript.visible_lines(6);
        assert_eq!(lines.last().map(|line| line.text), Some("answer"));
        assert!(lines
            .iter()
            .any(|line| line.text == "tool" && line.marker == Some("▸ ")));
        assert!(lines.iter().any(|line| line.text == "detail 1"));
        assert!(lines
            .iter()
            .any(|line| line.kind == LiveLineKind::Reasoning));
    }

    #[test]
    fn reasoning_inspection_keeps_answer_at_single_row_with_focused_tool() {
        let mut transcript = LiveTranscript::default();
        transcript.push_reasoning("thinking");
        transcript.push_tool(
            ToolBlock::from_lines(vec![
                ("tool".into(), Color::Cyan),
                ("detail".into(), Color::Gray),
            ])
            .expect("tool"),
        );
        transcript.push_answer("answer");

        assert!(transcript.toggle_details());
        assert!(transcript.toggle_reasoning());
        let lines = transcript.visible_lines(1);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "answer");
        assert_eq!(lines[0].marker, Some("🤖 "));
    }

    #[test]
    fn visible_tail_is_bounded_before_render_and_keeps_marker() {
        let mut transcript = LiveTranscript::default();
        transcript.push_reasoning("r0\nr1\nr2");
        transcript.push_tool(
            ToolBlock::from_lines(vec![
                ("tool".into(), Color::Cyan),
                ("detail".into(), Color::Gray),
            ])
            .expect("tool"),
        );

        let lines = transcript.visible_lines(3);
        assert_eq!(lines.len(), 3);
        assert_eq!(
            lines
                .iter()
                .find(|line| line.kind == LiveLineKind::Reasoning)
                .and_then(|line| line.marker),
            Some("💭 ")
        );
    }

    #[test]
    fn pre_answer_reasoning_and_tool_tail_keep_block_order() {
        let mut transcript = LiveTranscript::default();
        transcript.push_reasoning("thought before");
        transcript
            .push_tool(ToolBlock::from_lines(vec![("tool".into(), Color::Cyan)]).expect("tool"));
        transcript.push_reasoning("thought after");
        transcript.focused_tool = None;

        let texts = transcript
            .visible_lines(8)
            .into_iter()
            .map(|line| line.text)
            .collect::<Vec<_>>();
        assert_eq!(texts, vec!["thought before", "tool", "thought after",]);
    }

    #[test]
    fn answer_tail_keeps_fence_context_after_opener_leaves_viewport() {
        let mut transcript = LiveTranscript::default();
        transcript.push_answer("```rust\nline 0\nline 1\nline 2");

        let lines = transcript.visible_lines(2);
        assert_eq!(
            lines.iter().map(|line| line.text).collect::<Vec<_>>(),
            vec!["line 1", "line 2"]
        );
        assert!(lines.iter().all(|line| line.fence_before));
    }

    #[test]
    fn answer_fence_cache_handles_split_markers_and_closing_fence() {
        let mut transcript = LiveTranscript::default();
        transcript.push_answer("``");
        transcript.push_answer("`rust\nhidden 0\nhidden 1");
        transcript.push_answer("\n```");

        let answer = match transcript.blocks.back().expect("answer block") {
            LiveBlock::Answer(answer) => answer,
            _ => panic!("expected answer block"),
        };
        assert_eq!(answer.fence_starts.len(), 2);
        let lines = transcript.visible_lines(2);
        assert_eq!(lines[0].text, "hidden 1");
        assert_eq!(lines[1].text, "```");
        assert!(lines.iter().all(|line| line.fence_before));

        transcript.push_answer("\nafter");
        let lines = transcript.visible_lines(1);
        assert_eq!(lines[0].text, "after");
        assert!(!lines[0].fence_before);
    }

    #[test]
    fn answer_tail_ranges_only_keep_requested_viewport_rows() {
        let text = (0..128)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let ranges = tail_ranges(&text, 3);
        assert_eq!(ranges.len(), 3);
        assert!(ranges[0].0 > 0);
        assert_eq!(
            ranges
                .iter()
                .map(|&(start, end)| &text[start..end])
                .collect::<Vec<_>>(),
            vec!["line 125", "line 126", "line 127"]
        );
    }

    #[test]
    fn tool_focus_moves_and_ctrl_o_targets_focused_block() {
        let mut transcript = LiveTranscript::default();
        transcript.push_tool(
            ToolBlock::from_lines(vec![
                ("tool 0".into(), Color::Cyan),
                ("detail 0".into(), Color::Gray),
            ])
            .expect("tool 0"),
        );
        transcript.push_tool(
            ToolBlock::from_lines(vec![
                ("tool 1".into(), Color::Cyan),
                ("detail 1".into(), Color::Gray),
            ])
            .expect("tool 1"),
        );
        assert!(transcript.move_tool_focus(-1));
        assert!(transcript.toggle_details());
        let lines = transcript.visible_lines(8);
        assert!(lines.iter().any(|line| line.text == "detail 0"));
        assert!(lines
            .iter()
            .any(|line| line.text == "tool 0" && line.marker == Some("▸ ")));
        assert!(!lines.iter().any(|line| line.text == "detail 1"));
    }

    #[test]
    fn manual_tool_focus_stays_pinned_until_latest_is_selected() {
        let mut transcript = LiveTranscript::default();
        transcript.push_tool(
            ToolBlock::from_lines(vec![
                ("old tool".into(), Color::Cyan),
                ("old detail".into(), Color::Gray),
            ])
            .expect("old tool"),
        );
        transcript.push_tool(
            ToolBlock::from_lines(vec![("current tool".into(), Color::Cyan)])
                .expect("current tool"),
        );
        assert!(transcript.move_tool_focus(-1));
        assert!(transcript.toggle_details());

        transcript.push_tool(
            ToolBlock::from_lines(vec![("new tool".into(), Color::Cyan)]).expect("new tool"),
        );
        assert_eq!(transcript.focused_tool, Some(0));
        assert!(transcript
            .visible_lines(6)
            .iter()
            .any(|line| line.text == "old detail"));

        assert!(transcript.move_tool_focus(1));
        assert!(transcript.move_tool_focus(1));
        assert!(!transcript.focus_pinned);
        transcript.push_tool(
            ToolBlock::from_lines(vec![("follow tool".into(), Color::Cyan)]).expect("follow tool"),
        );
        assert_eq!(transcript.focused_tool, Some(3));
    }

    #[test]
    fn focused_tool_keeps_summary_when_expanded_tail_is_tall() {
        let mut transcript = LiveTranscript::default();
        transcript.push_tool(
            ToolBlock::from_lines(
                std::iter::once(("old tool".to_owned(), Color::Cyan))
                    .chain((0..8).map(|index| (format!("old detail {index}"), Color::Gray)))
                    .collect(),
            )
            .expect("old tool"),
        );
        transcript.push_tool(
            ToolBlock::from_lines(vec![("new tool".into(), Color::Cyan)]).expect("new tool"),
        );
        assert!(transcript.move_tool_focus(-1));
        assert!(transcript.toggle_details());

        let lines = transcript.visible_lines(4);
        assert_eq!(lines.len(), 4);
        assert!(lines
            .iter()
            .any(|line| line.text == "old tool" && line.marker == Some("▸ ")));
        assert!(lines.iter().any(|line| line.text == "old detail 7"));
    }

    #[test]
    fn non_focused_expanded_tool_keeps_summary_when_tail_is_tall() {
        let mut transcript = LiveTranscript::default();
        transcript.push_tool(
            ToolBlock::from_lines(
                std::iter::once(("old tool".to_owned(), Color::Cyan))
                    .chain((0..8).map(|index| (format!("old detail {index}"), Color::Gray)))
                    .collect(),
            )
            .expect("old tool"),
        );
        transcript.push_tool(
            ToolBlock::from_lines(vec![("new tool".into(), Color::Cyan)]).expect("new tool"),
        );
        assert!(transcript.move_tool_focus(-1));
        assert!(transcript.toggle_details());
        assert!(transcript.move_tool_focus(1));

        let lines = transcript.visible_lines(4);
        assert!(lines.iter().any(|line| line.text == "old tool"));
        assert!(lines.iter().any(|line| line.text == "new tool"));
        assert!(!lines.iter().any(|line| line.text == "old detail 7"));
    }

    #[test]
    fn splash_clears_stale_tool_focus() {
        let mut transcript = LiveTranscript::default();
        transcript
            .push_tool(ToolBlock::from_lines(vec![("tool".into(), Color::Cyan)]).expect("tool"));
        transcript.set_splash("loading".into());
        assert!(transcript.focused_tool.is_none());
        transcript.push_answer("answer");
        assert_eq!(
            transcript
                .visible_lines(2)
                .first()
                .and_then(|line| line.marker),
            Some("🤖 ")
        );
    }

    #[test]
    fn live_blocks_and_tool_details_stay_bounded() {
        let mut transcript = LiveTranscript::default();
        for index in 0..(MAX_LIVE_BLOCKS + 8) {
            transcript.push_tool(
                ToolBlock::from_lines(vec![(format!("tool {index}"), Color::Cyan)]).expect("tool"),
            );
        }
        assert!(transcript.blocks.len() <= MAX_LIVE_BLOCKS);

        let lines: Vec<_> = (0..(MAX_TOOL_DETAIL_LINES + 8))
            .map(|index| (format!("detail {index}"), Color::Gray))
            .collect();
        let mut tool = ToolBlock::from_lines(
            std::iter::once(("tool".to_owned(), Color::Cyan))
                .chain(lines)
                .collect(),
        )
        .expect("tool");
        tool.toggle();
        assert_eq!(tool.live_lines().len(), 1 + MAX_TOOL_DETAIL_LINES);
    }

    #[test]
    fn stream_text_cap_is_preserved_across_chunks() {
        let mut transcript = LiveTranscript::default();
        transcript.push_answer(&"a".repeat(MAX_LIVE_TEXT_CHARS + 3));
        transcript.push_answer("xyz");

        let text = match transcript.blocks.back().expect("answer block") {
            LiveBlock::Answer(answer) => &answer.text,
            _ => panic!("expected answer block"),
        };
        assert_eq!(text.chars().count(), MAX_LIVE_TEXT_CHARS);
        assert!(text.ends_with("axyz"));
    }
}
