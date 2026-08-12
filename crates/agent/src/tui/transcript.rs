use std::collections::VecDeque;

use ratatui::style::Color;

use super::{role_color, PresentationId, Role};

const MAX_LIVE_BLOCKS: usize = 64;
const MAX_LIVE_TEXT_CHARS: usize = 32_768;
const MAX_READ_BATCH_PATHS: usize = 8;
const MAX_READ_BATCH_PATH_CHARS: usize = 96;
const TOOL_DETAIL_SCROLL_STEP: usize = 4;
const LIVE_SCROLL_STEP: usize = 4;
const MAX_LIVE_INSPECT_OFFSET: usize = 512;
const MAX_LIVE_PHASE_TRACE: usize = 5;
pub(crate) const MAX_TOOL_HISTORY: usize = 64;
/// Answer 已占用 Live 视口时仍保留一小段实际 reasoning；纯思考阶段不钳位。
/// 视口越宽裕，预览越长；Ctrl+R 仍负责完整展开。
const MAX_LIVE_REASONING_PREVIEW_ROWS: usize = 3;

fn default_reasoning_preview_rows(view_rows: usize) -> usize {
    match view_rows {
        0..=6 => 1,
        7..=9 => 2,
        _ => MAX_LIVE_REASONING_PREVIEW_ROWS,
    }
}

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
    /// Stable semantic position used only when a held viewport is reflowed.
    /// Physical rows are disposable; block identity plus logical line is not.
    pub(crate) anchor: Option<LiveLineAnchor>,
    /// The source Answer block contains no Markdown markers.  This lets the
    /// renderer tail a long unbroken line without rescanning it each frame.
    pub(crate) answer_plain: bool,
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
            anchor: None,
            answer_plain: false,
            fence_before: false,
            continuation_before: false,
        }
    }

    fn with_anchor(mut self, anchor: LiveLineAnchor) -> Self {
        self.anchor = Some(anchor);
        self
    }
}

/// Incremental byte offsets for logical text lines.  This is render metadata:
/// it lets a bounded tail query jump to the visible lines without rescanning
/// the whole Answer/Reasoning buffer on every streamed frame.
#[derive(Clone, Debug)]
struct LineIndex {
    starts: Vec<usize>,
}

impl Default for LineIndex {
    fn default() -> Self {
        Self { starts: vec![0] }
    }
}

impl LineIndex {
    fn append(&mut self, base: usize, text: &str) {
        self.starts
            .extend(text.char_indices().filter_map(|(offset, character)| {
                (character == '\n').then_some(base + offset + 1)
            }));
    }

    fn trim_prefix(&mut self, prefix_len: usize) {
        let mut shifted = Vec::with_capacity(self.starts.len());
        shifted.push(0);
        shifted.extend(
            self.starts
                .iter()
                .copied()
                .filter(|&start| start > prefix_len)
                .map(|start| start - prefix_len),
        );
        self.starts = shifted;
    }

    fn last_start(&self) -> usize {
        self.starts.last().copied().unwrap_or(0)
    }

    fn line_count(&self) -> usize {
        self.starts.len()
    }

