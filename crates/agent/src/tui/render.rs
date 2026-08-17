use std::borrow::Cow;

use agent::Todo;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use super::presentation::PresentationMetrics;
use super::{fmt_reasoning_meta, wrap_live_spans_greedy, ActivityKind, ToolBlock};
use unicode_segmentation::UnicodeSegmentation;

/// 要不要画这一帧:有状态变更(dirty)或显式动画需求才画;业务 busy 不直接拥有渲染决策。
pub(crate) fn should_draw(dirty: bool, animation_due: bool) -> bool {
    dirty || animation_due
}

/// 单字符终端单元格宽度(wcwidth 口径):CJK/emoji=2、控制/零宽=0、常规=1(iter-30)。
pub(crate) fn char_cells(c: char) -> usize {
    unicode_width::UnicodeWidthChar::width(c).unwrap_or(0)
}

/// 字符串显示单元格宽度(iter-30):替代 `.chars().count()`,CJK/emoji 按实占计。
pub(crate) fn str_cells(s: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(s)
}

/// 将实时单行裁到终端 cell 宽度，保留省略号；静态 scrollback 仍走完整折行。
/// Live 区每帧只处理可见尾部，避免宽度溢出触发不可控的 Paragraph 换行。
pub(crate) fn clip_display_cells(text: &str, width: u16) -> String {
    let width = width as usize;
    if width == 0 {
        return String::new();
    }
    if str_cells(text) <= width {
        return text.to_owned();
    }
    if width == 1 {
        return "…".to_owned();
    }
    let limit = width - 1;
    let mut out = String::new();
    let mut cells = 0;
    for grapheme in text.graphemes(true) {
        if matches!(grapheme, "\n" | "\r") {
            break;
        }
        let used = str_cells(grapheme);
        if cells + used > limit {
            break;
        }
        out.push_str(grapheme);
        cells += used;
    }
    out.push('…');
    out
}

/// Return a bounded tail for a single logical live line. Reverse UTF-8
/// traversal avoids scanning/materializing an unbroken 32K token line before
/// the viewport's small physical-row budget is known.
pub(crate) fn tail_display_cells(text: &str, width: u16, max_rows: usize) -> String {
    let width = width.max(1) as usize;
    let limit = width.saturating_mul(max_rows.max(1));
    let exact_check_limit = limit.saturating_mul(4);
    if text.len() <= exact_check_limit && str_cells(text) <= limit {
        return text.to_owned();
    }
    if limit == 1 {
        return "…".to_owned();
    }
    let body_limit = limit - 1;
    let mut reversed = String::new();
    let mut cells = 0usize;
    let mut tail = Vec::new();
    for grapheme in text.graphemes(true).rev() {
        let used = str_cells(grapheme);
        if cells.saturating_add(used) > body_limit {
            break;
        }
        tail.push(grapheme);
        cells = cells.saturating_add(used);
    }
    for grapheme in tail.into_iter().rev() {
        reversed.push_str(grapheme);
    }
    let mut result = String::with_capacity(reversed.len() + 1);
    result.push('…');
    result.push_str(&reversed);
    result
}

/// 折行行数(iter-26 抽取,`input_height`/`commit_height` 共用):按**显示单元格宽**折行
/// (iter-30 起用 wcwidth 口径,CJK 实占 2 格,不再低估行数致边框撕裂)。
pub(crate) fn wrapped_rows(content: &str, width: u16) -> usize {
    let w = width.max(1) as usize;
    content.split('\n').map(|l| line_visual_rows(l, w)).sum()
}

/// 一条逻辑行按显示单元格宽做**贪心字符折行**占的可视行数(≥1)。与 [`wrap_input`] 同口径 ——
/// 宽字符(CJK 占 2 格)不整除宽度时也精确(旧 `div_ceil` 会低估致边框/光标错位)。
pub(crate) fn line_visual_rows(line: &str, w: usize) -> usize {
    let mut rows = 1usize;
    let mut cells = 0usize;
    for grapheme in line.graphemes(true) {
        let cw = str_cells(grapheme);
        if cells + cw > w && cells > 0 {
            rows += 1;
            cells = 0;
        }
        cells += cw;
    }
    rows
}

/// 输入框字符折行(按显示单元格宽,含显式 `\n`)+ 光标可视 (row, col)。**渲染与光标共用同一折行** ——
/// 修「文字换到第二行时光标仍卡在第一行末」根因(此前渲染走 ratatui 词折行、光标只按 `\n` 算行,两者不一致)。
pub(crate) fn wrap_input(buffer: &str, cursor: usize, width: u16) -> (Vec<String>, u16, u16) {
    let w = width.max(1) as usize;
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut cells = 0usize;
    let (mut crow, mut ccol) = (0u16, 0u16);
    let mut recorded = false;
    let mut char_index = 0usize;
    for grapheme in buffer.graphemes(true) {
        let next_char_index = char_index + grapheme.chars().count();
        if !recorded && cursor <= char_index {
            crow = lines.len() as u16;
            ccol = cells as u16;
            recorded = true;
        }
        if !recorded && cursor < next_char_index {
            crow = lines.len() as u16;
            ccol = cells as u16;
            recorded = true;
        }
        if grapheme == "\n" {
            lines.push(std::mem::take(&mut line));
            cells = 0;
        } else {
            let cw = str_cells(grapheme);
            if cells + cw > w && cells > 0 {
                lines.push(std::mem::take(&mut line));
                cells = 0;
            }
            line.push_str(grapheme);
            cells += cw;
        }
        char_index = next_char_index;
    }
    if !recorded {
        crow = lines.len() as u16; // 光标在末尾
        ccol = cells as u16;
    }
    lines.push(line);
    (lines, crow, ccol)
}

/// 静态提交一段文本需占的终端行数(供 `insert_before`)。
#[cfg(test)]
pub(crate) fn commit_height(text: &str, width: u16) -> u16 {
    wrapped_rows(text, width).min(u16::MAX as usize).max(1) as u16
}

/// Preview a queued prompt in chrome, notes, and the queue panel.
pub(crate) fn queue_preview(text: &str, width: u16) -> String {
    let cleaned = sanitize_display_text(text);
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        return "(empty)".into();
    }
    clip_display_cells(cleaned, width.max(1))
}

/// 粘贴净化(iter-24):CRLF/CR 归一 LF,滤除其余控制字符(留 \n \t),防转义序列注入输入框。
pub(crate) fn sanitize_paste(s: &str) -> String {
    let stripped = s
        .replace("\u{1b}[200~", "")
        .replace("\u{1b}[201~", "")
        .replace("[200~", "")
        .replace("[201~", "");
    stripped
        .replace("\r\n", "\n")
        .chars()
        .map(|c| if c == '\r' { '\n' } else { c })
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

/// Display sanitization: strip ANSI CSI/OSC and other control characters so
/// model/tool text cannot contaminate the TUI buffer or native scrollback.
pub(crate) fn sanitize_display_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                Some('[') => skip_csi(&mut chars),
                Some(']') => skip_osc(&mut chars),
                Some(_) | None => {}
            },
            '\u{9b}' => skip_csi(&mut chars),
            '\u{9d}' => skip_osc(&mut chars),
            '\n' | '\t' => out.push(c),
            c if !c.is_control() && c != '\u{7f}' => out.push(c),
            _ => {}
        }
    }
    out
}

fn skip_csi<I>(chars: &mut std::iter::Peekable<I>)
where
    I: Iterator<Item = char>,
{
    while let Some(&c) = chars.peek() {
        // A malformed sequence must not consume the next visible line. Leave
        // the newline for the outer sanitizer so Answer/Reasoning rows survive.
        if matches!(c, '\n' | '\r') {
            break;
        }
        chars.next();
        if ('@'..='~').contains(&c) {
            break;
        }
    }
}

fn skip_osc<I>(chars: &mut std::iter::Peekable<I>)
where
    I: Iterator<Item = char>,
{
    while let Some(&c) = chars.peek() {
        // Recover at a line boundary when BEL/ST is missing; otherwise an
        // accidental OSC opener can hide the rest of a streamed answer.
        if matches!(c, '\n' | '\r') {
            break;
        }
        chars.next();
        if c == '\u{7}' {
            break;
        }
        if c == '\u{1b}' && chars.peek() == Some(&'\\') {
            chars.next();
            break;
        }
    }
}

/// 动态输入框高度(iter-24):按内容折行数伸缩,clamp 在 [min,max](计入上下边框 2 行)。
pub(crate) fn input_height(content: &str, width: u16, min: u16, max: u16) -> u16 {
    (wrapped_rows(content, width).min(u16::MAX as usize) as u16)
        .saturating_add(2)
        .clamp(min, max)
}

/// 流式尾巴:Live 视口只显示正在生成文本的最后 `k` 行(前面的行等 Superstep 后整段历史化)。
#[cfg(test)]
pub(crate) fn stream_tail(stream: &str, k: usize) -> Vec<&str> {
    let lines: Vec<&str> = stream.lines().collect();
    let start = lines.len().saturating_sub(k);
    lines[start..].to_vec()
}

/// TODO 进度 (done, total);空清单 → None(状态行不显)。
pub(crate) fn todo_progress(todos: &[Todo]) -> Option<(usize, usize)> {
    if todos.is_empty() {
        return None;
    }
    let done = todos.iter().filter(|t| t.status == "completed").count();
    Some((done, todos.len()))
}

