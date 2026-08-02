use super::*;

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
    for c in text.chars() {
        if matches!(c, '\n' | '\r') {
            break;
        }
        let used = char_cells(c);
        if cells + used > limit {
            break;
        }
        out.push(c);
        cells += used;
    }
    out.push('…');
    out
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
    for c in line.chars() {
        let cw = char_cells(c);
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
    for (i, c) in buffer.chars().enumerate() {
        if i == cursor {
            crow = lines.len() as u16;
            ccol = cells as u16;
            recorded = true;
        }
        if c == '\n' {
            lines.push(std::mem::take(&mut line));
            cells = 0;
        } else {
            let cw = char_cells(c);
            if cells + cw > w && cells > 0 {
                lines.push(std::mem::take(&mut line));
                cells = 0;
            }
            line.push(c);
            cells += cw;
        }
    }
    if !recorded {
        crow = lines.len() as u16; // 光标在末尾
        ccol = cells as u16;
    }
    lines.push(line);
    (lines, crow, ccol)
}

/// 静态提交一段文本需占的终端行数(供 `insert_before`)。
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

pub(crate) fn role_color(r: Role) -> Color {
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
/// ``` 围栏切态(围栏行 Border 色)、块内 Muted、`#` 标题加粗 Primary、引用/列表有结构侧栏、余走行内扫描。
pub(crate) fn md_line_spans(line: &str, in_code: bool) -> (Vec<Span<'static>>, bool) {
    let trimmed = line.trim_start();
    if trimmed.starts_with("```") {
        return (
            vec![Span::styled(
                line.to_owned(),
                Style::default().fg(role_color(Role::Border)),
            )],
            next_fence_state(trimmed, in_code),
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
    if let Some(spans) = markdown_structure_spans(line) {
        return (spans, false);
    }
    (inline_md_spans(line), false)
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
    text.lines()
        .map(|line| {
            let (spans, next) = answer_line_spans(line, in_code);
            in_code = next;
            Line::from(spans)
        })
        .collect()
}

fn answer_line_spans(line: &str, in_code: bool) -> (Vec<Span<'static>>, bool) {
    let Some(body) = line.strip_prefix("🤖 ") else {
        return md_line_spans(line, in_code);
    };
    let mut spans = vec![Span::styled(
        "🤖 ".to_owned(),
        Style::default()
            .fg(role_color(Role::Primary))
            .add_modifier(Modifier::BOLD),
    )];
    let (mut body_spans, next) = md_line_spans(body, in_code);
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