    fn tail_ranges(&self, text: &str, max_rows: usize) -> Vec<(usize, usize)> {
        if max_rows == 0 || text.is_empty() {
            return Vec::new();
        }
        let first = self.starts.len().saturating_sub(max_rows);
        self.starts[first..]
            .iter()
            .enumerate()
            .map(|(offset, &start)| {
                let start = start.min(text.len());
                let end = self
                    .starts
                    .get(first + offset + 1)
                    .map(|&next| next.saturating_sub(1))
                    .unwrap_or(text.len())
                    .max(start)
                    .min(text.len());
                (start, end)
            })
            .collect()
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ToolPhase {
    #[cfg(test)]
    Standalone,
    Call,
    Observation,
}

#[derive(Clone, Debug)]
pub(crate) struct ToolBlock {
    id: u64,
    summary: String,
    summary_color: Color,
    details: Vec<(String, Color)>,
    /// Stable, bounded affordance text for the collapsed projection. Keeping
    /// it with the block lets live and scrollback renderers share the same
    /// detail-count signal without borrowing a temporary String.
    collapsed_hint: String,
    expanded: bool,
    /// Rows scrolled upward from the newest detail tail; zero means latest.
    detail_scroll: usize,
    phase: ToolPhase,
    tool_name: Option<String>,
    /// Number of adjacent completed `read_file` observations represented by this
    /// presentation block.  Zero means this is not a read batch.
    read_batch_count: usize,
    read_batch_error: bool,
    read_batch_paths: Vec<String>,
}

impl ToolBlock {
    #[cfg(test)]
    pub(crate) fn from_lines(lines: Vec<(String, Color)>) -> Option<Self> {
        Self::from_lines_with_phase(lines, ToolPhase::Standalone, None)
    }

    pub(crate) fn from_lines_with_phase(
        lines: Vec<(String, Color)>,
        phase: ToolPhase,
        tool_name: Option<String>,
    ) -> Option<Self> {
        let mut remaining = lines
            .into_iter()
            .map(|(text, color)| (super::render::sanitize_display_text(&text), color));
        let (summary, summary_color) = remaining.next()?;
        let details: Vec<(String, Color)> = remaining.collect();
        let read_batch =
            matches!(&phase, ToolPhase::Observation) && tool_name.as_deref() == Some("read_file");
        let read_call =
            matches!(&phase, ToolPhase::Call) && tool_name.as_deref() == Some("read_file");
        let read_batch_paths = if read_call {
            vec![compact_read_path(&summary)]
        } else {
            Vec::new()
        };
        Some(Self {
            id: 0,
            summary,
            summary_color,
            collapsed_hint: tool_detail_hint(details.len()),
            details,
            expanded: false,
            detail_scroll: 0,
            phase,
            tool_name,
            read_batch_count: usize::from(read_batch),
            read_batch_error: read_batch
                && summary_color == super::render::role_color(super::render::Role::Error),
            read_batch_paths,
        })
    }

    fn can_merge_observation(&self, incoming: &Self) -> bool {
        matches!(self.phase, ToolPhase::Call)
            && matches!(incoming.phase, ToolPhase::Observation)
            && self.tool_name.is_some()
            && self.tool_name == incoming.tool_name
    }

    fn is_completed_read(&self) -> bool {
        self.read_batch_count > 0
            && matches!(&self.phase, ToolPhase::Observation)
            && self.tool_name.as_deref() == Some("read_file")
    }

    fn can_merge_read_batch(&self, incoming: &Self) -> bool {
        self.is_completed_read() && incoming.is_completed_read()
    }

    fn merge_observation(&mut self, mut incoming: Self) {
        let mut read_batch_paths = std::mem::take(&mut self.read_batch_paths);
        read_batch_paths.extend(incoming.read_batch_paths);
        let mut details = Vec::with_capacity(self.details.len() + incoming.details.len() + 1);
        details.push((self.summary.clone(), self.summary_color));
        details.append(&mut self.details);
        details.append(&mut incoming.details);
        self.summary = incoming.summary;
        self.summary_color = incoming.summary_color;
        self.details = details;
        self.collapsed_hint = tool_detail_hint(self.details.len());
        self.phase = ToolPhase::Observation;
        self.tool_name = incoming.tool_name;
        self.read_batch_count = incoming.read_batch_count;
        self.read_batch_error = incoming.read_batch_error;
        self.read_batch_paths = bound_read_batch_paths(read_batch_paths);
        self.detail_scroll = 0;
    }

    /// Fold one completed read into the current batch while retaining every
    /// file's complete summary/detail payload in arrival order.
    fn merge_read_batch(&mut self, mut incoming: Self) {
        let count = self
            .read_batch_count
            .saturating_add(incoming.read_batch_count);
        let has_error = self.read_batch_error || incoming.read_batch_error;
        let mut read_batch_paths = std::mem::take(&mut self.read_batch_paths);
        read_batch_paths.extend(incoming.read_batch_paths);
        let read_batch_paths = bound_read_batch_paths(read_batch_paths);
        let mut details = Vec::with_capacity(self.details.len() + incoming.details.len() + 2);
        details.push((self.summary.clone(), self.summary_color));
        details.append(&mut self.details);
        details.push((incoming.summary, incoming.summary_color));
        details.append(&mut incoming.details);
        self.details = details;
        self.collapsed_hint = tool_detail_hint(self.details.len());
        self.summary = read_batch_summary(count, &read_batch_paths, has_error);
        self.summary_color = if has_error {
            super::render::role_color(super::render::Role::Error)
        } else {
            super::render::role_color(super::render::Role::Success)
        };
        self.read_batch_count = count;
        self.read_batch_error = has_error;
        self.read_batch_paths = read_batch_paths;
        self.detail_scroll = 0;
    }

    pub(crate) fn toggle(&mut self) -> bool {
        let expanded = !self.expanded;
        self.set_expanded(expanded);
        expanded
    }

    pub(crate) fn set_expanded(&mut self, expanded: bool) -> bool {
        if self.expanded == expanded {
            return false;
        }
        self.expanded = expanded;
        if !expanded {
            self.detail_scroll = 0;
        }
        true
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

    /// Static scrollback keeps the same phase vocabulary as the live
    /// projection; this is presentation metadata, not execution state.
    pub(crate) fn phase_label(&self) -> &'static str {
        match self.phase {
            #[cfg(test)]
            ToolPhase::Standalone => "TOOL",
            ToolPhase::Call => "CALL",
            ToolPhase::Observation => "OUT",
        }
    }

    /// One-cell phase token for narrow static scrollback; it replaces the
    /// decorative glyph instead of consuming another column.
    pub(crate) fn phase_short_label(&self) -> &'static str {
        match self.phase {
            #[cfg(test)]
            ToolPhase::Standalone => "T",
            ToolPhase::Call => "C",
            ToolPhase::Observation => "O",
        }
    }

    pub(crate) fn presentation_id(&self) -> PresentationId {
        self.id
    }

    pub(crate) fn details_text(&self) -> String {
        if self.details.is_empty() {
            return if matches!(self.phase, ToolPhase::Observation) {
                "no output".to_owned()
            } else {
                String::new()
            };
        }
        self.details
            .iter()
            .map(|(text, _)| text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub(crate) fn has_details(&self) -> bool {
        !self.details.is_empty()
    }

    pub(crate) fn collapsed_hint(&self) -> &str {
        &self.collapsed_hint
    }

    pub(crate) fn details_count(&self) -> usize {
        self.details.len()
    }

    pub(crate) fn presentation_chars(&self) -> usize {
        self.summary.chars().count()
            + self
                .details
                .iter()
                .map(|(text, _)| text.chars().count())
                .sum::<usize>()
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
                self.collapsed_hint(),
                role_color(Role::Muted),
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
            )
            .with_anchor(LiveLineAnchor {
                focus: LiveBlockFocus::Tool(self.id),
                logical_line: 0,
            });
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
                        .enumerate()
                        .map(|(index, (text, color))| {
                            LiveLine::new(text.as_str(), *color, LiveLineKind::ToolDetail)
                                .with_anchor(LiveLineAnchor {
                                    focus: LiveBlockFocus::Tool(self.id),
                                    logical_line: start + index + 1,
                                })
                        }),
                    max_rows,
                );
            } else if !self.details.is_empty() {
                append_tail(
                    target,
                    std::iter::once(
                        LiveLine::new(
                            self.collapsed_hint(),
                            role_color(Role::Muted),
                            LiveLineKind::ToolDetail,
                        )
                        .with_anchor(LiveLineAnchor {
                            focus: LiveBlockFocus::Tool(self.id),
                            logical_line: 1,
                        }),
                    ),
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
        )
        .with_anchor(LiveLineAnchor {
            focus: LiveBlockFocus::Tool(self.id),
            logical_line: 0,
        });
        append_tail(target, std::iter::once(summary), max_rows);
        if !self.details.is_empty() {
            append_tail(
                target,
                std::iter::once(
                    LiveLine::new(
                        self.collapsed_hint(),
                        role_color(Role::Muted),
                        LiveLineKind::ToolDetail,
                    )
                    .with_anchor(LiveLineAnchor {
                        focus: LiveBlockFocus::Tool(self.id),
                        logical_line: 1,
                    }),
                ),
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

fn tool_detail_hint(rows: usize) -> String {
    format!("  [Ctrl+O details · {rows} rows]")
}

fn compact_read_path(summary: &str) -> String {
    let label = summary
        .find("Read ")
        .map(|index| &summary[index + "Read ".len()..])
        .unwrap_or(summary)
        .trim();
    let mut clipped = label
        .chars()
        .take(MAX_READ_BATCH_PATH_CHARS)
        .collect::<String>();
    if label.chars().count() > MAX_READ_BATCH_PATH_CHARS {
        clipped.push('…');
    }
    clipped
}

fn bound_read_batch_paths(mut paths: Vec<String>) -> Vec<String> {
    paths.truncate(MAX_READ_BATCH_PATHS);
    paths
}

fn read_batch_summary(count: usize, paths: &[String], has_error: bool) -> String {
    let marker = if has_error { '✗' } else { '✓' };
    let mut summary = format!("  {marker} Read batch · {count} files");
    let visible = paths.iter().take(3).cloned().collect::<Vec<_>>();
    if !visible.is_empty() {
        summary.push_str(" · ");
        summary.push_str(&visible.join(", "));
        if count > visible.len() {
            summary.push_str(&format!(" +{} more", count - visible.len()));
        }
    }
    summary
}

#[derive(Clone, Debug)]
struct AnswerBlock {
    id: u64,
    text: String,
    /// Complete received answer retained for the explicit audit panel. `text`
    /// remains a bounded live viewport source so streaming redraws stay cheap.
    full_text: String,
    line_index: LineIndex,
    /// Byte offsets of lines whose trimmed content starts a fenced code block.
    /// Keeping these offsets moves fence scanning out of every live redraw.
    fence_starts: Vec<usize>,
    last_line_start: usize,
    /// Conservative cache for the live renderer.  It may stay true after an
    /// old marker is trimmed; that only skips the tail fast path temporarily.
    has_markdown_syntax: bool,
}

#[derive(Clone, Debug)]
struct ReasoningBlock {
    id: u64,
    text: String,
    full_text: String,
    line_index: LineIndex,
}

#[derive(Clone, Debug)]
enum LiveBlock {
    Answer(AnswerBlock),
    Reasoning(ReasoningBlock),
    Tool(ToolBlock),
}

struct VisibleLanes<'a> {
    answers: VecDeque<LiveLine<'a>>,
    reasoning: VecDeque<LiveLine<'a>>,
    other: VecDeque<LiveLine<'a>>,
    focused_id: Option<u64>,
    last_answer_text: Option<&'a str>,
    focused_tool_expanded: bool,
    reasoning_truncated: bool,
}

/// Semantic target behind a live Inspector row.  Keeping identity separate
/// from display text lets navigation focus an older block without parsing
/// summaries or coupling the execution graph to the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LiveBlockFocus {
    Answer(u64),
    Reasoning(u64),
    Tool(u64),
}

/// Logical position of a live line.  Reflow may change its physical height,
/// but this identity remains stable while the semantic block is retained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LiveLineAnchor {
    pub(crate) focus: LiveBlockFocus,
    pub(crate) logical_line: usize,
}

/// Stable presentation row for the live-block inspector. The execution graph
/// never sees this projection; its source is complete, while each redraw only
/// lays out the visible viewport.
#[derive(Clone, Debug)]
pub(crate) struct LiveBlockEntry {
    pub(crate) key: String,
    pub(crate) detail: String,
    pub(crate) focus: LiveBlockFocus,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct LiveTranscript {
    blocks: VecDeque<LiveBlock>,
    splash: Option<String>,
    next_stream_id: u64,
    next_tool_id: u64,
    /// 当前 Inspector 选中的语义块；与工具展开焦点分离，令 Answer/Reasoning
    /// 也能被顶栏与快照明确标识，而不把面板状态泄漏进执行图。
    focused_block: Option<LiveBlockFocus>,
    focused_tool: Option<u64>,
    /// Presentation-only anchor for an Inspector-selected Answer/Reasoning
    /// block.  Follow clears it; model execution never reads it.
    audit_focus: Option<LiveBlockFocus>,
    /// 用户用 Alt+↑/↓ 选定旧工具后，暂时阻止新工具夺走焦点。
    focus_pinned: bool,
    reasoning_expanded: bool,
    /// Rows behind the newest live tail; zero keeps the default Follow view.
    inspect_offset: usize,
    /// Explicit user hold state; unlike the offset, this remains visible even
    /// when the current output is shorter than one scroll step.
    inspect_mode: bool,
    answer_chars: usize,
    reasoning_chars: usize,
    /// Presentation revision for the live-line cache.  Execution state never
    /// reads this; any stream/focus/viewport mutation advances it.
    render_revision: u64,
}

impl LiveTranscript {
    #[allow(dead_code)]
    pub(crate) fn push_answer(&mut self, text: &str) -> Vec<ToolBlock> {
        let id = self
            .current_stream_id(LiveChannel::Answer)
            .unwrap_or_else(|| self.next_stream_id());
        self.push_answer_with_id(text, id)
    }

    pub(crate) fn push_answer_with_id(&mut self, text: &str, id: PresentationId) -> Vec<ToolBlock> {
        self.touch_render();
        self.splash = None;
        let text = super::render::sanitize_display_text(text);
        if text.is_empty() {
            return Vec::new();
        }
        let starts_new_block = !matches!(self.blocks.back(), Some(LiveBlock::Answer(_)));
        self.preserve_hold_for_append(&text, starts_new_block);
        match self.blocks.back_mut() {
            Some(LiveBlock::Answer(current)) => {
                current.full_text.push_str(&text);
                append_answer_bounded(current, &mut self.answer_chars, &text)
            }
            _ => {
                // Preserve an explicit Ctrl+R choice across the Answer phase;
                // a later token must not silently collapse the user's audit
                // view.  New tasks still reset this flag in clear_streams.
                if !self.inspect_mode {
                    self.inspect_offset = 0;
                }
                self.answer_chars = 0;
                let mut current = AnswerBlock {
                    id,
                    text: String::new(),
                    full_text: String::new(),
                    line_index: LineIndex::default(),
                    fence_starts: Vec::new(),
                    last_line_start: 0,
                    has_markdown_syntax: false,
                };
                current.full_text.push_str(&text);
                append_answer_bounded(&mut current, &mut self.answer_chars, &text);
                self.blocks.push_back(LiveBlock::Answer(current));
                self.reserve_stream_id(id);
            }
        }
        self.trim_blocks()
    }

    #[allow(dead_code)]
    pub(crate) fn push_reasoning(&mut self, text: &str) -> Vec<ToolBlock> {
        let id = self
            .current_stream_id(LiveChannel::Reasoning)
            .unwrap_or_else(|| self.next_stream_id());
        self.push_reasoning_with_id(text, id)
    }

    pub(crate) fn push_reasoning_with_id(
        &mut self,
        text: &str,
        id: PresentationId,
    ) -> Vec<ToolBlock> {
        self.touch_render();
        self.splash = None;
        let text = super::render::sanitize_display_text(text);
        if text.is_empty() {
            return Vec::new();
        }
        let starts_new_block = !matches!(self.blocks.back(), Some(LiveBlock::Reasoning(_)));
        self.preserve_hold_for_append(&text, starts_new_block);
        match self.blocks.back_mut() {
            Some(LiveBlock::Reasoning(current)) => {
                current.full_text.push_str(&text);
                append_bounded(
                    &mut current.text,
                    &mut current.line_index,
                    &mut self.reasoning_chars,
                    &text,
                )
            }
            _ => {
                self.reasoning_chars = 0;
                let mut current = ReasoningBlock {
                    id,
                    text: String::new(),
                    full_text: String::new(),
                    line_index: LineIndex::default(),
                };
                current.full_text.push_str(&text);
                append_bounded(
                    &mut current.text,
                    &mut current.line_index,
                    &mut self.reasoning_chars,
                    &text,
                );
                self.blocks.push_back(LiveBlock::Reasoning(current));
                self.reserve_stream_id(id);
            }
        }
        self.trim_blocks()
    }

    #[allow(dead_code)]
    pub(crate) fn push_tool(&mut self, block: ToolBlock) -> Vec<ToolBlock> {
        let id = self.next_tool_id;
        let (evicted, _, _) = self.push_tool_with_id(block, id);
        evicted
    }

    pub(crate) fn push_tool_with_id(
        &mut self,
        mut block: ToolBlock,
        id: PresentationId,
    ) -> (Vec<ToolBlock>, PresentationId, bool) {
        self.touch_render();
        self.splash = None;
        // A new or merged tool contributes at least one presentation row. Keep
        // a held viewport behind the moving tail; the full detail layout still
        // belongs to the existing focused-tool projection.
        self.preserve_hold_for_append("", true);
        let can_merge = self.blocks.back().is_some_and(|current| {
            matches!(current, LiveBlock::Tool(current) if current.can_merge_observation(&block))
        });
        if can_merge {
            if let Some(LiveBlock::Tool(current)) = self.blocks.back_mut() {
                current.merge_observation(block);
            }
            self.coalesce_read_batch();
            let id = self
                .blocks
                .back()
                .and_then(|block| match block {
                    LiveBlock::Tool(tool) => Some(tool.id),
                    _ => None,
                })
                .unwrap_or(id);
            return (self.trim_blocks(), id, false);
        }
        let can_merge_read_batch = self.blocks.back().is_some_and(|current| {
            matches!(current, LiveBlock::Tool(current) if current.can_merge_read_batch(&block))
        });
        if can_merge_read_batch {
            if let Some(LiveBlock::Tool(current)) = self.blocks.back_mut() {
                current.merge_read_batch(block);
            }
            let id = self
                .blocks
                .back()
                .and_then(|block| match block {
                    LiveBlock::Tool(tool) => Some(tool.id),
                    _ => None,
                })
                .unwrap_or(id);
            return (self.trim_blocks(), id, false);
        }
        block.id = id;
        self.next_tool_id = self.next_tool_id.max(id.saturating_add(1));
        if !self.focus_pinned && self.audit_focus.is_none() {
            self.focused_tool = Some(block.id);
            self.focused_block = Some(LiveBlockFocus::Tool(block.id));
        }
        self.blocks.push_back(LiveBlock::Tool(block));
        (self.trim_blocks(), id, true)
    }

    /// Coalesce the just-completed read with the immediately preceding read.
    /// The newest block's id is intentionally remapped to the older block so a
    /// held/expanded Inspector target survives the presentation-only fold.
    fn coalesce_read_batch(&mut self) {
        if self.blocks.len() < 2 {
            return;
        }
        let previous_index = self.blocks.len() - 2;
        let can_merge = matches!(
            (self.blocks.get(previous_index), self.blocks.back()),
            (
                Some(LiveBlock::Tool(previous)),
                Some(LiveBlock::Tool(current))
            ) if previous.can_merge_read_batch(current)
        );
        if !can_merge {
            return;
        }

        let Some(LiveBlock::Tool(current)) = self.blocks.pop_back() else {
            return;
        };
        let Some(LiveBlock::Tool(mut previous)) = self.blocks.pop_back() else {
            self.blocks.push_back(LiveBlock::Tool(current));
            return;
        };
        let current_id = current.id;
        let previous_id = previous.id;
        previous.merge_read_batch(current);
        self.remap_tool_focus(current_id, previous_id);
        self.blocks.push_back(LiveBlock::Tool(previous));
    }

    fn remap_tool_focus(&mut self, from: u64, to: u64) {
        if self.focused_tool == Some(from) {
            self.focused_tool = Some(to);
        }
        self.focused_block = self.focused_block.map(|focus| match focus {
            LiveBlockFocus::Tool(id) if id == from => LiveBlockFocus::Tool(to),
            other => other,
        });
        self.audit_focus = self.audit_focus.map(|focus| match focus {
            LiveBlockFocus::Tool(id) if id == from => LiveBlockFocus::Tool(to),
            other => other,
        });
    }

    pub(crate) fn clear_streams(&mut self) {
        self.touch_render();
        self.blocks
            .retain(|block| matches!(block, LiveBlock::Tool(_)));
        self.answer_chars = 0;
        self.reasoning_chars = 0;
        self.reasoning_expanded = false;
        self.inspect_offset = 0;
        self.inspect_mode = false;
        self.audit_focus = None;
        self.focus_pinned = false;
        if !self.has_tools() {
            self.focused_tool = None;
            self.focused_block = None;
        } else {
            self.focused_block = self.focused_tool.map(LiveBlockFocus::Tool);
        }
        self.splash = None;
    }

    pub(crate) fn set_splash(&mut self, text: String) {
        self.touch_render();
        self.blocks.clear();
        self.answer_chars = 0;
        self.reasoning_chars = 0;
        self.reasoning_expanded = false;
        self.inspect_offset = 0;
        self.inspect_mode = false;
        self.focused_tool = None;
        self.focused_block = None;
        self.focus_pinned = false;
        self.audit_focus = None;
        self.splash = Some(super::render::sanitize_display_text(&text));
    }

    pub(crate) fn drain_tools(&mut self) -> Vec<ToolBlock> {
        self.touch_render();
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
        self.focused_block = None;
        self.focus_pinned = false;
        tools
    }

    #[allow(dead_code)]
    pub(crate) fn drain_reasoning(&mut self) -> Vec<String> {
        self.drain_reasoning_with_ids()
            .into_iter()
            .map(|(_, text)| text)
            .collect()
    }

    pub(crate) fn drain_reasoning_with_ids(&mut self) -> Vec<(PresentationId, String)> {
        self.touch_render();
        let mut retained = VecDeque::new();
        let mut reasoning = Vec::new();
        while let Some(block) = self.blocks.pop_front() {
            match block {
                LiveBlock::Reasoning(block) => reasoning.push((block.id, block.full_text)),
                other => retained.push_back(other),
            }
        }
        self.blocks = retained;
        self.reasoning_chars = 0;
        if self
            .audit_focus
            .is_some_and(|focus| matches!(focus, LiveBlockFocus::Reasoning(_)))
        {
            self.audit_focus = None;
            self.inspect_mode = false;
            self.inspect_offset = 0;
        }
        if matches!(self.focused_block, Some(LiveBlockFocus::Reasoning(_))) {
            self.focused_block = self.focused_tool.map(LiveBlockFocus::Tool);
        }
        reasoning
    }

    /// Preserve streamed Answer text when a run ends before emitting a final
    /// graph message. The caller decides how to label the partial output;
    /// execution never reads this presentation-only drain.
    #[allow(dead_code)]
    pub(crate) fn drain_answers(&mut self) -> Vec<String> {
        self.drain_answers_with_ids()
            .into_iter()
            .map(|(_, text)| text)
            .collect()
    }

    pub(crate) fn drain_answers_with_ids(&mut self) -> Vec<(PresentationId, String)> {
        self.touch_render();
        let mut retained = VecDeque::new();
        let mut answers = Vec::new();
        while let Some(block) = self.blocks.pop_front() {
            match block {
                LiveBlock::Answer(block) => answers.push((block.id, block.full_text)),
                other => retained.push_back(other),
            }
        }
        self.blocks = retained;
        self.answer_chars = 0;
        if self
            .audit_focus
            .is_some_and(|focus| matches!(focus, LiveBlockFocus::Answer(_)))
        {
            self.audit_focus = None;
            self.inspect_mode = false;
            self.inspect_offset = 0;
        }
        if matches!(self.focused_block, Some(LiveBlockFocus::Answer(_))) {
            self.focused_block = self.focused_tool.map(LiveBlockFocus::Tool);
        }
        answers
    }

    pub(crate) fn has_tools(&self) -> bool {
        self.blocks
            .iter()
            .any(|block| matches!(block, LiveBlock::Tool(_)))
    }

    pub(crate) fn current_stream_id(&self, channel: LiveChannel) -> Option<PresentationId> {
        match (channel, self.blocks.back()) {
            (LiveChannel::Answer, Some(LiveBlock::Answer(block))) => Some(block.id),
            (LiveChannel::Reasoning, Some(LiveBlock::Reasoning(block))) => Some(block.id),
            (LiveChannel::Tool, Some(LiveBlock::Tool(block))) => Some(block.id),
            _ => None,
        }
    }

    pub(crate) fn current_stream_chars(&self, channel: LiveChannel) -> Option<usize> {
        match (channel, self.blocks.back()) {
            (LiveChannel::Answer, Some(LiveBlock::Answer(block))) => {
                Some(block.text.chars().count())
            }
            (LiveChannel::Reasoning, Some(LiveBlock::Reasoning(block))) => {
                Some(block.text.chars().count())
            }
            _ => None,
        }
    }

    pub(crate) fn live_block_chars(&self, focus: LiveBlockFocus) -> Option<usize> {
        self.blocks.iter().find_map(|block| match (focus, block) {
            (LiveBlockFocus::Answer(id), LiveBlock::Answer(block)) if block.id == id => {
                Some(block.text.chars().count())
            }
            (LiveBlockFocus::Reasoning(id), LiveBlock::Reasoning(block)) if block.id == id => {
                Some(block.text.chars().count())
            }
            (LiveBlockFocus::Tool(id), LiveBlock::Tool(block)) if block.id == id => {
                Some(block.presentation_chars())
            }
            _ => None,
        })
    }

    pub(crate) fn live_tool_chars(&self, id: PresentationId) -> Option<usize> {
        self.blocks.iter().find_map(|block| match block {
            LiveBlock::Tool(tool) if tool.id == id => Some(tool.presentation_chars()),
            _ => None,
        })
    }

    fn reserve_stream_id(&mut self, id: PresentationId) {
        self.next_stream_id = self.next_stream_id.max(id.saturating_add(1));
    }

    pub(crate) fn has_reasoning(&self) -> bool {
        self.blocks
            .iter()
            .any(|block| matches!(block, LiveBlock::Reasoning(_)))
    }

    pub(crate) fn has_answer(&self) -> bool {
        self.blocks
            .iter()
            .any(|block| matches!(block, LiveBlock::Answer(_)))
    }

    /// Focus the newest block for a channel without copying its text.  This
    /// is the live audit bridge used by contextual shortcuts such as Ctrl+A.
    pub(crate) fn focus_latest(&mut self, channel: LiveChannel) -> bool {
        let focus = self
            .blocks
            .iter()
            .rev()
            .find_map(|block| match (channel, block) {
                (LiveChannel::Answer, LiveBlock::Answer(answer)) => {
                    Some(LiveBlockFocus::Answer(answer.id))
                }
                (LiveChannel::Reasoning, LiveBlock::Reasoning(reasoning)) => {
                    Some(LiveBlockFocus::Reasoning(reasoning.id))
                }
                (LiveChannel::Tool, LiveBlock::Tool(tool)) => Some(LiveBlockFocus::Tool(tool.id)),
                _ => None,
            });
        let Some(focus) = focus else {
            return false;
        };
        self.focus_live_block(focus);
        true
    }

    pub(crate) fn has_inspectable_output(&self) -> bool {
        self.blocks
            .iter()
            .any(|block| matches!(block, LiveBlock::Answer(_) | LiveBlock::Reasoning(_)))
    }

    pub(crate) fn has_history(&self) -> bool {
        !self.blocks.is_empty()
    }

    /// Snapshot the current mixed stream for the modal inspector.  Newest
    /// block appears first; detail values retain the complete received source
    /// while the live viewport may use a cheaper bounded projection.
    pub(crate) fn inspector_rows(&self) -> Vec<LiveBlockEntry> {
        self.blocks
            .iter()
            .rev()
            .enumerate()
            .map(|(index, block)| match block {
                LiveBlock::Answer(answer) => LiveBlockEntry {
                    key: format!(
                        "#{} 🤖 Answer · {} chars · p#{}",
                        index + 1,
                        answer.full_text.chars().count(),
                        answer.id
                    ),
                    detail: answer.full_text.clone(),
                    focus: LiveBlockFocus::Answer(answer.id),
                },
                LiveBlock::Reasoning(text) => LiveBlockEntry {
                    key: format!(
                        "#{} 💭 Reasoning · {} chars · p#{}",
                        index + 1,
                        text.full_text.chars().count(),
                        text.id
                    ),
                    detail: text.full_text.clone(),
                    focus: LiveBlockFocus::Reasoning(text.id),
                },
                LiveBlock::Tool(tool) => LiveBlockEntry {
                    key: format!(
                        "#{} ⚙ {} · p#{}",
                        index + 1,
                        tool.summary(),
                        tool.presentation_id()
                    ),
                    detail: tool.details_text(),
                    focus: LiveBlockFocus::Tool(tool.id),
                },
            })
            .collect()
    }

    /// Move render focus to the semantic block selected in the Inspector.
    /// This only changes the presentation projection; the running model task
    /// and pending queue remain untouched.
    pub(crate) fn focus_live_block(&mut self, focus: LiveBlockFocus) -> bool {
        let present = self.contains_focus(focus);
        if !present {
            return false;
        }
        let next_tool = match focus {
            LiveBlockFocus::Tool(id) => Some(id),
            LiveBlockFocus::Answer(_) | LiveBlockFocus::Reasoning(_) => None,
        };
        let next_pinned = matches!(focus, LiveBlockFocus::Tool(_)) && next_tool.is_some();
        let audit = matches!(
            focus,
            LiveBlockFocus::Answer(_) | LiveBlockFocus::Reasoning(_)
        );
        let before_audit = (self.audit_focus, self.inspect_mode, self.inspect_offset);
        let changed = self.focused_block != Some(focus)
            || self.focused_tool != next_tool
            || self.focus_pinned != next_pinned
            || (audit && before_audit != (Some(focus), true, 0))
            || (!audit && self.audit_focus.is_some());
        self.focused_block = Some(focus);
        self.focused_tool = next_tool;
        self.focus_pinned = next_pinned;
        if audit {
            // Selecting a semantic block is an explicit non-blocking audit:
            // keep that block visible while tokens continue arriving.
            self.audit_focus = Some(focus);
            self.inspect_mode = true;
            self.inspect_offset = 0;
        } else {
            self.audit_focus = None;
        }
        if changed {
            self.touch_render();
        }
        changed
    }

    /// Align an underlying tool's live expansion with the Inspector detail
    /// toggle.  The panel remains presentation-only, but closing it preserves
    /// the user's chosen tool view in the live viewport.
    pub(crate) fn set_tool_expanded(&mut self, id: u64, expanded: bool) -> bool {
        let changed = self
            .blocks
            .iter_mut()
            .find_map(|block| match block {
                LiveBlock::Tool(tool) if tool.id == id => Some(tool.set_expanded(expanded)),
                _ => None,
            })
            .unwrap_or(false);
        if changed {
            self.audit_focus = None;
            self.focused_tool = Some(id);
            self.focused_block = Some(LiveBlockFocus::Tool(id));
            self.focus_pinned = true;
            self.touch_render();
        }
        changed
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
        let changed = self.inspect_offset != before;
        if changed {
            self.inspect_mode = true;
            self.touch_render();
        }
        changed
    }

    pub(crate) fn scroll_live_page(&mut self, direction: i8, page_rows: usize) -> bool {
        if !self.has_inspectable_output() {
            return false;
        }
        let before = self.inspect_offset;
        let step = page_rows.saturating_sub(1).max(1);
        if direction > 0 {
            self.inspect_offset = self
                .inspect_offset
                .saturating_add(step)
                .min(MAX_LIVE_INSPECT_OFFSET);
        } else if direction < 0 {
            self.inspect_offset = self.inspect_offset.saturating_sub(step);
        }
        let changed = self.inspect_offset != before;
        if changed {
            self.inspect_mode = true;
            self.touch_render();
        }
        changed
    }

    /// Hold the live viewport without cancelling the running model task.
    pub(crate) fn hold_live(&mut self) -> bool {
        if !self.has_inspectable_output() {
            return false;
        }
        let before = (self.inspect_mode, self.inspect_offset);
        self.inspect_mode = true;
        if self.inspect_offset == 0 {
            self.inspect_offset = LIVE_SCROLL_STEP.min(MAX_LIVE_INSPECT_OFFSET);
        }
        let changed = (self.inspect_mode, self.inspect_offset) != before;
        if changed {
            self.touch_render();
        }
        changed
    }

    pub(crate) fn follow_live(&mut self) -> bool {
        let audit_focus = self.audit_focus.take();
        let clear_semantic_focus = matches!(
            self.focused_block,
            Some(LiveBlockFocus::Answer(_) | LiveBlockFocus::Reasoning(_))
        );
        let changed = audit_focus.is_some()
            || clear_semantic_focus
            || self.inspect_mode
            || self.inspect_offset != 0;
        self.inspect_mode = false;
        self.inspect_offset = 0;
        if clear_semantic_focus {
            self.focused_block = self.focused_tool.map(LiveBlockFocus::Tool);
        }
        if changed {
            self.touch_render();
        }
        changed
    }

    pub(crate) fn is_inspecting(&self) -> bool {
        self.inspect_mode || self.inspect_offset != 0
    }

    /// 顶栏焦点 chip 只读取当前已净化的工具摘要，不复制工具详情或引入新状态。
    pub(crate) fn focused_tool_summary(&self) -> Option<&str> {
        let focused = self.focused_tool?;
        self.blocks.iter().find_map(|block| match block {
            LiveBlock::Tool(tool) if tool.id == focused => Some(tool.summary()),
            _ => None,
        })
    }

    pub(crate) fn focused_block(&self) -> Option<LiveBlockFocus> {
        self.focused_block
    }

    /// 顶部 chrome 的真实通道 badge：只看最后一个 LiveBlock，不推断模型隐藏状态。
    pub(crate) fn active_channel(&self) -> Option<LiveChannel> {
        self.blocks.back().map(|block| match block {
            LiveBlock::Answer(_) => LiveChannel::Answer,
            LiveBlock::Reasoning(_) => LiveChannel::Reasoning,
            LiveBlock::Tool(_) => LiveChannel::Tool,
        })
    }

    /// Derive a compact phase trace from the existing mixed live blocks.
    /// Consecutive chunks of one channel collapse into one token; only the
    /// latest bounded transitions survive block eviction.  This is a
    /// presentation projection, never a second execution timeline.
    pub(crate) fn phase_trace(&self) -> Vec<LiveChannel> {
        let mut trace = Vec::new();
        for block in &self.blocks {
            let channel = match block {
                LiveBlock::Answer(_) => LiveChannel::Answer,
                LiveBlock::Reasoning(_) => LiveChannel::Reasoning,
                LiveBlock::Tool(_) => LiveChannel::Tool,
            };
            if trace.last().copied() != Some(channel) {
                trace.push(channel);
            }
        }
        if trace.len() > MAX_LIVE_PHASE_TRACE {
            trace.drain(..trace.len() - MAX_LIVE_PHASE_TRACE);
        }
        trace
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
        self.focused_block = Some(LiveBlockFocus::Tool(ids[next]));
        self.focus_pinned = next + 1 < ids.len();
        if changed {
            self.touch_render();
        }
        changed
    }

    /// Move the held live projection across Answer/Reasoning/Tool blocks.
    /// This reuses `focused_block`; it does not add a second focus state.
    pub(crate) fn move_semantic_focus(&mut self, delta: i8) -> bool {
        let focuses = self
            .inspector_rows()
            .into_iter()
            .map(|entry| entry.focus)
            .collect::<Vec<_>>();
        if focuses.is_empty() {
            return false;
        }
        let current = self
            .focused_block
            .and_then(|focus| focuses.iter().position(|candidate| *candidate == focus));
        let next = match (current, delta < 0) {
            (Some(index), true) => index.saturating_sub(1),
            (Some(index), false) => index.saturating_add(1).min(focuses.len() - 1),
            (None, true) => focuses.len() - 1,
            (None, false) => 0,
        };
        self.focus_live_block(focuses[next])
    }

    pub(crate) fn scroll_tool_details(&mut self, delta: i8) -> bool {
        let Some(focused_id) = self.focused_tool else {
            return false;
        };
        let changed = self
            .blocks
            .iter_mut()
            .find_map(|block| match block {
                LiveBlock::Tool(tool) if tool.id == focused_id => Some(tool.scroll_details(delta)),
                _ => None,
            })
            .unwrap_or(false);
        if changed {
            self.touch_render();
        }
        changed
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
        let mut changed = false;
        let mut expanded = false;
        for block in &mut self.blocks {
            if let LiveBlock::Tool(tool) = block {
                if tool.id == target {
                    self.focused_tool = Some(target);
                    expanded = tool.toggle();
                    changed = true;
                    break;
                }
            }
        }
        if changed {
            self.touch_render();
        }
        expanded
    }

    pub(crate) fn toggle_reasoning(&mut self) -> bool {
        if !self.has_reasoning() {
            return false;
        }
        self.reasoning_expanded = !self.reasoning_expanded;
        self.touch_render();
        self.reasoning_expanded
    }

    /// Activate the currently inspected semantic block without creating a
    /// second expansion state or touching execution data.
    pub(crate) fn toggle_focused_semantic(&mut self) -> bool {
        match self.focused_block {
            Some(LiveBlockFocus::Tool(_)) => {
                self.toggle_details();
                true
            }
            Some(LiveBlockFocus::Reasoning(_)) => self.toggle_reasoning() || self.has_reasoning(),
            Some(LiveBlockFocus::Answer(_)) | None if self.has_tools() => {
                self.toggle_details();
                true
            }
            Some(LiveBlockFocus::Answer(_)) | None => {
                self.toggle_reasoning() || self.has_reasoning()
            }
        }
    }

    pub(crate) fn is_reasoning_expanded(&self) -> bool {
        self.reasoning_expanded
    }

    pub(crate) fn render_revision(&self) -> u64 {
        self.render_revision
    }

    #[allow(dead_code)]
    fn next_stream_id(&mut self) -> u64 {
        let id = self.next_stream_id;
        self.next_stream_id = self.next_stream_id.wrapping_add(1);
        id
    }

    fn contains_focus(&self, focus: LiveBlockFocus) -> bool {
        self.blocks.iter().any(|block| match (focus, block) {
            (LiveBlockFocus::Answer(id), LiveBlock::Answer(answer)) => answer.id == id,
            (LiveBlockFocus::Reasoning(id), LiveBlock::Reasoning(reasoning)) => reasoning.id == id,
            (LiveBlockFocus::Tool(id), LiveBlock::Tool(tool)) => tool.id == id,
            _ => false,
        })
    }

    fn touch_render(&mut self) {
        self.render_revision = self.render_revision.wrapping_add(1);
    }

    /// Keep an explicit live hold stable while the producer appends rows.
    /// `inspect_offset` is measured in bounded logical rows, so advancing it
    /// by the appended newline count prevents the held view drifting toward
    /// the tail on every streamed token. Inspector focus has its own semantic
    /// projection and must not be shifted here.
    fn preserve_hold_for_append(&mut self, text: &str, starts_new_block: bool) {
        if !self.inspect_mode || self.audit_focus.is_some() {
            return;
        }
        let added_rows = text.matches('\n').count() + usize::from(starts_new_block);
        if added_rows > 0 {
            self.inspect_offset = self
                .inspect_offset
                .saturating_add(added_rows)
                .min(MAX_LIVE_INSPECT_OFFSET);
        }
    }

    fn audit_lines<'a>(
        &'a self,
        focus: LiveBlockFocus,
        max_rows: usize,
    ) -> Option<(VecDeque<LiveLine<'a>>, bool)> {
        let mut lines = VecDeque::with_capacity(max_rows);
        let truncated = match focus {
            LiveBlockFocus::Answer(id) => {
                let LiveBlock::Answer(answer) = self
                    .blocks
                    .iter()
                    .find(|block| matches!(block, LiveBlock::Answer(answer) if answer.id == id))?
                else {
                    return None;
                };
                append_answer_tail(
                    &mut lines,
                    answer,
                    role_color(Role::Answer),
                    Some("🤖 "),
                    false,
                    max_rows,
                )
            }
            LiveBlockFocus::Reasoning(id) => {
                let LiveBlock::Reasoning(reasoning) = self.blocks.iter().find(
                    |block| matches!(block, LiveBlock::Reasoning(reasoning) if reasoning.id == id),
                )?
                else {
                    return None;
                };
                append_text_tail(
                    &mut lines,
                    &reasoning.text,
                    &reasoning.line_index,
                    role_color(Role::Reasoning),
                    Some("💭 "),
                    LiveBlockFocus::Reasoning(reasoning.id),
                    max_rows,
                )
            }
            LiveBlockFocus::Tool(_) => return None,
        };
        Some((lines, truncated))
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
                text_lines(splash, role_color(Role::Answer), LiveLineKind::Splash),
                max_rows,
            );
        }

        // Inspector selection can pin one historical Answer/Reasoning block
        // without stopping the producer.  Render that bounded block directly;
        // normal Follow projection below remains unchanged after Alt+End.
        if let Some(visible) = self.audit_visible_lines(max_rows, requested_rows, inspect_offset) {
            return visible;
        }

        // Keep only rows that can reach this frame.  A long-running task may
        // retain 64 blocks, but the viewport is bounded; materializing every
        // collapsed/expanded tool row on every spinner frame needlessly turns
        // redraw cost into O(blocks × detail).
        let lanes = self.collect_visible_lanes(max_rows);

        // Default view keeps Answer readable while reserving an adaptive reasoning
        // preview. Ctrl+R opts into an inspection view: reasoning gets the
        // remaining rows, while Answer keeps one row and a focused tool keeps its
        // summary.
        self.compose_visible_lanes(lanes, max_rows, requested_rows, inspect_offset)
    }