/// TODO 清单渲染成静态提交块(变更时整段落进终端历史,取代旧侧边栏面板)。
pub(crate) fn render_todo_block(todos: &[Todo]) -> String {
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

/// 语义化色角色：与启动动画共用 RidgeCode 的 olive → violet → blue → ice
/// 主题，避免启动画面与 TUI 像两个产品。颜色集中于此，正文渲染不各自发明色值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    /// 品牌/焦点:提示符、状态行徽标、浮窗高亮、流式游标。
    Primary,
    /// 用户命令回显(`›`)。
    Command,
    /// 完成的模型回答与回答通道正文。
    Answer,
    /// 模型思考/推理通道。与通用次要文本分离，避免思考被压成不可读的暗灰。
    Reasoning,
    /// 系统信息、事件默认色。
    Info,
    Success,
    Error,
    Warn,
    /// 面板/围栏边框。
    Border,
    /// 次要文本、代码块内文。
    Muted,
    /// 紧凑遥测的数值，不代表用户输入命令。
    Metric,
    /// 紧凑遥测的字段标签与分隔语义。
    Label,
    DiffAdd,
    DiffDel,
}

// Renaissance chrome: gold / wine / bronze / parchment / ink.
// Splash pixel contract keeps its own hardcoded RGB.
pub(crate) const THEME_OLIVE: Color = Color::Rgb(201, 162, 39);
pub(crate) const THEME_VIOLET: Color = Color::Rgb(122, 42, 50);
pub(crate) const THEME_BLUE: Color = Color::Rgb(184, 124, 48);
pub(crate) const THEME_ICE: Color = Color::Rgb(244, 232, 204);
pub(crate) const THEME_BORDER: Color = Color::Rgb(92, 64, 40);
pub(crate) const THEME_MUTED: Color = Color::Rgb(148, 120, 88);

pub(crate) fn role_color(r: Role) -> Color {
    match r {
        Role::Primary => THEME_BLUE,
        Role::Command => THEME_OLIVE,
        Role::Answer => THEME_ICE,
        Role::Reasoning => THEME_VIOLET,
        Role::Info => THEME_BLUE,
        Role::Success => THEME_OLIVE,
        Role::Error => THEME_VIOLET,
        Role::Warn => THEME_OLIVE,
        Role::Border => THEME_BORDER,
        Role::Muted => THEME_MUTED,
        Role::Metric => THEME_ICE,
        Role::Label => THEME_MUTED,
        Role::DiffAdd => THEME_OLIVE,
        Role::DiffDel => THEME_VIOLET,
    }
}

/// Telemetry chrome uses the terminal surface instead of painting a full
/// gray band.  This keeps muted context text readable on both dark and light
/// terminal themes and leaves the cyan rail as the single focus accent.
pub(crate) fn telemetry_surface() -> Style {
    Style::default().bg(Color::Reset)
}

/// Quiet selection affordance: retain a background only for focus, with no
/// high-contrast neon block competing with the transcript.
pub(crate) fn selection_style() -> Style {
    Style::default()
        .fg(role_color(Role::Primary))
        .bg(Color::Rgb(48, 35, 78))
        .add_modifier(Modifier::BOLD)
}

/// 行内 md 扫描:`` `code` ``(Warn 色)与 `**bold**`(加粗);未闭合记号按字面。纯函数。
pub(crate) fn inline_md_spans(text: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut rest = text;
    loop {
        let Some((pos, marker)) = next_inline_marker(rest) else {
            if !rest.is_empty() {
                spans.push(Span::raw(rest.to_owned()));
            }
            break;
        };
        if pos > 0 {
            spans.push(Span::raw(rest[..pos].to_owned()));
        }
        let Some((mut marked, next)) = consume_inline_marker(rest, pos, marker) else {
            let marker_len = rest[pos..].chars().next().map(char::len_utf8).unwrap_or(0);
            spans.push(Span::raw(rest[pos..pos + marker_len].to_owned()));
            rest = &rest[pos + marker_len..];
            continue;
        };
        spans.append(&mut marked);
        rest = &rest[next..];
    }
    spans
}

#[derive(Clone, Copy)]
enum InlineMarker {
    Code,
    Bold,
    Link,
    Emphasis(char),
}

fn next_inline_marker(text: &str) -> Option<(usize, InlineMarker)> {
    let mut candidates = Vec::with_capacity(5);
    if let Some(position) = text.find('`') {
        candidates.push((position, 0, InlineMarker::Code));
    }
    if let Some(position) = text.find("**") {
        candidates.push((position, 1, InlineMarker::Bold));
    }
    if let Some(position) = text.find('[') {
        candidates.push((position, 2, InlineMarker::Link));
    }
    if let Some(position) = text.find('*') {
        candidates.push((position, 3, InlineMarker::Emphasis('*')));
    }
    if let Some(position) = text.find('_') {
        candidates.push((position, 4, InlineMarker::Emphasis('_')));
    }
    candidates
        .into_iter()
        .min_by_key(|(position, priority, _)| (*position, *priority))
        .map(|(position, _, marker)| (position, marker))
}

fn consume_inline_marker(
    text: &str,
    position: usize,
    marker: InlineMarker,
) -> Option<(Vec<Span<'static>>, usize)> {
    if matches!(marker, InlineMarker::Link) {
        let suffix = &text[position + 1..];
        let middle = suffix.find("](")?;
        let url_start = position + 1 + middle + 2;
        let url_end = text[url_start..].find(')')? + url_start;
        let label = &suffix[..middle];
        let url = &text[url_start..url_end];
        if label.is_empty() || url.is_empty() {
            return None;
        }
        return Some((
            vec![
                Span::styled(
                    label.to_owned(),
                    Style::default()
                        .fg(role_color(Role::Info))
                        .add_modifier(Modifier::UNDERLINED),
                ),
                Span::raw(" ("),
                Span::styled(url.to_owned(), Style::default().fg(role_color(Role::Muted))),
                Span::raw(")"),
            ],
            url_end + 1,
        ));
    }
    let (delimiter, style) = match marker {
        InlineMarker::Code => ("`", Style::default().fg(role_color(Role::Success))),
        InlineMarker::Bold => ("**", Style::default().add_modifier(Modifier::BOLD)),
        InlineMarker::Emphasis(delimiter) => (
            if delimiter == '*' { "*" } else { "_" },
            Style::default().add_modifier(Modifier::ITALIC),
        ),
        InlineMarker::Link => unreachable!("link handled above"),
    };
    let delimiter_len = delimiter.len();
    let suffix = &text[position + delimiter_len..];
    let end = suffix.find(delimiter)?;
    if end == 0 {
        return None;
    }
    let inner = suffix[..end].to_owned();
    Some((
        vec![Span::styled(inner, style)],
        position + delimiter_len + end + delimiter_len,
    ))
}

fn markdown_quote_prefix(line: &str) -> Option<(String, &str)> {
    let bytes = line.as_bytes();
    let mut start = 0;
    while matches!(bytes.get(start), Some(b' ' | b'\t')) {
        start += 1;
    }
    if bytes.get(start) != Some(&b'>') {
        return None;
    }

    let mut end = start;
    loop {
        if bytes.get(end) != Some(&b'>') {
            break;
        }
        end += 1;
        while matches!(bytes.get(end), Some(b' ' | b'\t')) {
            end += 1;
        }
        if bytes.get(end) != Some(&b'>') {
            break;
        }
    }

    let rail = line[..end]
        .chars()
        .map(|c| if c == '>' { '│' } else { c })
        .collect();
    Some((rail, &line[end..]))
}

fn markdown_list_prefix(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut end = 0;
    while matches!(bytes.get(end), Some(b' ' | b'\t')) {
        end += 1;
    }
    let marker = end;
    match bytes.get(end) {
        Some(b'-' | b'+' | b'*') => {
            end += 1;
        }
        Some(b'0'..=b'9') => {
            while matches!(bytes.get(end), Some(b'0'..=b'9')) {
                end += 1;
            }
            if !matches!(bytes.get(end), Some(b'.' | b')')) {
                return None;
            }
            end += 1;
        }
        _ => return None,
    }
    if end == marker || !matches!(bytes.get(end), Some(b' ' | b'\t')) {
        return None;
    }
    while matches!(bytes.get(end), Some(b' ' | b'\t')) {
        end += 1;
    }
    Some(end)
}

/// 结构化 Markdown 前缀：引用走信息色侧栏，列表保留缩进与标记；正文仍走行内扫描。
/// 纯呈现层投影，不改变 Answer 文本或围栏状态。
fn markdown_structure_spans(line: &str) -> Option<Vec<Span<'static>>> {
    let (quote, mut body) = markdown_quote_prefix(line)
        .map(|(prefix, rest)| (Some(prefix), rest))
        .unwrap_or((None, line));
    let list_end = markdown_list_prefix(body);
    if quote.is_none() && list_end.is_none() {
        return None;
    }

    let mut spans = Vec::with_capacity(3);
    if let Some(prefix) = quote {
        spans.push(Span::styled(
            prefix,
            Style::default().fg(role_color(Role::Info)),
        ));
    }
    if let Some(end) = list_end {
        spans.push(Span::styled(
            body[..end].to_owned(),
            Style::default().fg(role_color(Role::Info)),
        ));
        body = &body[end..];
    }
    spans.extend(inline_md_spans(body));
    Some(spans)
}

