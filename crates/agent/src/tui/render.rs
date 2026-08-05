use std::borrow::Cow;

use super::*;
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

/// 粘贴净化(iter-24):CRLF/CR 归一 LF,滤除其余控制字符(留 \n \t),防转义序列注入输入框。
pub(crate) fn sanitize_paste(s: &str) -> String {
    s.replace("\r\n", "\n")
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

/// 语义化色角色(iter-28):界面色一律经角色取 **ANSI 16 具名色**,零 RGB 硬编码 ——
/// 尊重用户终端主题(浅色/高对比/透明背景下不悲剧)。
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

pub(crate) fn role_color(r: Role) -> Color {
    match r {
        // Calm base palette: neutral text carries the screen; cyan is the
        // single product/focus accent. Warning colors remain exceptional.
        Role::Primary => Color::Cyan,
        Role::Command => Color::White,
        Role::Answer => Color::White,
        Role::Reasoning => Color::LightBlue,
        Role::Info => Color::Gray,
        Role::Success => Color::Green,
        Role::Error => Color::Red,
        Role::Warn => Color::Yellow,
        Role::Border => Color::DarkGray,
        Role::Muted => Color::DarkGray,
        Role::Metric => Color::White,
        Role::Label => Color::DarkGray,
        Role::DiffAdd => Color::Green,
        Role::DiffDel => Color::Red,
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
        .bg(Color::DarkGray)
        .add_modifier(Modifier::BOLD)
}

/// 行内 md 扫描:`` `code` ``(Warn 色)与 `**bold**`(加粗);未闭合记号按字面。纯函数。
pub(crate) fn inline_md_spans(text: &str) -> Vec<Span<'static>> {
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
        if markdown_alert_role(alert_body(lines[index])).is_none() {
            index += 1;
            continue;
        }
        let mut end = index;
        while end + 1 < lines.len() && is_alert_continuation(lines[end + 1]) {
            end += 1;
        }
        if end == index {
            edges[index] = Some(AlertEdge::Single);
        } else {
            edges[index] = Some(AlertEdge::Top);
            for (offset, edge) in edges[index + 1..=end].iter_mut().enumerate() {
                *edge = Some(if index + 1 + offset == end {
                    AlertEdge::Bottom
                } else {
                    AlertEdge::Middle
                });
            }
        }
        index = end + 1;
    }
    edges
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
        let previous_is_space = text[..index]
            .chars()
            .next_back()
            .is_none_or(char::is_whitespace);

        if rest.starts_with("//") || (c == '#' && !rest.starts_with("#[") && previous_is_space) {
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
            let end = text[index..]
                .char_indices()
                .take_while(|(_, value)| {
                    value.is_ascii_alphanumeric() || matches!(*value, '_' | '.')
                })
                .last()
                .map(|(offset, value)| index + offset + value.len_utf8())
                .unwrap_or(index + c.len_utf8());
            push_code_span(&mut spans, &text[index..end], Role::Warn);
            index = end;
            continue;
        }

        if code_identifier_start(c) {
            let end = text[index..]
                .char_indices()
                .take_while(|(_, value)| code_identifier_continue(*value))
                .last()
                .map(|(offset, value)| index + offset + value.len_utf8())
                .unwrap_or(index + c.len_utf8());
            let token = &text[index..end];
            if let Some(role) = code_token_role(token) {
                flush_code_plain(&mut spans, &mut plain);
                push_code_span(&mut spans, token, role);
            } else {
                plain.push_str(token);
            }
            index = end;
            continue;
        }

        plain.push(c);
        index += c.len_utf8();
    }
    flush_code_plain(&mut spans, &mut plain);
    spans
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
pub(crate) fn markdown_lines(text: &str) -> Vec<Line<'static>> {
    let mut in_code = false;
    let mut alert_role = None;
    let source_lines = text.lines().collect::<Vec<_>>();
    let edges = alert_edges(source_lines.iter().copied());
    source_lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let (spans, next) = answer_line_spans(
                line,
                in_code,
                &mut alert_role,
                edges.get(index).copied().flatten(),
            );
            in_code = next;
            Line::from(spans)
        })
        .collect()
}

/// Stable presentation rail for an answer that has left the live viewport.
/// The live renderer already exposes the same semantic channel; keeping this
/// prefix in committed scrollback prevents the final answer from becoming
/// indistinguishable from an untyped Markdown note.
fn answer_commit_rail(index: usize) -> &'static str {
    if index == 0 {
        "╭ ANSWER "
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
    text.lines()
        .enumerate()
        .map(|(index, line)| {
            format!(
                "{}{line}{}",
                answer_commit_rail(index),
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

pub(crate) fn answer_commit_lines_with_status_and_metrics(
    text: &str,
    partial: bool,
    metrics: Option<PresentationMetrics>,
) -> Vec<Line<'static>> {
    markdown_lines(text)
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let rail = answer_commit_rail(index);
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

/// 呈现层折叠上限(iter-28):静态提交前超限留头 + 尾标,历史不刷屏。
/// (内核 `bound_observation` 已在源头有界,此为第二道收敛。)
pub(crate) const FOLD_MAX: usize = 20;

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

/// 启动 banner(iter-28):ASCII 安全字符(单格宽),经 `splash_frame` 列渐显 ≈1s。
pub(crate) const SPLASH: &[&str] = &[
    r"  ____  _     _            ____          _      ",
    r" |  _ \(_) __| | __ _  ___/ ___|___   __| | ___ ",
    r" | |_) | |/ _` |/ _` |/ _ \ |   / _ \ / _` |/ _ \",
    r" |  _ <| | (_| | (_| |  __/ |__| (_) | (_| |  __/",
    r" |_| \_\_|\__,_|\__, |\___|\____\___/ \__,_|\___|",
    r"                |___/                            ",
];
pub(crate) const SPLASH_TICKS: usize = 14;
/// banner 最大行宽(用于居中与折行守卫)。
pub(crate) const SPLASH_W: usize = 48;

/// 帧序列纯函数:第 `tick`/`total` 帧显示前 (maxw·tick/total) 列。首帧零字形,末帧全幅,单调渐显。
pub(crate) fn splash_frame(tick: usize, total: usize) -> String {
    let maxw = SPLASH.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let cols = maxw * tick / total.max(1);
    SPLASH
        .iter()
        .map(|l| l.chars().take(cols).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// banner 在 `width` 内水平居中的左空白列数(纯函数)。
pub(crate) fn splash_pad(width: usize) -> usize {
    width.saturating_sub(SPLASH_W) / 2
}

/// 每行前置 `pad` 空格(动画帧居中用)。
pub(crate) fn indent(text: &str, pad: usize) -> String {
    let p = " ".repeat(pad);
    text.lines()
        .map(|l| format!("{p}{l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 落定 banner 块(iter-36,纯函数,防「标识乱了」):
/// 终端宽 ≥ banner 宽 → **居中** ASCII 艺术字(逐行 `trim_end` 去尾空格,故每行 ≤ width 不折行)+ 英文 tagline;
/// 窄于 banner 宽 → 退化紧凑单行标题(极窄也不折行)。
pub(crate) fn splash_block(width: usize) -> Vec<String> {
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
