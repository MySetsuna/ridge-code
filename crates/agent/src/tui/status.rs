use super::*;

/// 上下文窗口人读化:200000 → "200K",1048576 → "1.0M"(纯函数)。
pub(crate) fn fmt_ctx(n: u64) -> String {
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
pub(crate) const DEFAULT_CTX_WINDOW: u64 = 200_000;
/// 输入框下方自定义状态条内置默认模板。config `status_bar` 留空时用它。
pub(crate) const DEFAULT_STATUS_BAR: &str = " {provider} · {model} · ctx {ctx} · {tokens} tok ";

/// 实时 token 速率(tok/s,纯函数):elapsed 为 0 → 0,防除零。
pub(crate) fn token_rate(tokens: usize, elapsed_ms: u128) -> u64 {
    (tokens as u128 * 1000).checked_div(elapsed_ms).unwrap_or(0) as u64
}

/// 上下文占用百分比(纯函数):window 为 0 → 0;上限 100(压缩前估算,超窗即封顶)。
pub(crate) fn ctx_percent(used: usize, window: usize) -> u16 {
    (used * 100).checked_div(window).unwrap_or(0).min(100) as u16
}

/// 忙碌粘条文案(需求 6,纯函数):运行态 · 读秒 · token 消耗 · 速率 · 任务进度 · 待跑队列。
/// todo 空则省略进度段;`queued>0` 追加 ` · ⏳N`(iter-33)。计时/计量全由入参给定 —— 零 wall-clock,可纯测。
pub(crate) fn fmt_busy_bar(
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
pub(crate) struct StatusVars {
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) ctx: String,
    pub(crate) tokens: String,
    pub(crate) cwd: String,
}

/// 底栏模板渲染(需求 3,纯函数):替换 `{provider}{model}{ctx}{tokens}{cwd}`,
/// 未知占位原样保留(不吞字符,便于用户排错)。
pub(crate) fn render_status_template(tmpl: &str, v: &StatusVars) -> String {
    tmpl.replace("{provider}", &v.provider)
        .replace("{model}", &v.model)
        .replace("{ctx}", &v.ctx)
        .replace("{tokens}", &v.tokens)
        .replace("{cwd}", &v.cwd)
}

/// 当前工作目录末段名(状态栏用),取不到 → 空串。
pub(crate) fn cwd_name() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|x| x.to_string_lossy().to_string()))
        .unwrap_or_default()
}

/// 一帧的实时体征(iter-31):由主环据 `Instant`/token 计量算好后传入 `draw`,
/// draw 只消费数值 —— 计时逻辑不入 draw,便于纯测各格式化函数。
pub(crate) struct Vitals {
    pub(crate) elapsed_s: u64,
    pub(crate) task_tokens: usize,
    pub(crate) rate: u64,
    /// 当前 history 估算 token(ctx% 分子)。
    pub(crate) ctx_used: usize,
    /// 待跑排队条数(iter-33),忙碌粘条显 ⏳N。
    pub(crate) queued: usize,
}