/// 告警块只改变呈现层的左边界，不增物理行、不改正文。
/// `Single` 用于尚未形成多行块的流式片段；静态/可见尾部有完整上下文时，
/// `Top`/`Middle`/`Bottom` 组成稳定的 ANSI16 容器边界。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AlertEdge {
    Single,
    Top,
    Middle,
    Bottom,
}

fn alert_body(line: &str) -> &str {
    line.strip_prefix("🤖 ").unwrap_or(line)
}

fn is_alert_continuation(line: &str) -> bool {
    let line = alert_body(line);
    markdown_quote_prefix(line).is_some() && markdown_alert_role(line).is_none()
}

/// Produce one bounded edge marker per logical line. The caller supplies only
/// the visible/tail lines, so this helper never turns the live renderer into a
/// whole-document scan.
pub(crate) fn alert_edges<'a, I>(lines: I) -> Vec<Option<AlertEdge>>
where
    I: IntoIterator<Item = &'a str>,
{
    let lines = lines.into_iter().collect::<Vec<_>>();
    let mut edges = vec![None; lines.len()];
    let mut index = 0;
    while index < lines.len() {
        if !is_alert_start(lines[index]) {
            index += 1;
            continue;
        }
        let end = alert_run_end(&lines, index);
        mark_alert_edges(&mut edges, index, end);
        index = end + 1;
    }
    edges
}

fn is_alert_start(line: &str) -> bool {
    markdown_alert_role(alert_body(line)).is_some()
}

fn alert_run_end(lines: &[&str], start: usize) -> usize {
    let mut end = start;
    while end + 1 < lines.len() && is_alert_continuation(lines[end + 1]) {
        end += 1;
    }
    end
}

fn mark_alert_edges(edges: &mut [Option<AlertEdge>], start: usize, end: usize) {
    if end == start {
        edges[start] = Some(AlertEdge::Single);
        return;
    }
    edges[start] = Some(AlertEdge::Top);
    for (offset, edge) in edges[start + 1..=end].iter_mut().enumerate() {
        *edge = Some(if start + 1 + offset == end {
            AlertEdge::Bottom
        } else {
            AlertEdge::Middle
        });
    }
}

fn alert_edge_glyph(edge: AlertEdge) -> &'static str {
    match edge {
        AlertEdge::Single => "│",
        AlertEdge::Top => "┌",
        AlertEdge::Middle => "│",
        AlertEdge::Bottom => "└",
    }
}

pub(crate) fn apply_alert_edge(spans: &mut [Span<'static>], edge: AlertEdge) {
    let Some(first) = spans.first_mut() else {
        return;
    };
    let Some(index) = first.content.rfind('│') else {
        return;
    };
    let mut content = first.content.to_string();
    content.replace_range(index..index + '│'.len_utf8(), alert_edge_glyph(edge));
    first.content = Cow::Owned(content);
}

/// Markdown alert/callout 的展示投影：保留正文语义，给标记加 ANSI16 角色色与侧栏。
/// 仅识别标准 `> [!NOTE]` 等行级标记，避免把普通引用误判为告警块。
fn markdown_alert_spans(line: &str) -> Option<Vec<Span<'static>>> {
    let (quote, body) = markdown_quote_prefix(line)?;
    let marker = body.trim_start();
    let (label, role, rest) = [
        ("[!NOTE]", "NOTE", Role::Info),
        ("[!TIP]", "TIP", Role::Success),
        ("[!IMPORTANT]", "IMPORTANT", Role::Primary),
        ("[!WARNING]", "WARNING", Role::Warn),
        ("[!CAUTION]", "CAUTION", Role::Error),
    ]
    .into_iter()
    .find_map(|(marker_text, label, role)| {
        let rest = marker.strip_prefix(marker_text)?;
        if rest
            .chars()
            .next()
            .is_some_and(|c| !matches!(c, ' ' | '\t'))
        {
            return None;
        }
        Some((label, role, rest.trim_start_matches([' ', '\t'])))
    })?;

    let color = role_color(role);
    let mut spans = vec![
        Span::styled(quote, Style::default().fg(color)),
        Span::styled(
            label.to_owned(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
    ];
    if !rest.is_empty() {
        spans.push(Span::styled(" ┃ ".to_owned(), Style::default().fg(color)));
        spans.extend(inline_md_spans(rest));
    }
    Some(spans)
}

fn markdown_alert_role(line: &str) -> Option<Role> {
    let (_, body) = markdown_quote_prefix(line)?;
    let marker = body.trim_start();
    [
        ("[!NOTE]", Role::Info),
        ("[!TIP]", Role::Success),
        ("[!IMPORTANT]", Role::Primary),
        ("[!WARNING]", Role::Warn),
        ("[!CAUTION]", Role::Error),
    ]
    .into_iter()
    .find_map(|(marker_text, role)| {
        let rest = marker.strip_prefix(marker_text)?;
        if rest
            .chars()
            .next()
            .is_some_and(|c| !matches!(c, ' ' | '\t'))
        {
            None
        } else {
            Some(role)
        }
    })
}

/// Keep the semantic rail on subsequent quoted lines of one alert block.
/// The body stays normal Markdown; only the structural rail inherits the
/// alert role, so long conclusions remain readable without adding layout rows.
fn markdown_alert_continuation_spans(line: &str, role: Role) -> Option<Vec<Span<'static>>> {
    let (quote, body) = markdown_quote_prefix(line)?;
    if body.trim_start().starts_with("[!") {
        return None;
    }
    let mut spans = vec![Span::styled(quote, Style::default().fg(role_color(role)))];
    spans.extend(inline_md_spans(body));
    Some(spans)
}

fn code_identifier_start(c: char) -> bool {
    c.is_ascii_alphabetic() || matches!(c, '_' | '$')
}

fn code_identifier_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '$')
}

fn code_token_role(token: &str) -> Option<Role> {
    if matches!(
        token,
        "as" | "async"
            | "await"
            | "break"
            | "catch"
            | "class"
            | "const"
            | "continue"
            | "crate"
            | "def"
            | "else"
            | "enum"
            | "extends"
            | "fn"
            | "for"
            | "from"
            | "function"
            | "if"
            | "impl"
            | "import"
            | "in"
            | "interface"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "new"
            | "or"
            | "pub"
            | "ref"
            | "return"
            | "self"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "try"
            | "type"
            | "unsafe"
            | "use"
            | "var"
            | "while"
            | "with"
            | "yield"
    ) {
        Some(Role::Primary)
    } else if matches!(
        token,
        "Err"
            | "False"
            | "None"
            | "Ok"
            | "Some"
            | "True"
            | "Undefined"
            | "false"
            | "null"
            | "true"
            | "undefined"
    ) {
        Some(Role::Warn)
    } else if matches!(
        token,
        "String"
            | "Vec"
            | "bool"
            | "f32"
            | "f64"
            | "i32"
            | "i64"
            | "isize"
            | "str"
            | "u32"
            | "u64"
            | "usize"
    ) {
        Some(Role::Info)
    } else {
        None
    }
}

fn code_quote_end(text: &str, start: usize, quote: char) -> usize {
    let content_start = start + quote.len_utf8();
    let mut escaped = false;
    for (offset, c) in text[content_start..].char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == quote {
            return content_start + offset + c.len_utf8();
        }
    }
    text.len()
}

fn push_code_span(spans: &mut Vec<Span<'static>>, text: &str, role: Role) {
    if !text.is_empty() {
        spans.push(Span::styled(
            text.to_owned(),
            Style::default().fg(role_color(role)),
        ));
    }
}

fn flush_code_plain(spans: &mut Vec<Span<'static>>, plain: &mut String) {
    if !plain.is_empty() {
        push_code_span(spans, plain, Role::Muted);
        plain.clear();
    }
}

fn code_line_spans(text: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::with_capacity(8);
    let mut plain = String::new();
    let mut index = 0;
    while index < text.len() {
        let rest = &text[index..];
        let c = rest
            .chars()
            .next()
            .expect("code index is on a char boundary");

        if is_code_comment_start(text, index, rest, c) {
            flush_code_plain(&mut spans, &mut plain);
            push_code_span(&mut spans, rest, Role::Muted);
            break;
        }

        if matches!(c, '\'' | '"' | '`') {
            flush_code_plain(&mut spans, &mut plain);
            let end = code_quote_end(text, index, c);
            push_code_span(&mut spans, &text[index..end], Role::Success);
            index = end;
            continue;
        }

        if c.is_ascii_digit()
            && text[..index]
                .chars()
                .next_back()
                .is_none_or(|previous| !code_identifier_continue(previous))
        {
            flush_code_plain(&mut spans, &mut plain);
            let end = code_number_end(text, index, c);
            push_code_span(&mut spans, &text[index..end], Role::Warn);
            index = end;
            continue;
        }

        if code_identifier_start(c) {
            let end = code_identifier_end(text, index, c);
            let token = &text[index..end];
            append_code_identifier(&mut spans, &mut plain, token);
            index = end;
            continue;
        }

        plain.push(c);
        index += c.len_utf8();
    }
    flush_code_plain(&mut spans, &mut plain);
    spans
}

fn is_code_comment_start(text: &str, index: usize, rest: &str, c: char) -> bool {
    rest.starts_with("//")
        || (c == '#'
            && !rest.starts_with("#[")
            && text[..index]
                .chars()
                .next_back()
                .is_none_or(char::is_whitespace))
}

