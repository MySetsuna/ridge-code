use super::*;

/// 要不要画这一帧:有状态变更(dirty)或 busy(spinner 需动)才画;空闲零重绘(iter-23)。
pub(crate) fn should_draw(dirty: bool, busy: bool) -> bool {
    dirty || busy
}

/// 单字符终端单元格宽度(wcwidth 口径):CJK/emoji=2、控制/零宽=0、常规=1(iter-30)。
pub(crate) fn char_cells(c: char) -> usize {
    unicode_width::UnicodeWidthChar::width(c).unwrap_or(0)
}

/// 字符串显示单元格宽度(iter-30):替代 `.chars().count()`,CJK/emoji 按实占计。
pub(crate) fn str_cells(s: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(s)
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

/// 动态输入框高度(iter-24):按内容折行数伸缩,clamp 在 [min,max](计入上下边框 2 行)。
pub(crate) fn input_height(content: &str, width: u16, min: u16, max: u16) -> u16 {
    (wrapped_rows(content, width).min(u16::MAX as usize) as u16)
        .saturating_add(2)
        .clamp(min, max)
}

/// 流式尾巴:Live 视口只显示正在生成文本的最后 `k` 行(前面的行等 Superstep 后整段历史化)。
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

/// 行级 md 轻渲染(iter-28,**只在静态提交时染** —— 样式定型才历史化):
/// ``` 围栏切态(围栏行 Border 色)、块内 Muted、`#` 标题加粗 Primary、余走行内扫描。
pub(crate) fn md_line_spans(line: &str, in_code: bool) -> (Vec<Span<'static>>, bool) {
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