    fn compose_visible_lanes<'a>(
        &'a self,
        lanes: VisibleLanes<'a>,
        max_rows: usize,
        requested_rows: usize,
        inspect_offset: usize,
    ) -> Vec<LiveLine<'a>> {
        let VisibleLanes {
            answers,
            reasoning,
            other,
            focused_id,
            last_answer_text,
            focused_tool_expanded,
            mut reasoning_truncated,
            ..
        } = lanes;
        let reasoning_preview_rows = default_reasoning_preview_rows(max_rows);
        let reserve_reasoning = should_reserve_reasoning(
            self.reasoning_expanded,
            &answers,
            focused_tool_expanded,
            !reasoning.is_empty(),
            focused_id.is_some(),
            max_rows,
            reasoning_preview_rows,
        );
        let answer_budget = visible_answer_budget(
            self.reasoning_expanded,
            !answers.is_empty(),
            focused_id.is_some(),
            max_rows,
            reserve_reasoning,
            reasoning_preview_rows,
        );
        let answers = pin_answer_header(
            into_tail(answers, answer_budget),
            last_answer_text,
            answer_budget,
        );
        let focused = self.focused_tool_lines(
            focused_id,
            max_rows,
            answers.len(),
            !reasoning.is_empty(),
            reserve_reasoning,
            reasoning_preview_rows,
        );
        let reasoning_rows = reasoning.len();
        let mut visible = merge_visible_lanes(VisibleLaneMerge {
            reasoning_expanded: self.reasoning_expanded,
            reserve_reasoning,
            reasoning,
            other,
            focused,
            answers,
            max_rows,
            reasoning_preview_rows,
        });
        if apply_inspect_offset(&mut visible, requested_rows, inspect_offset) {
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

    fn focused_tool_lines<'a>(
        &'a self,
        focused_id: Option<u64>,
        max_rows: usize,
        answer_rows: usize,
        has_reasoning: bool,
        reserve_reasoning: bool,
        reasoning_preview_rows: usize,
    ) -> VecDeque<LiveLine<'a>> {
        let mut focused = VecDeque::with_capacity(max_rows);
        let Some(focused_id) = focused_id else {
            return focused;
        };
        let Some(LiveBlock::Tool(tool)) = self
            .blocks
            .iter()
            .find(|block| matches!(block, LiveBlock::Tool(tool) if tool.id == focused_id))
        else {
            return focused;
        };
        let budget = focused_tool_budget(
            self.reasoning_expanded,
            tool.expanded,
            max_rows,
            answer_rows,
            has_reasoning,
            reserve_reasoning,
            reasoning_preview_rows,
        );
        tool.append_live_tail(&mut focused, budget, true);
        focused
    }

    fn audit_visible_lines<'a>(
        &'a self,
        max_rows: usize,
        requested_rows: usize,
        inspect_offset: usize,
    ) -> Option<Vec<LiveLine<'a>>> {
        let focus = self.audit_focus?;
        let (lines, mut truncated) = self.audit_lines(focus, max_rows)?;
        let mut visible = into_tail(lines, max_rows);
        if apply_inspect_offset(&mut visible, requested_rows, inspect_offset) {
            truncated = true;
        }
        mark_reasoning_continuation(&mut visible, truncated);
        ensure_marker(&mut visible, LiveLineKind::Reasoning, "💭 ");
        ensure_marker(&mut visible, LiveLineKind::Answer, "🤖 ");
        Some(visible)
    }

    fn collect_visible_lanes<'a>(&'a self, max_rows: usize) -> VisibleLanes<'a> {
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
                        role_color(Role::Answer),
                        Some("🤖 "),
                        answer_fence,
                        max_rows,
                    );
                }
                LiveBlock::Reasoning(reasoning_block) => {
                    let target = if has_answer {
                        &mut reasoning
                    } else {
                        &mut other
                    };
                    reasoning_truncated |= append_text_tail(
                        target,
                        &reasoning_block.text,
                        &reasoning_block.line_index,
                        role_color(Role::Reasoning),
                        Some("💭 "),
                        LiveBlockFocus::Reasoning(reasoning_block.id),
                        max_rows,
                    );
                }
                LiveBlock::Tool(tool) if focused_id != Some(tool.id) => {
                    tool.append_live_tail(&mut other, max_rows, false);
                }
                LiveBlock::Tool(_) => {}
            }
        }
        VisibleLanes {
            answers,
            reasoning,
            other,
            focused_id,
            last_answer_text,
            focused_tool_expanded,
            reasoning_truncated,
        }
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
            self.focused_block = self.focused_tool.map(LiveBlockFocus::Tool);
            self.focus_pinned = false;
        }
        if let Some(focus) = self.focused_block {
            let still_present = match focus {
                LiveBlockFocus::Answer(id) => self
                    .blocks
                    .iter()
                    .any(|block| matches!(block, LiveBlock::Answer(answer) if answer.id == id)),
                LiveBlockFocus::Reasoning(id) => self.blocks.iter().any(
                    |block| matches!(block, LiveBlock::Reasoning(reasoning) if reasoning.id == id),
                ),
                LiveBlockFocus::Tool(id) => self
                    .blocks
                    .iter()
                    .any(|block| matches!(block, LiveBlock::Tool(tool) if tool.id == id)),
            };
            if !still_present {
                self.focused_block = self.focused_tool.map(LiveBlockFocus::Tool);
            }
        }
        if self
            .audit_focus
            .is_some_and(|focus| !self.contains_focus(focus))
        {
            self.audit_focus = None;
            self.inspect_mode = false;
            self.inspect_offset = 0;
        }
        evicted
    }
}