fn code_number_end(text: &str, index: usize, first: char) -> usize {
    text[index..]
        .char_indices()
        .take_while(|(_, value)| value.is_ascii_alphanumeric() || matches!(*value, '_' | '.'))
        .last()
        .map(|(offset, value)| index + offset + value.len_utf8())
        .unwrap_or(index + first.len_utf8())
}

fn code_identifier_end(text: &str, index: usize, first: char) -> usize {
    text[index..]
        .char_indices()
        .take_while(|(_, value)| code_identifier_continue(*value))
        .last()
        .map(|(offset, value)| index + offset + value.len_utf8())
        .unwrap_or(index + first.len_utf8())
}

fn append_code_identifier(spans: &mut Vec<Span<'static>>, plain: &mut String, token: &str) {
    if let Some(role) = code_token_role(token) {
        flush_code_plain(spans, plain);
        push_code_span(spans, token, role);
    } else {
        plain.push_str(token);
    }
}

/// 行级 md 轻渲染(iter-28):静态提交与 Live Answer 共用；样式仅存在呈现层。
/// ``` 围栏切态(围栏行 Border 色)、块内 bounded code roles、`#` 标题加粗 Primary、引用/列表有结构侧栏、余走行内扫描。
#[cfg(test)]
pub(crate) fn md_line_spans(line: &str, in_code: bool) -> (Vec<Span<'static>>, bool) {
    let mut alert_role = None;
    md_line_spans_with_alert(line, in_code, &mut alert_role)
}

pub(crate) fn md_line_spans_with_alert(
    line: &str,
    in_code: bool,
    alert_role: &mut Option<Role>,
) -> (Vec<Span<'static>>, bool) {
    let trimmed = line.trim_start();
    if trimmed.starts_with("```") {
        *alert_role = None;
        return (fence_line_spans(line), next_fence_state(trimmed, in_code));
    }
    if in_code {
        *alert_role = None;
        return (code_line_spans(line), true);
    }
    if trimmed.starts_with('#') {
        *alert_role = None;
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
    if let Some(spans) = markdown_alert_spans(line) {
        *alert_role = markdown_alert_role(line);
        return (spans, false);
    }
    if let Some(role) = *alert_role {
        if let Some(spans) = markdown_alert_continuation_spans(line, role) {
            return (spans, false);
        }
        *alert_role = None;
    }
    if let Some(spans) = markdown_structure_spans(line) {
        return (spans, false);
    }
    (inline_md_spans(line), false)
}

/// Keep the fence text byte-for-byte unchanged while giving a recognized
/// language token its own semantic role.  This is a display-only cue: no
/// cells are added, so static scrollback and the live narrow fallback keep
/// their existing wrap/height behavior.
fn fence_line_spans(line: &str) -> Vec<Span<'static>> {
    let Some(language) = fence_language(line) else {
        return vec![Span::styled(
            line.to_owned(),
            Style::default().fg(role_color(Role::Border)),
        )];
    };
    let trimmed = line.trim_start();
    let leading_bytes = line.len() - trimmed.len();
    let language_start = leading_bytes + 3;
    let language_end = language_start + language.len();
    let border = Style::default().fg(role_color(Role::Border));
    let language_style = Style::default()
        .fg(role_color(Role::Info))
        .add_modifier(Modifier::BOLD);
    let mut spans = vec![Span::styled(line[..language_start].to_owned(), border)];
    spans.push(Span::styled(
        line[language_start..language_end].to_owned(),
        language_style,
    ));
    if language_end < line.len() {
        spans.push(Span::styled(line[language_end..].to_owned(), border));
    }
    spans
}

fn next_fence_state(trimmed: &str, in_code: bool) -> bool {
    if trimmed.starts_with("```") {
        !in_code
    } else {
        in_code
    }
}

/// Live Answer 的有界 Markdown 投影：先按完整行推进围栏状态，再按 cell 宽裁切可见文本。
/// 未闭合标记保持字面；每帧只处理已进入视口的行，不把解析状态写回 transcript。
pub(crate) fn live_markdown_spans_with_alert(
    text: &str,
    in_code: &mut bool,
    base_color: Color,
    modifier: Modifier,
    alert_role: &mut Option<Role>,
) -> Vec<Span<'static>> {
    let start_code = *in_code;
    let next_code = next_fence_state(text.trim_start(), start_code);
    let (mut spans, _) = md_line_spans_with_alert(text, start_code, alert_role);
    *in_code = next_code;

    for span in &mut spans {
        let color = span.style.fg.unwrap_or(base_color);
        span.style = Style::default()
            .fg(color)
            .add_modifier(modifier)
            .add_modifier(span.style.add_modifier);
    }
    spans
}

pub(crate) fn live_markdown_spans_with_alert_edge(
    text: &str,
    in_code: &mut bool,
    base_color: Color,
    modifier: Modifier,
    alert_role: &mut Option<Role>,
    alert_edge: Option<AlertEdge>,
) -> Vec<Span<'static>> {
    let mut spans = live_markdown_spans_with_alert(text, in_code, base_color, modifier, alert_role);
    if let Some(edge) = alert_edge {
        apply_alert_edge(&mut spans, edge);
    }
    spans
}

/// Live Answer 的兼容单行投影：保留旧的 cell 裁切口径，供纯逻辑测试与窄徽标使用。
#[cfg(test)]
pub(crate) fn live_markdown_line(
    text: &str,
    width: u16,
    in_code: &mut bool,
    base_color: Color,
    modifier: Modifier,
) -> Vec<Span<'static>> {
    let start_code = *in_code;
    let next_code = next_fence_state(text.trim_start(), start_code);
    let clipped = clip_display_cells(text, width);
    let (mut spans, _) = if start_code && !clipped.trim_start().starts_with("```") {
        (code_line_spans(&clipped), true)
    } else {
        md_line_spans(&clipped, start_code)
    };
    *in_code = next_code;

    for span in &mut spans {
        let color = span.style.fg.unwrap_or(base_color);
        span.style = Style::default()
            .fg(color)
            .add_modifier(modifier)
            .add_modifier(span.style.add_modifier);
    }
    spans
}

const MAX_FENCE_LANGUAGE_CELLS: usize = 10;

/// 提取受限的 fenced-code language token；仅作 Live 视觉 badge，不改变 Markdown 语义。
pub(crate) fn fence_language(text: &str) -> Option<&str> {
    let trimmed = text.trim_start();
    let language = trimmed.strip_prefix("```")?.split_whitespace().next()?;
    if language.is_empty()
        || str_cells(language) > MAX_FENCE_LANGUAGE_CELLS
        || !language
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-' | '.' | '#'))
    {
        return None;
    }
    Some(language)
}

/// 将已识别语言的围栏正文归一为裸围栏；语言由旁侧 badge 承载。
pub(crate) fn fence_without_language(text: &str) -> String {
    let trimmed = text.trim_start();
    let leading = text.len() - trimmed.len();
    format!("{}{}", &text[..leading], "```")
}

/// 最终回答静态提交的行级渲染：首行徽标走 Primary，其余内容沿用 Markdown 语义角色。
/// 代码围栏状态跨行传递，故不会因中间换行把代码块误当普通回答。
#[cfg(test)]
pub(crate) fn markdown_lines(text: &str) -> Vec<Line<'static>> {
    markdown_lines_with_width(text, 120)
}

fn table_cells(line: &str) -> Option<Vec<String>> {
    let trimmed = line.strip_prefix("🤖 ").unwrap_or(line).trim();
    if !trimmed.contains('|') {
        return None;
    }
    let body = trimmed
        .strip_prefix('|')
        .unwrap_or(trimmed)
        .strip_suffix('|')
        .unwrap_or_else(|| trimmed.strip_prefix('|').unwrap_or(trimmed));
    let cells = body
        .split('|')
        .map(|cell| cell.trim().to_owned())
        .collect::<Vec<_>>();
    (cells.len() >= 2).then_some(cells)
}

fn table_delimiter(cells: &[String]) -> bool {
    cells.iter().all(|cell| {
        let marker = cell.trim().trim_start_matches(':').trim_end_matches(':');
        marker.len() >= 3 && marker.bytes().all(|byte| byte == b'-')
    })
}

fn table_widths(header: &[String], rows: &[Vec<String>], width: usize) -> Option<Vec<usize>> {
    let columns = header.len();
    let separators = columns.saturating_sub(1).saturating_mul(3);
    let available = width.saturating_sub(separators);
    if available < columns.saturating_mul(8) {
        return None;
    }
    let mut desired = header
        .iter()
        .map(|cell| str_cells(cell).max(1))
        .collect::<Vec<_>>();
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            desired[index] = desired[index].max(str_cells(cell).max(1));
        }
    }
    let mut widths = vec![8; columns];
    let mut remaining = available.saturating_sub(columns * 8);
    while remaining > 0 {
        let mut grew = false;
        for index in 0..columns {
            if remaining == 0 {
                break;
            }
            if widths[index] < desired[index] {
                widths[index] += 1;
                remaining -= 1;
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    Some(widths)
}

fn table_cell_lines(cell: &str, width: usize, header: bool) -> Vec<Vec<Span<'static>>> {
    let mut spans = inline_md_spans(cell);
    if spans.is_empty() {
        spans.push(Span::raw(String::new()));
    }
    if header {
        for span in &mut spans {
            span.style = span
                .style
                .fg(role_color(Role::Primary))
                .add_modifier(Modifier::BOLD);
        }
    }
    wrap_live_spans_greedy(spans, width.max(1) as u16)
}

fn table_row_lines(cells: &[String], widths: &[usize], header: bool) -> Vec<Line<'static>> {
    let wrapped = cells
        .iter()
        .zip(widths)
        .map(|(cell, width)| table_cell_lines(cell, *width, header))
        .collect::<Vec<_>>();
    let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
    (0..height)
        .map(|row_index| {
            let mut spans = Vec::new();
            for (column, width) in widths.iter().enumerate() {
                let cell = wrapped[column].get(row_index).cloned().unwrap_or_default();
                let used = cell
                    .iter()
                    .map(|span| str_cells(span.content.as_ref()))
                    .sum::<usize>();
                spans.extend(cell);
                if used < *width {
                    spans.push(Span::raw(" ".repeat(*width - used)));
                }
                if column + 1 < widths.len() {
                    spans.push(Span::styled(
                        " │ ",
                        Style::default().fg(role_color(Role::Border)),
                    ));
                }
            }
            Line::from(spans)
        })
        .collect()
}

fn stacked_table_lines(
    header: &[String],
    rows: &[Vec<String>],
    width: usize,
) -> Vec<Line<'static>> {
    let rows = if rows.is_empty() {
        vec![vec![String::new(); header.len()]]
    } else {
        rows.to_vec()
    };
    let mut lines = Vec::new();
    for (row_index, row) in rows.iter().enumerate() {
        if row_index > 0 {
            lines.push(Line::default());
        }
        for (label, value) in header.iter().zip(row) {
            let mut spans = vec![Span::styled(
                format!("{label}: "),
                Style::default()
                    .fg(role_color(Role::Primary))
                    .add_modifier(Modifier::BOLD),
            )];
            spans.extend(inline_md_spans(value));
            lines.extend(
                wrap_live_spans_greedy(spans, width.max(1) as u16)
                    .into_iter()
                    .map(Line::from),
            );
        }
    }
    lines
}

fn render_table(header: &[String], rows: &[Vec<String>], width: usize) -> Vec<Line<'static>> {
    let Some(widths) = table_widths(header, rows, width) else {
        return stacked_table_lines(header, rows, width);
    };
    let mut lines = table_row_lines(header, &widths, true);
    let divider = widths
        .iter()
        .map(|width| "─".repeat(*width))
        .collect::<Vec<_>>()
        .join("─┼─");
    lines.push(Line::from(Span::styled(
        divider,
        Style::default().fg(role_color(Role::Border)),
    )));
    for row in rows {
        lines.extend(table_row_lines(row, &widths, false));
    }
    lines
}

pub(crate) fn markdown_lines_with_width(text: &str, width: u16) -> Vec<Line<'static>> {
    let mut in_code = false;
    let mut alert_role = None;
    let source_lines = text.lines().collect::<Vec<_>>();
    let edges = alert_edges(source_lines.iter().copied());
    let mut rendered = Vec::new();
    let mut index = 0;
    while index < source_lines.len() {
        let table = if !in_code && index + 1 < source_lines.len() {
            table_cells(source_lines[index]).and_then(|header| {
                let delimiter = table_cells(source_lines[index + 1])?;
                (delimiter.len() == header.len() && table_delimiter(&delimiter)).then_some(header)
            })
        } else {
            None
        };
        if let Some(header) = table {
            let mut end = index + 2;
            let mut rows = Vec::new();
            while end < source_lines.len() {
                let Some(row) = table_cells(source_lines[end]) else {
                    break;
                };
                if row.len() != header.len() || table_delimiter(&row) {
                    break;
                }
                rows.push(row);
                end += 1;
            }
            rendered.extend(render_table(&header, &rows, width.max(1) as usize));
            index = end;
            continue;
        }
        let (spans, next) = answer_line_spans(
            source_lines[index],
            in_code,
            &mut alert_role,
            edges.get(index).copied().flatten(),
        );
        in_code = next;
        rendered.push(Line::from(spans));
        index += 1;
    }
    rendered
}

/// Stable presentation rail for an answer that has left the live viewport.
/// The live renderer already exposes the same semantic channel; keeping this
/// prefix in committed scrollback prevents the final answer from becoming
/// indistinguishable from an untyped Markdown note.
fn answer_commit_rail(index: usize, last_index: usize) -> &'static str {
    if index == 0 {
        "╭ ANSWER "
    } else if index == last_index {
        "╰ "
    } else {
        "│ "
    }
}

fn answer_commit_hint(index: usize, partial: bool) -> &'static str {
    match (index, partial) {
        (0, true) => "  [PARTIAL · Ctrl+A answers]",
        (0, false) => "  [Ctrl+A answers]",
        _ => "",
    }
}

#[cfg(test)]
pub(crate) fn answer_commit_measure(text: &str) -> String {
    let lines = text.lines().collect::<Vec<_>>();
    let last_index = lines.len().saturating_sub(1);
    lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            format!(
                "{}{line}{}",
                answer_commit_rail(index, last_index),
                answer_commit_hint(index, false)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
pub(crate) fn answer_commit_lines(text: &str) -> Vec<Line<'static>> {
    answer_commit_lines_with_status(text, false)
}

#[cfg(test)]
pub(crate) fn answer_commit_lines_with_status(text: &str, partial: bool) -> Vec<Line<'static>> {
    answer_commit_lines_with_status_and_metrics(text, partial, None)
}

fn answer_commit_meta(metrics: PresentationMetrics) -> String {
    let step = if metrics.step > 0 {
        format!("step {} · ", metrics.step)
    } else {
        String::new()
    };
    format!(
        "[{step}+{}s · {} task tok] ",
        metrics.elapsed_s, metrics.tokens
    )
}

#[cfg(test)]
pub(crate) fn answer_commit_lines_with_status_and_metrics(
    text: &str,
    partial: bool,
    metrics: Option<PresentationMetrics>,
) -> Vec<Line<'static>> {
    answer_commit_lines_with_status_and_metrics_at_width(text, partial, metrics, 120)
}

pub(crate) fn answer_commit_lines_with_status_and_metrics_at_width(
    text: &str,
    partial: bool,
    metrics: Option<PresentationMetrics>,
    width: u16,
) -> Vec<Line<'static>> {
    let text = super::tighten_answer_spacing(text);
    let lines = markdown_lines_with_width(&text, width.saturating_sub(2).max(1));
    let last_index = lines.len().saturating_sub(1);
    lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let rail = answer_commit_rail(index, last_index);
            let mut spans = Vec::with_capacity(line.spans.len() + 1);
            spans.push(Span::styled(
                rail,
                Style::default().fg(role_color(Role::Primary)),
            ));
            if index == 0 {
                if let Some(metrics) = metrics {
                    spans.push(Span::styled(
                        answer_commit_meta(metrics),
                        Style::default()
                            .fg(role_color(Role::Label))
                            .add_modifier(Modifier::DIM),
                    ));
                }
            }
            spans.extend(line.spans);
            let hint = answer_commit_hint(index, partial);
            if !hint.is_empty() {
                spans.push(Span::styled(
                    hint,
                    Style::default().fg(role_color(if partial { Role::Warn } else { Role::Muted })),
                ));
            }
            Line::from(spans)
        })
        .collect()
}

fn answer_line_spans(
    line: &str,
    in_code: bool,
    alert_role: &mut Option<Role>,
    alert_edge: Option<AlertEdge>,
) -> (Vec<Span<'static>>, bool) {
    let Some(body) = line.strip_prefix("🤖 ") else {
        let (mut spans, next) = md_line_spans_with_alert(line, in_code, alert_role);
        if let Some(edge) = alert_edge {
            apply_alert_edge(&mut spans, edge);
        }
        return (spans, next);
    };
    let mut spans = vec![Span::styled(
        "🤖 ".to_owned(),
        Style::default()
            .fg(role_color(Role::Primary))
            .add_modifier(Modifier::BOLD),
    )];
    let (mut body_spans, next) = md_line_spans_with_alert(body, in_code, alert_role);
    if let Some(edge) = alert_edge {
        apply_alert_edge(&mut body_spans, edge);
    }
    spans.append(&mut body_spans);
    (spans, next)
}

/// Live/summary projection fold limit. Static scrollback never uses this
/// presentation-only cap: users must be able to inspect every committed row.
#[cfg(test)]
pub(crate) fn fold_lines(text: &str, max: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max {
        return text.to_owned();
    }
    let hidden = lines.len() - max;
    let mut out = lines[..max].join("\n");
    out.push_str(&format!("\n… (+{hidden} lines folded)"));
    out
}

pub(crate) fn activity_role(kind: ActivityKind) -> Role {
    match kind {
        ActivityKind::Run => Role::Primary,
        ActivityKind::Plan => Role::Reasoning,
        ActivityKind::Waiting | ActivityKind::Approval => Role::Warn,
        ActivityKind::Takeover => Role::Primary,
        ActivityKind::Completed | ActivityKind::Conclusion => Role::Success,
        ActivityKind::Error => Role::Error,
        _ => Role::Info,
    }
}