fn append_bounded(
    target: &mut String,
    line_index: &mut LineIndex,
    char_count: &mut usize,
    text: &str,
) {
    let old_len = target.len();
    target.push_str(text);
    line_index.append(old_len, text);
    *char_count += text.chars().count();
    if *char_count > MAX_LIVE_TEXT_CHARS {
        let skip = *char_count - MAX_LIVE_TEXT_CHARS;
        let start = target
            .char_indices()
            .nth(skip)
            .map(|(index, _)| index)
            .unwrap_or(0);
        target.drain(..start);
        line_index.trim_prefix(start);
        *char_count = MAX_LIVE_TEXT_CHARS;
    }
}

fn append_answer_bounded(target: &mut AnswerBlock, char_count: &mut usize, text: &str) {
    let old_len = target.text.len();
    let old_last_line_start = target.last_line_start;
    target.text.push_str(text);
    target.line_index.append(old_len, text);
    target.has_markdown_syntax |= contains_markdown_syntax(text);
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
        target.line_index.trim_prefix(start);
        target.fence_starts.clear();
        *char_count = MAX_LIVE_TEXT_CHARS;
        rebuild_fence_starts(target);
        target.has_markdown_syntax = contains_markdown_syntax(&target.text);
    } else {
        target
            .fence_starts
            .retain(|&start| start < old_last_line_start);
        append_fence_starts(target, old_last_line_start, old_len);
        target.last_line_start = target.line_index.last_start();
    }
}