#[cfg(test)]
pub(crate) fn reasoning_commit_text(
    text: &str,
    step: usize,
    elapsed_s: u64,
    tokens: usize,
) -> String {
    let meta = fmt_reasoning_meta(step, elapsed_s, tokens);
    let mut lines = text.lines();
    let Some(first) = lines.next() else {
        return format!("┊ {meta}");
    };
    let mut committed = format!("┊ {meta}{first}  [Ctrl+R history]");
    for line in lines {
        committed.push('\n');
        committed.push_str("│ ");
        committed.push_str(line);
    }
    committed
}

#[cfg(test)]
pub(crate) fn activity_commit_text(sequence: u64, kind: ActivityKind, text: &str) -> String {
    format!("⟦{} #{sequence}⟧ {text}  [Ctrl+T activity]", kind.tag())
}

/// Static commit projection stays with the other presentation renderers.
/// `app.rs` owns only CommitBlock orchestration and terminal insertion; these
/// helpers own Answer/Reasoning/Activity/Tool text, rails, and cell wrapping.
pub(crate) fn reasoning_commit_lines(
    text: &str,
    step: usize,
    elapsed_s: u64,
    tokens: usize,
    width: u16,
) -> Vec<Line<'static>> {
    let text = sanitize_display_text(text);
    let meta = fmt_reasoning_meta(step, elapsed_s, tokens);
    let base = Style::default()
        .fg(role_color(Role::Reasoning))
        .add_modifier(Modifier::DIM | Modifier::ITALIC);
    let meta_style = Style::default()
        .fg(role_color(Role::Label))
        .add_modifier(Modifier::DIM | Modifier::ITALIC);
    let hint_style = Style::default()
        .fg(role_color(Role::Muted))
        .add_modifier(Modifier::DIM | Modifier::ITALIC);
    let mut in_code = false;
    let mut alert_role = None;
    let source_lines = text.lines().collect::<Vec<_>>();
    let edges = alert_edges(source_lines.iter().copied());
    let last_index = source_lines.len().saturating_sub(1);
    let mut lines = vec![Line::default()];

    for (index, line) in source_lines.into_iter().enumerate() {
        let (mut body, next_code) = md_line_spans_with_alert(line, in_code, &mut alert_role);
        in_code = next_code;
        if let Some(edge) = edges.get(index).copied().flatten() {
            apply_alert_edge(&mut body, edge);
        }
        for span in &mut body {
            span.style = base.patch(span.style);
        }

        let mut spans = Vec::with_capacity(body.len() + 4);
        let rail = if index == 0 {
            "┊ "
        } else if index == last_index {
            "└ "
        } else {
            "│ "
        };
        spans.push(Span::styled(rail, base));
        if index == 0 {
            spans.push(Span::styled(meta.clone(), meta_style));
        }
        spans.extend(body);
        if index == 0 {
            spans.push(Span::styled("  [Ctrl+R history]", hint_style));
        }
        lines.push(Line::from(spans));
    }

    if lines.len() == 1 {
        lines.push(Line::from(vec![
            Span::styled("┊ ", base),
            Span::styled(meta, meta_style),
        ]));
    }

    wrap_commit_lines(lines, width)
}

pub(crate) fn activity_commit_lines(
    sequence: u64,
    kind: ActivityKind,
    text: &str,
    width: u16,
) -> Vec<Line<'static>> {
    let text = sanitize_display_text(text);
    let prefix = format!("⟦{} #{sequence}⟧ ", kind.tag());
    let hint = "  [Ctrl+T activity]";
    let source_lines = text.lines().collect::<Vec<_>>();
    let last_index = source_lines.len().saturating_sub(1);
    let first = source_lines.first().copied().unwrap_or("");
    let role = activity_role(kind);
    let tag_style = Style::default()
        .fg(role_color(role))
        .add_modifier(Modifier::BOLD);
    let body_style = Style::default()
        .fg(role_color(role))
        .add_modifier(Modifier::DIM);
    let hint_style = Style::default()
        .fg(role_color(Role::Muted))
        .add_modifier(Modifier::DIM);
    let mut lines = vec![Line::default()];
    lines.push(Line::from(vec![
        Span::styled(prefix, tag_style),
        Span::styled(first.to_owned(), body_style),
        Span::styled(hint, hint_style),
    ]));
    lines.extend(
        source_lines
            .into_iter()
            .enumerate()
            .skip(1)
            .map(|(index, line)| {
                let rail = if index == last_index { "└ " } else { "│ " };
                Line::from(vec![
                    Span::styled(rail.to_owned(), body_style),
                    Span::styled(line.to_owned(), body_style),
                ])
            }),
    );
    wrap_commit_lines(lines, width)
}

pub(crate) fn static_tool_lines(tool: &ToolBlock, width: u16) -> Vec<(String, Color)> {
    let lines = tool.collapsed_lines();
    lines
        .into_iter()
        .enumerate()
        .map(|(index, (text, color))| {
            if index == 0 {
                let prefix = if width >= 72 {
                    format!("◈ {} ", tool.phase_label())
                } else {
                    format!("{} ", tool.phase_short_label())
                };
                (format!("{prefix}{text}"), color)
            } else {
                (format!("  ┆ {text}"), color)
            }
        })
        .collect()
}

pub(crate) fn commit_lines(
    text: String,
    color: Color,
    markdown: bool,
    partial: bool,
    modifier: Modifier,
    width: u16,
) -> Vec<Line<'static>> {
    commit_lines_with_answer_metrics(text, color, markdown, partial, modifier, width, None)
}

pub(crate) fn commit_lines_with_answer_metrics(
    text: String,
    color: Color,
    markdown: bool,
    partial: bool,
    modifier: Modifier,
    width: u16,
    metrics: Option<PresentationMetrics>,
) -> Vec<Line<'static>> {
    let text = sanitize_display_text(&text);
    // Answer/Diff details are user-controlled review surfaces: keep the full
    // source text here and let the terminal viewport/wrap provide navigation.
    let mut lines: Vec<Line> = vec![Line::default()];
    if markdown {
        lines.extend(answer_commit_lines_with_status_and_metrics_at_width(
            &text, partial, metrics, width,
        ));
    } else {
        lines.extend(text.lines().map(|line| {
            Line::from(Span::styled(
                line.to_owned(),
                Style::default().fg(color).add_modifier(modifier),
            ))
        }));
    }
    wrap_commit_lines(lines, width)
}

pub(crate) fn colored_commit_lines(
    entries: Vec<(String, Color)>,
    width: u16,
) -> Vec<Line<'static>> {
    let rows = entries
        .into_iter()
        .flat_map(|(text, color)| {
            let text = sanitize_display_text(&text);
            text.lines()
                .map(move |line| (line.to_owned(), color))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut lines = vec![Line::default()];
    lines.extend(rows.into_iter().map(|(text, color)| {
        // Diff rows carry a decorative `┆` rail in static tool output.  Keep
        // the marker check because Success/Error use the same ANSI colors as
        // DiffAdd/DiffDel; ordinary green/red tool rows must not get a diff
        // background.
        let trimmed = text.trim_start();
        let is_diff_add = color == role_color(Role::DiffAdd) && diff_line_marker(trimmed, '+');
        let is_diff_del = color == role_color(Role::DiffDel) && diff_line_marker(trimmed, '-');
        let style = if is_diff_add {
            Style::default().fg(Color::Black).bg(color)
        } else if is_diff_del {
            Style::default().fg(Color::White).bg(color)
        } else {
            Style::default().fg(color)
        };
        Line::from(Span::styled(text, style))
    }));
    wrap_commit_lines(lines, width)
}

fn diff_line_marker(text: &str, marker: char) -> bool {
    let text = text.strip_prefix('┆').map(str::trim_start).unwrap_or(text);
    text.strip_prefix(marker)
        .is_some_and(|rest| rest.starts_with(' '))
}

#[derive(Clone, Copy)]
struct CommitSemanticPrefix {
    first: &'static str,
    continuation: &'static str,
}

fn commit_semantic_prefix(spans: &[Span<'static>]) -> Option<CommitSemanticPrefix> {
    let first = spans.first()?.content.as_ref();
    [
        ("\u{256d} ANSWER ", "\u{2502} "),
        ("\u{2502} ", "\u{2502} "),
        ("\u{256d} ", "\u{2502} "),
        ("\u{2514} ", "\u{2502} "),
        ("\u{257a} ", "\u{2502} "),
        ("\u{250a} ", "\u{2502} "),
        ("C ", "  \u{2506} "),
        ("O ", "  \u{2506} "),
        ("T ", "  \u{2506} "),
        ("\u{25c8} CALL ", "  \u{2506} "),
        ("\u{25c8} OUT ", "  \u{2506} "),
        ("\u{25c8} TOOL ", "  \u{2506} "),
        ("\u{25c8} ", "  \u{2506} "),
        ("  \u{2506} ", "  \u{2506} "),
    ]
    .into_iter()
    .find_map(|(prefix, continuation)| {
        first.starts_with(prefix).then_some(CommitSemanticPrefix {
            first: prefix,
            continuation,
        })
    })
}

fn split_commit_prefix(
    spans: Vec<Span<'static>>,
    prefix: &str,
) -> Option<(Vec<Span<'static>>, Vec<Span<'static>>)> {
    let joined = spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    if !joined.starts_with(prefix) {
        return None;
    }

    let mut prefix_spans = Vec::new();
    let mut body_spans = Vec::new();
    let mut remaining = prefix.len();
    for span in spans {
        let content = span.content.into_owned();
        if remaining == 0 {
            body_spans.push(Span::styled(content, span.style));
            continue;
        }
        let take = remaining.min(content.len());
        if !content.is_char_boundary(take) {
            return None;
        }
        let (head, tail) = content.split_at(take);
        if !head.is_empty() {
            prefix_spans.push(Span::styled(head.to_owned(), span.style));
        }
        if !tail.is_empty() {
            body_spans.push(Span::styled(tail.to_owned(), span.style));
        }
        remaining -= take;
    }
    (remaining == 0).then_some((prefix_spans, body_spans))
}

#[derive(Clone)]
struct CommitFragment {
    text: String,
    style: Style,
    cells: usize,
}

enum CommitBodyUnit {
    Word(Vec<CommitFragment>),
    Whitespace(Vec<CommitFragment>),
    Newline,
}

fn commit_body_units(body: Vec<Span<'static>>) -> Vec<CommitBodyUnit> {
    let mut units = Vec::new();
    let mut kind = None;
    let mut fragments = Vec::new();

    for span in body {
        let style = span.style;
        for grapheme in span.content.as_ref().graphemes(true) {
            if grapheme == "\n" {
                if let Some(kind) = kind.take() {
                    push_commit_unit(&mut units, &mut fragments, kind);
                }
                units.push(CommitBodyUnit::Newline);
                continue;
            }

            let whitespace = grapheme.chars().all(char::is_whitespace);
            if kind.is_some_and(|current| current != whitespace) {
                let previous = kind.take().expect("commit unit kind exists");
                push_commit_unit(&mut units, &mut fragments, previous);
            }
            kind = Some(whitespace);
            fragments.push(CommitFragment {
                text: grapheme.to_owned(),
                style,
                cells: str_cells(grapheme),
            });
        }
    }

    if let Some(kind) = kind {
        push_commit_unit(&mut units, &mut fragments, kind);
    }
    units
}

fn push_commit_unit(
    units: &mut Vec<CommitBodyUnit>,
    fragments: &mut Vec<CommitFragment>,
    whitespace: bool,
) {
    let chunk = std::mem::take(fragments);
    units.push(if whitespace {
        CommitBodyUnit::Whitespace(chunk)
    } else {
        CommitBodyUnit::Word(chunk)
    });
}

fn commit_fragments_cells(fragments: &[CommitFragment]) -> usize {
    fragments.iter().map(|fragment| fragment.cells).sum()
}

fn push_commit_continuation(
    rows: &mut Vec<Vec<Span<'static>>>,
    continuation: &str,
    prefix_style: Style,
) {
    rows.push(vec![Span::styled(continuation.to_owned(), prefix_style)]);
}

fn append_commit_fragments(
    rows: &mut Vec<Vec<Span<'static>>>,
    fragments: &[CommitFragment],
    row_cells: &mut usize,
    width: usize,
    continuation: &str,
    continuation_cells: usize,
    prefix_style: Style,
) {
    for fragment in fragments {
        if fragment.cells > width.saturating_sub(continuation_cells.max(1))
            && fragment.text.chars().count() > 1
        {
            for grapheme in fragment.text.graphemes(true) {
                let used = str_cells(grapheme);
                if *row_cells > 0 && row_cells.saturating_add(used) > width {
                    push_commit_continuation(rows, continuation, prefix_style);
                    *row_cells = continuation_cells;
                }
                rows.last_mut()
                    .expect("semantic commit wrap owns one row")
                    .push(Span::styled(grapheme.to_owned(), fragment.style));
                *row_cells = row_cells.saturating_add(used);
            }
            continue;
        }
        if *row_cells > 0 && row_cells.saturating_add(fragment.cells) > width {
            push_commit_continuation(rows, continuation, prefix_style);
            *row_cells = continuation_cells;
        }
        rows.last_mut()
            .expect("semantic commit wrap owns one row")
            .push(Span::styled(fragment.text.clone(), fragment.style));
        *row_cells = row_cells.saturating_add(fragment.cells);
    }
}

fn activity_commit_prefix(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("⟦")?;
    let close = rest.find("⟧ ")?;
    let header = &rest[..close];
    let (tag, sequence) = header.split_once(" #")?;
    if !matches!(
        tag,
        "SYS"
            | "PLAN"
            | "THK"
            | "ANS"
            | "TLS"
            | "CHK"
            | "SUM"
            | "WAIT"
            | "ASK"
            | "QUE"
            | "TAKE"
            | "DONE"
            | "ERR"
    ) || sequence.is_empty()
        || !sequence.chars().all(|character| character.is_ascii_digit())
    {
        return None;
    }
    let end = "⟦".len() + close + "⟧ ".len();
    text.get(..end)
}

fn wrap_semantic_commit_line_with_prefix(
    spans: Vec<Span<'static>>,
    first: &str,
    continuation: &str,
    width: u16,
) -> Option<Vec<Line<'static>>> {
    let (first_prefix, body) = split_commit_prefix(spans, first)?;
    let first_cells = str_cells(first);
    let continuation_cells = str_cells(continuation);
    let width = width as usize;
    if width <= first_cells || width <= continuation_cells {
        return None;
    }

    let prefix_style = first_prefix
        .first()
        .map(|span| span.style)
        .unwrap_or_default();
    let mut rows = vec![first_prefix];
    let mut row_cells = first_cells;
    let mut row_has_body = false;
    let mut pending_whitespace = Vec::new();
    for unit in commit_body_units(body) {
        match unit {
            CommitBodyUnit::Newline => {
                pending_whitespace.clear();
                push_commit_continuation(&mut rows, continuation, prefix_style);
                row_cells = continuation_cells;
                row_has_body = false;
            }
            CommitBodyUnit::Whitespace(fragments) => {
                pending_whitespace = fragments;
            }
            CommitBodyUnit::Word(fragments) => {
                let whitespace_cells = commit_fragments_cells(&pending_whitespace);
                let word_cells = commit_fragments_cells(&fragments);
                let would_overflow = row_cells
                    .saturating_add(whitespace_cells)
                    .saturating_add(word_cells)
                    > width;
                if row_has_body && would_overflow {
                    push_commit_continuation(&mut rows, continuation, prefix_style);
                    row_cells = continuation_cells;
                    pending_whitespace.clear();
                } else if !row_has_body && would_overflow {
                    pending_whitespace.clear();
                }
                append_commit_fragments(
                    &mut rows,
                    &pending_whitespace,
                    &mut row_cells,
                    width,
                    continuation,
                    continuation_cells,
                    prefix_style,
                );
                append_commit_fragments(
                    &mut rows,
                    &fragments,
                    &mut row_cells,
                    width,
                    continuation,
                    continuation_cells,
                    prefix_style,
                );
                pending_whitespace.clear();
                row_has_body = true;
            }
        }
    }
    Some(rows.into_iter().map(Line::from).collect())
}

fn wrap_semantic_commit_line(spans: Vec<Span<'static>>, width: u16) -> Option<Vec<Line<'static>>> {
    let prefix = commit_semantic_prefix(&spans)?;
    wrap_semantic_commit_line_with_prefix(spans, prefix.first, prefix.continuation, width)
}

fn process_bundle_wrap_prefix(text: &str) -> Option<&str> {
    const TITLE: &str = "§ ACTA · ";
    if text.starts_with(TITLE) {
        return Some(TITLE);
    }
    let rest = text.strip_prefix("  ")?;
    let (tag, _) = rest.split_once(" · ")?;
    if !matches!(
        tag,
        "SYS"
            | "RUN"
            | "PLAN"
            | "THK"
            | "ANS"
            | "TLS"
            | "CHK"
            | "SUM"
            | "WAIT"
            | "ASK"
            | "QUE"
            | "TAKE"
            | "DONE"
            | "ERR"
    ) {
        return None;
    }
    text.get(..2 + tag.len() + " · ".len())
}

fn wrap_commit_line(line: Line<'static>, width: u16) -> Vec<Line<'static>> {
    let spans = line.spans.into_iter().collect::<Vec<_>>();
    let activity_prefix = spans
        .first()
        .and_then(|span| activity_commit_prefix(span.content.as_ref()))
        .map(str::to_owned);
    if let Some(prefix) = activity_prefix.as_deref() {
        if let Some(wrapped) =
            wrap_semantic_commit_line_with_prefix(spans.clone(), prefix, "│ ", width)
        {
            return wrapped;
        }
    }
    let bundle_prefix = spans
        .first()
        .and_then(|span| process_bundle_wrap_prefix(span.content.as_ref()))
        .map(str::to_owned);
    if let Some(prefix) = bundle_prefix.as_deref() {
        if let Some(wrapped) =
            wrap_semantic_commit_line_with_prefix(spans.clone(), prefix, "│ ", width)
        {
            return wrapped;
        }
    }
    wrap_semantic_commit_line(spans.clone(), width).unwrap_or_else(|| {
        wrap_live_spans_greedy(spans, width)
            .into_iter()
            .map(Line::from)
            .collect()
    })
}

pub(crate) fn wrap_commit_lines(lines: Vec<Line<'static>>, width: u16) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .flat_map(|line| wrap_commit_line(line, width))
        .collect()
}

/// Monumental Roman RIDGECODE wordmark. The startup animation reveals this
/// compact, heavyweight inscription instead of the former mixed-case banner.
pub(crate) const SPLASH: &[&str] = &[
    r"██████▙ ███████ ██████▙ ▟█████▛ ███████ ▟██████ ▟█████▙ ██████▙ ███████",
    r" ██  ██   ███    ██  ▜█ ██       ██     ██      ██   ██  ██  ▜█  ██    ",
    r" ██  ██   ███    ██   █ ██  ▟██  ██ ▄   ██      ██   ██  ██   █  ██ ▄  ",
    r" █████▛   ███    ██   █ ██   ██  █████  ██      ██   ██  ██   █  █████ ",
    r" ██ ▜█    ███    ██  ▟█ ██  ▟██  ██     ██      ██   ██  ██  ▟█  ██    ",
    r"███  ██ ███████ ██████▛ ▜█████▛ ███████ ▜██████ ▜█████▛ ██████▛ ███████",
];
/// TUI hand-off sentinel; the standalone reference animation runs before TUI.
pub(crate) const SPLASH_TICKS: usize = 1;
/// banner 最大行宽(用于居中与折行守卫)。
#[cfg(test)]
pub(crate) const SPLASH_W: usize = 71;
pub(crate) const SPLASH_H: usize = 6;
pub(crate) const SPLASH_DURATION_SECS: f64 = 2.6;
pub(crate) const SPLASH_FPS: u32 = 60;

type SplashRgb = (i32, i32, i32);

fn splash_py_round(value: f64) -> i32 {
    // Python's round() uses ties-to-even; matching it keeps RGB transitions
    // frame-identical at the half-way points used by the reference script.
    let floor = value.floor();
    let fraction = value - floor;
    if fraction < 0.5 {
        floor as i32
    } else if fraction > 0.5 {
        floor as i32 + 1
    } else {
        let integer = floor as i64;
        if integer % 2 == 0 {
            integer as i32
        } else {
            integer as i32 + 1
        }
    }
}

fn splash_ease(value: f64) -> f64 {
    let t = value.clamp(0.0, 1.0);
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

fn splash_rgb(rgb: SplashRgb) -> String {
    format!("\x1b[38;2;{};{};{}m", rgb.0, rgb.1, rgb.2)
}

fn splash_base(x: usize, logo_w: usize, vertical: f64) -> SplashRgb {
    if x as f64 >= logo_w as f64 * 5.0 / 9.0 {
        let code_x = (x as f64 - logo_w as f64 * 5.0 / 9.0) / (logo_w as f64 * 4.0 / 9.0).max(1.0);
        (
            splash_py_round(112.0 - 72.0 * code_x + 24.0 * (1.0 - vertical)),
            splash_py_round(62.0 + 86.0 * code_x + 24.0 * (1.0 - vertical)),
            splash_py_round(196.0 + 48.0 * code_x),
        )
    } else {
        (
            splash_py_round(78.0 + 58.0 * (1.0 - vertical)),
            splash_py_round(58.0 + 82.0 * (1.0 - vertical)),
            splash_py_round(25.0 + 25.0 * (1.0 - vertical)),
        )
    }
}

fn splash_foreground_tone(
    base: SplashRgb,
    x: usize,
    logo_w: usize,
    glow: f64,
    alpha: f64,
) -> SplashRgb {
    let hi = if x as f64 >= logo_w as f64 * 5.0 / 9.0 {
        (225, 235, 255)
    } else {
        (205, 180, 92)
    };
    (
        splash_py_round((base.0 as f64 + (hi.0 - base.0) as f64 * glow) * alpha),
        splash_py_round((base.1 as f64 + (hi.1 - base.1) as f64 * glow) * alpha),
        splash_py_round((base.2 as f64 + (hi.2 - base.2) as f64 * glow) * alpha),
    )
}

struct SplashCanvas {
    grid: Vec<Vec<char>>,
    colors: Vec<Vec<Option<SplashRgb>>>,
    width: usize,
    height: usize,
    ox: isize,
    oy: isize,
    logo_w: usize,
}

fn splash_paint_reflection(canvas: &mut SplashCanvas, reflection: f64) {
    let reflected_rows = SPLASH_H.min(
        (canvas.height as isize - (canvas.oy + SPLASH_H as isize + 1))
            .max(0)
            .try_into()
            .unwrap_or(0),
    );
    for ry in 0..reflected_rows {
        let row_alpha = splash_ease((reflection * 1.45 - ry as f64 * 0.075).clamp(0.0, 1.0));
        if row_alpha <= 0.01 {
            continue;
        }
        let source_y = SPLASH_H - 1 - ry;
        let fade = (0.48 - 0.025 * ry as f64) * row_alpha;
        let left_lean = splash_py_round(ry as f64 * 0.75) as isize;
        for (x, ch) in SPLASH[source_y].chars().enumerate() {
            if ch == ' ' {
                continue;
            }
            let gx = canvas.ox + x as isize - left_lean;
            let gy = canvas.oy + SPLASH_H as isize + 1 + ry as isize;
            if gx < 0 || gy < 0 || gx >= canvas.width as isize || gy >= canvas.height as isize {
                continue;
            }
            let base = splash_base(x, canvas.logo_w, source_y as f64 / (SPLASH_H - 1) as f64);
            canvas.grid[gy as usize][gx as usize] = ch;
            canvas.colors[gy as usize][gx as usize] = Some((
                splash_py_round(base.0 as f64 * fade),
                splash_py_round(base.1 as f64 * fade),
                splash_py_round(base.2 as f64 * fade),
            ));
        }
    }
}

fn splash_paint_foreground_cell(
    canvas: &mut SplashCanvas,
    x: usize,
    y: usize,
    ch: char,
    local_alpha: f64,
    lift: isize,
    sweep: Option<f64>,
) {
    let gx = canvas.ox + x as isize;
    let gy = canvas.oy + y as isize + lift;
    if gx < 0 || gy < 0 || gx >= canvas.width as isize || gy >= canvas.height as isize {
        return;
    }
    let glow = if let Some(sweep) = sweep {
        (1.0 - (x as f64 - sweep).abs() / 11.0).max(0.0).powi(2)
    } else {
        0.0
    };
    let base = splash_base(x, canvas.logo_w, y as f64 / (SPLASH_H - 1) as f64);
    canvas.colors[gy as usize][gx as usize] = Some(splash_foreground_tone(
        base,
        x,
        canvas.logo_w,
        glow,
        local_alpha,
    ));
    canvas.grid[gy as usize][gx as usize] = ch;
}

fn splash_paint_foreground(canvas: &mut SplashCanvas, reveal: f64, sweep: f64, sweep_active: bool) {
    for (y, row) in SPLASH.iter().enumerate() {
        let row_delay = (SPLASH_H - 1 - y) as f64 * 0.018;
        let local_alpha = splash_ease(((reveal - row_delay) / 0.88).clamp(0.0, 1.0));
        if local_alpha <= 0.025 {
            continue;
        }
        let lift = splash_py_round((1.0 - local_alpha).powi(2) * 4.0) as isize;
        for (x, ch) in row.chars().enumerate() {
            if ch != ' ' {
                splash_paint_foreground_cell(
                    canvas,
                    x,
                    y,
                    ch,
                    local_alpha,
                    lift,
                    sweep_active.then_some(sweep),
                );
            }
        }
    }
}

fn splash_encode_rows(grid: &[Vec<char>], colors: &[Vec<Option<SplashRgb>>]) -> String {
    let height = grid.len();
    let width = grid.first().map_or(0, Vec::len);
    let mut lines = Vec::with_capacity(height);
    for row in 0..height {
        let mut line = String::new();
        let mut active = None;
        for column in 0..width {
            if colors[row][column] != active {
                let escape = colors[row][column]
                    .map(splash_rgb)
                    .unwrap_or_else(|| "\x1b[0m".to_string());
                line.push_str(&escape);
                active = colors[row][column];
            }
            line.push(grid[row][column]);
        }
        line.push_str("\x1b[0m");
        lines.push(line);
    }
    lines.join("\n")
}

/// Exact full-canvas frame port of `C:\code\wind\scripts\intro_block_preview.py`.
/// The live ratatui viewport must not clip or recolor this output.
pub(crate) fn splash_canvas(width: usize, height: usize, elapsed: f64, duration: f64) -> String {
    let width = width.max(1);
    let height = height.max(1);
    let logo_w = SPLASH
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0);
    let ox = (width as isize - logo_w as isize).div_euclid(2);
    let oy = (height as isize - SPLASH_H as isize).div_euclid(2);
    let t = (elapsed / duration).clamp(0.0, 1.0);

    let entry_end = 0.54;
    let sweep_end = 0.78;
    let reveal = splash_ease((t / entry_end).min(1.0));
    let sweep_progress = splash_ease(((t - entry_end) / (sweep_end - entry_end)).clamp(0.0, 1.0));
    let sweep = -12.0 + (logo_w as f64 + 24.0) * sweep_progress;
    let sweep_active = (entry_end..=sweep_end).contains(&t);
    let reflection = splash_ease(((t - entry_end) / (sweep_end - entry_end)).clamp(0.0, 1.0));
    let mut canvas = SplashCanvas {
        grid: vec![vec![' '; width]; height],
        colors: vec![vec![None; width]; height],
        width,
        height,
        ox,
        oy,
        logo_w,
    };
    splash_paint_reflection(&mut canvas, reflection);
    splash_paint_foreground(&mut canvas, reveal, sweep, sweep_active);
    splash_encode_rows(&canvas.grid, &canvas.colors)
}