fn contains_markdown_syntax(text: &str) -> bool {
    text.bytes()
        .any(|byte| matches!(byte, b'`' | b'*' | b'_' | b'[' | b']' | b'#' | b'<' | b'>'))
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
    line_index: &LineIndex,
    color: Color,
    marker: Option<&'static str>,
    focus: LiveBlockFocus,
    max_rows: usize,
) -> bool {
    if max_rows == 0 {
        return false;
    }
    let ranges = line_index.tail_ranges(text, max_rows);
    let text_truncated = line_index.line_count() > ranges.len();
    let mut tail = VecDeque::with_capacity(ranges.len());
    let first_line = line_index.line_count().saturating_sub(ranges.len());
    for (index, (start, end)) in ranges.into_iter().enumerate() {
        let mut line = LiveLine::new(&text[start..end], color, LiveLineKind::Reasoning);
        line.anchor = Some(LiveLineAnchor {
            focus,
            logical_line: first_line + index,
        });
        tail.push_back(line);
    }
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
    let ranges = answer_tail_ranges(answer, max_rows);
    let first_line = answer.line_index.line_count().saturating_sub(ranges.len());
    for (index, (start, end)) in ranges.into_iter().enumerate() {
        let line = &answer.text[start..end];
        let fence_count = answer
            .fence_starts
            .partition_point(|&fence_start| fence_start < start);
        let mut rendered = LiveLine::new(line, color, LiveLineKind::Answer);
        rendered.anchor = Some(LiveLineAnchor {
            focus: LiveBlockFocus::Answer(answer.id),
            logical_line: first_line + index,
        });
        rendered.answer_plain = !answer.has_markdown_syntax;
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

fn answer_tail_ranges(answer: &AnswerBlock, max_rows: usize) -> Vec<(usize, usize)> {
    if max_rows == 0 || answer.text.is_empty() {
        return Vec::new();
    }
    // `last_line_start == 0` is maintained by append/rebuild and means the
    // bounded Answer has no newline.  Avoid walking a 32K token tail just to
    // discover that it is one logical line.
    if answer.last_line_start == 0 {
        return vec![(0, answer.text.len())];
    }
    answer.line_index.tail_ranges(&answer.text, max_rows)
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

    let answer_plain = answers.first().is_some_and(|line| line.answer_plain);
    let answer_anchor = answers.first().and_then(|line| line.anchor);
    // The pinned header is a real logical line zero; it must not inherit the
    // tail row's anchor or a width change would resolve every held Answer view
    // back to the header.
    let header_anchor = answer_anchor.map(|anchor| LiveLineAnchor {
        focus: anchor.focus,
        logical_line: 0,
    });
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
    let mut first = LiveLine::new(header, role_color(Role::Answer), LiveLineKind::Answer);
    first.answer_plain = answer_plain;
    first.fence_before = header_fence_before;
    first.marker = Some("🤖 ");
    first.anchor = header_anchor;
    anchored.push(first);
    let mut continuation = LiveLine::new(
        "  … answer continues",
        role_color(Role::Muted),
        LiveLineKind::Answer,
    );
    continuation.fence_before = if header.trim_start().starts_with("```") {
        !header_fence_before
    } else {
        header_fence_before
    };
    continuation.anchor = answer_anchor;
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

fn apply_inspect_offset(
    lines: &mut Vec<LiveLine<'_>>,
    requested_rows: usize,
    offset: usize,
) -> bool {
    let effective_offset = offset.min(lines.len().saturating_sub(requested_rows));
    if effective_offset == 0 {
        return false;
    }
    let end = lines.len().saturating_sub(effective_offset);
    let start = end.saturating_sub(requested_rows);
    *lines = lines
        .drain(..)
        .skip(start)
        .take(end.saturating_sub(start))
        .collect();
    true
}

fn should_reserve_reasoning(
    reasoning_expanded: bool,
    answers: &VecDeque<LiveLine<'_>>,
    focused_tool_expanded: bool,
    has_reasoning: bool,
    has_focus: bool,
    max_rows: usize,
    preview_rows: usize,
) -> bool {
    !reasoning_expanded
        && !answers.is_empty()
        && !focused_tool_expanded
        && has_reasoning
        && max_rows > preview_rows + usize::from(has_focus)
}

fn visible_answer_budget(
    reasoning_expanded: bool,
    has_answers: bool,
    has_focus: bool,
    max_rows: usize,
    reserve_reasoning: bool,
    reasoning_preview_rows: usize,
) -> usize {
    if reasoning_expanded {
        let focus_reservation = usize::from(has_focus && max_rows > 1);
        usize::from(has_answers).min(max_rows.saturating_sub(focus_reservation))
    } else {
        let reserved = usize::from(has_focus)
            + if reserve_reasoning {
                reasoning_preview_rows
            } else {
                0
            };
        max_rows.saturating_sub(reserved)
    }
}

fn focused_tool_budget(
    reasoning_expanded: bool,
    tool_expanded: bool,
    max_rows: usize,
    answer_rows: usize,
    has_reasoning: bool,
    reserve_reasoning: bool,
    reasoning_preview_rows: usize,
) -> usize {
    if reasoning_expanded && tool_expanded {
        let available = max_rows.saturating_sub(answer_rows);
        let reasoning_floor = usize::from(has_reasoning);
        if available <= 1 {
            available
        } else {
            available.saturating_sub(reasoning_floor).max(1)
        }
    } else if reasoning_expanded {
        1
    } else {
        max_rows
            .saturating_sub(answer_rows)
            .saturating_sub(if reserve_reasoning {
                reasoning_preview_rows
            } else {
                0
            })
            .max(1)
    }
}

struct VisibleLaneMerge<'a> {
    reasoning_expanded: bool,
    reserve_reasoning: bool,
    reasoning: VecDeque<LiveLine<'a>>,
    other: VecDeque<LiveLine<'a>>,
    focused: VecDeque<LiveLine<'a>>,
    answers: Vec<LiveLine<'a>>,
    max_rows: usize,
    reasoning_preview_rows: usize,
}

fn merge_visible_lanes(input: VisibleLaneMerge<'_>) -> Vec<LiveLine<'_>> {
    let VisibleLaneMerge {
        reasoning_expanded,
        reserve_reasoning,
        reasoning,
        other,
        focused,
        answers,
        max_rows,
        reasoning_preview_rows,
    } = input;
    let focus_rows = focused.len();
    let remaining = max_rows.saturating_sub(answers.len() + focus_rows);
    let reasoning = if reasoning_expanded && !reasoning.is_empty() {
        into_tail(reasoning, remaining)
    } else if reserve_reasoning {
        let reasoning = into_tail(reasoning, remaining.min(reasoning_preview_rows));
        let mut visible = into_tail(other, remaining.saturating_sub(reasoning.len()));
        visible.extend(reasoning);
        visible
    } else {
        let mut combined = reasoning;
        combined.extend(other);
        into_tail(combined, remaining)
    };
    let mut visible = reasoning;
    visible.extend(focused);
    visible.extend(answers);
    visible
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
            .any(|line| line.text.contains("[Ctrl+O details")));
        assert!(tool.toggle());
        let expanded = tool.live_lines();
        assert!(expanded.iter().any(|line| line.text == "- old"));
        assert!(expanded.iter().any(|line| line.text == "+ new"));
    }

    #[test]
    fn inspector_rows_are_newest_first_and_bounded() {
        let mut transcript = LiveTranscript::default();
        transcript.push_reasoning("thinking");
        transcript.push_tool(
            ToolBlock::from_lines(vec![
                ("read_file".into(), Color::Cyan),
                ("detail".into(), Color::Gray),
            ])
            .expect("tool"),
        );
        transcript.push_answer("answer");

        let rows = transcript.inspector_rows();
        assert_eq!(rows.len(), 3);
        assert!(rows[0].key.contains("Answer"));
        assert!(rows[1].key.contains("read_file"));
        assert!(rows[2].key.contains("Reasoning"));
        assert_eq!(rows[1].detail, "detail");

        let long = "x".repeat(8_292);
        let mut long_transcript = LiveTranscript::default();
        long_transcript.push_answer(&long);
        let detail = &long_transcript.inspector_rows()[0].detail;
        assert_eq!(detail, &long);
    }

    #[test]
    fn inspector_rows_keep_semantic_tool_identity_for_focus() {
        let mut transcript = LiveTranscript::default();
        transcript.push_tool(
            ToolBlock::from_lines(vec![
                ("read_file".into(), Color::Cyan),
                ("file contents".into(), Color::Gray),
            ])
            .expect("tool"),
        );
        transcript.push_answer("answer");

        let rows = transcript.inspector_rows();
        let LiveBlockFocus::Tool(id) = rows[1].focus else {
            panic!("tool row must retain tool identity");
        };
        assert!(transcript.focus_live_block(LiveBlockFocus::Tool(id)));
        assert_eq!(transcript.focused_tool, Some(id));
        assert!(transcript.focus_pinned);
        assert!(transcript.set_tool_expanded(id, true));
        assert!(transcript
            .visible_lines(4)
            .iter()
            .any(|line| line.text == "file contents"));
    }

    #[test]
    fn inspector_focus_tracks_answer_and_reasoning_semantics() {
        let mut transcript = LiveTranscript::default();
        transcript.push_reasoning("inspect plan");
        transcript.push_answer("final answer");

        let answer_id = transcript
            .inspector_rows()
            .iter()
            .find_map(|row| match row.focus {
                LiveBlockFocus::Answer(id) => Some(id),
                _ => None,
            })
            .expect("answer focus");
        assert!(transcript.focus_live_block(LiveBlockFocus::Answer(answer_id)));
        assert_eq!(
            transcript.focused_block(),
            Some(LiveBlockFocus::Answer(answer_id))
        );
        assert_eq!(transcript.focused_tool, None);

        let reasoning_id = transcript
            .inspector_rows()
            .iter()
            .find_map(|row| match row.focus {
                LiveBlockFocus::Reasoning(id) => Some(id),
                _ => None,
            })
            .expect("reasoning focus");
        assert!(transcript.focus_live_block(LiveBlockFocus::Reasoning(reasoning_id)));
        assert_eq!(
            transcript.focused_block(),
            Some(LiveBlockFocus::Reasoning(reasoning_id))
        );
        assert!(!transcript.focus_live_block(LiveBlockFocus::Tool(999)));
    }

    #[test]
    fn phase_trace_deduplicates_and_preserves_order() {
        let mut transcript = LiveTranscript::default();
        transcript.push_reasoning("plan");
        transcript.push_reasoning(" more plan");
        transcript.push_answer("answer");
        transcript
            .push_tool(ToolBlock::from_lines(vec![("search".into(), Color::Cyan)]).expect("tool"));
        transcript
            .push_tool(ToolBlock::from_lines(vec![("read".into(), Color::Cyan)]).expect("tool"));
        transcript.push_answer("follow-up");

        assert_eq!(
            transcript.phase_trace(),
            vec![
                LiveChannel::Reasoning,
                LiveChannel::Answer,
                LiveChannel::Tool,
                LiveChannel::Answer,
            ]
        );
    }

    #[test]
    fn inspector_audit_focus_projects_selected_block_until_follow() {
        let mut transcript = LiveTranscript::default();
        transcript.push_reasoning("first plan");
        transcript.push_answer("first answer");
        transcript.push_tool(
            ToolBlock::from_lines(vec![("tool boundary".into(), Color::Cyan)])
                .expect("tool boundary"),
        );
        transcript.push_reasoning("latest plan");
        transcript.push_answer("latest answer");

        let first_answer = transcript
            .inspector_rows()
            .iter()
            .find(|row| row.detail == "first answer")
            .map(|row| row.focus)
            .expect("first answer row");
        assert!(transcript.focus_live_block(first_answer));
        assert!(transcript.is_inspecting());
        let audited = transcript.visible_lines(4);
        assert!(audited.iter().any(|line| line.text == "first answer"));
        assert!(!audited.iter().any(|line| line.text == "latest answer"));

        let first_reasoning = transcript
            .inspector_rows()
            .iter()
            .find(|row| row.detail == "first plan")
            .map(|row| row.focus)
            .expect("first reasoning row");
        assert!(transcript.focus_live_block(first_reasoning));
        let audited_reasoning = transcript.visible_lines(4);
        assert!(audited_reasoning
            .iter()
            .any(|line| line.text == "first plan"));
        assert!(!audited_reasoning
            .iter()
            .any(|line| line.text == "latest plan"));

        assert!(transcript.follow_live());
        assert!(!transcript.is_inspecting());
        assert!(transcript
            .visible_lines(4)
            .iter()
            .any(|line| line.text == "latest answer"));
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
            .any(|line| line.text.contains("[Ctrl+O details")));
    }

    #[test]
    fn answer_keeps_adaptive_reasoning_preview_visible() {
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
            default_reasoning_preview_rows(3)
        );
        assert!(lines.iter().any(|line| line.text == "r2"));
        assert!(lines.iter().any(|line| line.text == "a1"));
        assert!(lines.iter().any(|line| line.text == "a2"));
        assert!(lines.iter().any(|line| line.marker == Some("💭 ")));
        assert!(lines.iter().any(|line| line.marker == Some("🤖 ")));
    }

    #[test]
    fn answer_preview_uses_extra_height_for_more_reasoning() {
        let mut transcript = LiveTranscript::default();
        transcript.push_reasoning("r0\nr1\nr2\nr3\nr4");
        transcript.push_answer("answer");

        let lines = transcript.visible_lines(12);
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.kind == LiveLineKind::Reasoning)
                .count(),
            MAX_LIVE_REASONING_PREVIEW_ROWS
        );
        assert!(lines.iter().any(|line| line.text == "r2"));
        assert_eq!(lines.last().map(|line| line.text), Some("answer"));
    }

    #[test]
    fn answer_arrival_preserves_explicit_reasoning_inspection() {
        let mut transcript = LiveTranscript::default();
        transcript.push_reasoning("r0\nr1\nr2");
        assert!(transcript.toggle_reasoning());
        transcript.push_answer("answer");

        assert!(transcript.is_reasoning_expanded());
        let lines = transcript.visible_lines(4);
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.kind == LiveLineKind::Reasoning)
                .count(),
            3
        );
        assert_eq!(lines.last().map(|line| line.text), Some("answer"));
        assert!(lines.iter().any(|line| line.text == "r0"));
        assert!(!transcript.toggle_reasoning());
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
        let mut index = LineIndex::default();
        index.append(0, &text);
        let ranges = index.tail_ranges(&text, 3);
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
    fn incremental_line_index_tracks_utf8_appends_and_trimmed_prefix() {
        let chunks = ["头\n中", "\n尾"];
        let mut text = String::new();
        let mut index = LineIndex::default();
        for chunk in chunks {
            let base = text.len();
            text.push_str(chunk);
            index.append(base, chunk);
        }
        let ranges = index.tail_ranges(&text, 8);
        assert_eq!(
            ranges
                .iter()
                .map(|&(start, end)| &text[start..end])
                .collect::<Vec<_>>(),
            vec!["头", "中", "尾"]
        );

        let prefix = "头\n".len();
        text.drain(..prefix);
        index.trim_prefix(prefix);
        let ranges = index.tail_ranges(&text, 8);
        assert_eq!(
            ranges
                .iter()
                .map(|&(start, end)| &text[start..end])
                .collect::<Vec<_>>(),
            vec!["中", "尾"]
        );
    }

    #[test]
    fn unbroken_answer_uses_cached_single_line_tail_metadata() {
        let text = "x".repeat(MAX_LIVE_TEXT_CHARS);
        let mut line_index = LineIndex::default();
        line_index.append(0, &text);
        let answer = AnswerBlock {
            id: 0,
            text: text.clone(),
            full_text: text.clone(),
            line_index,
            fence_starts: Vec::new(),
            last_line_start: 0,
            has_markdown_syntax: false,
        };
        assert_eq!(answer_tail_ranges(&answer, 8), vec![(0, text.len())]);

        let mut transcript = LiveTranscript::default();
        transcript.push_answer(&text);
        let visible = transcript.visible_lines(8);
        assert!(visible.iter().all(|line| line.answer_plain));
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
    fn semantic_focus_moves_across_reasoning_answer_and_tool_blocks() {
        let mut transcript = LiveTranscript::default();
        transcript.push_reasoning("think");
        transcript.push_answer("answer");
        transcript
            .push_tool(ToolBlock::from_lines(vec![("tool".into(), Color::Cyan)]).expect("tool"));
        let focuses = transcript
            .inspector_rows()
            .into_iter()
            .map(|entry| entry.focus)
            .collect::<Vec<_>>();
        assert_eq!(focuses.len(), 3);

        assert!(transcript.focus_live_block(focuses[0]));
        assert!(transcript.move_semantic_focus(1));
        assert_eq!(transcript.focused_block(), Some(focuses[1]));
        assert!(transcript.move_semantic_focus(1));
        assert_eq!(transcript.focused_block(), Some(focuses[2]));
        assert!(!transcript.move_semantic_focus(1));
        assert!(transcript.move_semantic_focus(-1));
        assert_eq!(transcript.focused_block(), Some(focuses[1]));
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

        let lines: Vec<_> = (0..40)
            .map(|index| (format!("detail {index}"), Color::Gray))
            .collect();
        let mut tool = ToolBlock::from_lines(
            std::iter::once(("tool".to_owned(), Color::Cyan))
                .chain(lines)
                .collect(),
        )
        .expect("tool");
        tool.toggle();
        assert_eq!(tool.live_lines().len(), 41);
        assert!(tool
            .live_lines()
            .iter()
            .any(|line| line.text == "detail 39"));
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
