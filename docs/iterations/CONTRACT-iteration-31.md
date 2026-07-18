# CONTRACT · iteration-31 —— 视觉与状态双栏(输出分隔 / ctx% / 忙碌粘条)

> maker = 用户需求原文第 3、5、6 条。checker = 本文正确性门禁。计时特性一律纯格式化函数验收,**零 wall-clock / PTY / 进程信号**断言。

## 缺口(代码索引核实)

`tui.rs::draw` 现为三段布局(`Length(1)` 顶状态行 / `Min(1)` 输出尾 / `Length(input_rows)` 输入框):
- **需求 5**:`flush_commits` 逐块 `insert_before`,块与块**零间隔**贴排 —— 用户命令回显、事件、终答、todo 快照、结果摘要连成一片,难分类型。
- **需求 3**:顶状态行样式平(`bg DarkGray`),**无 ctx% 占用**;**无输入框下方的用户自定义状态条**。
- **需求 6**:`ui.busy`/`ui.phase` 仅驱动顶行 spinner 文案,**无输入框上方的粘性忙碌栏**(缺:规划任务进度 / 运行态 / 读秒 / 实时 token 速率与消耗)。`session_tokens` 仅在 `done` 落一次,**无流式实时计量**。

## 目标

1. **输出块间隔**(需求 5):`flush_commits` 每块前置一空白分隔行,连续块视觉分栏。
2. **ctx% + 自定义底栏**(需求 3):顶行加 `ctx N%`(当前 history 估算 tok / 上下文窗口);输入框**下方**加一行用户可配置状态条(`config.status_bar` 模板串,`{provider}{model}{ctx}{tokens}{turns}{cwd}` 占位)。
3. **忙碌粘条**(需求 6):`ui.busy` 时输入框**上方**多一粘性行,显示 `phase · 读秒 · tok · tok/s · todo d/n`;流式 token 实时累计计量(`est_tokens` 估算),任务起止各重置一次。

## 设计(最小面)

**纯函数(全部单测,零计时/PTY):**
- `token_rate(tokens: usize, elapsed_ms: u128) -> u64`:`elapsed_ms==0 → 0`,否则 `tokens*1000/elapsed_ms`。
- `ctx_percent(used: usize, window: usize) -> u16`:`window==0 → 0`,否则 `min(100, used*100/window)`。
- `fmt_busy_bar(phase, todos, elapsed_s, tokens, rate) -> String`:`"⚡ {phase} · ⏱ {s}s · {tok} tok · {rate} tok/s"`,todo 非空追加 ` · todo d/n`。
- `render_status_template(tmpl, vars) -> String`:`{key}` 替换 `provider/model/ctx/tokens/turns/cwd`,未知占位原样保留。

**wiring(布局/计量,靠门禁绿 + 既有 `draw_only_when_dirty_or_busy` 覆盖):**
- `est_tokens` 提升 `pub`(lib.rs;显示层复用同一估算口径,不另造)。`main.rs` 导入。
- `Config.status_bar: Option<String>`(serde default None)。`ReplMeta` 加 `status_bar: String`(解析默认或 config)、`ctx_window: u64`(默认 `DEFAULT_CTX_WINDOW=200_000`,`/models` 命中当前模型时缓存其真实窗口)。
- `draw` 布局按 `busy` 条件插入忙碌行;底栏恒在。顶行加 ctx%。
- `run` 主环:`task_started: Option<Instant>`(Submit 置、done/中断清)、`ui.stream_tokens`(token_rx 累计、Submit 清 0);draw 前算 `elapsed_ms`/`rate` 传入。`Instant` 属 app 运行时,非脚本,允许。

**不改**:逻辑光标语义、流式尾巴机制、审批模态、补全浮窗。

## 边界(不做)

- 真·倒计时(需已知总时长,无从得)—— 只做**读秒**(正计秒表),诚实标注。
- 每块左侧色条 gutter / 分隔线花纹 —— 色已区分,仅补间隔;超范围留后续。
- token 速率精确到 provider 计费口径 —— 用 `est_tokens` 估算即可,标注为估算。
- 模型选择器浮窗、`@` 递归补全 —— 后续轮。

## 确定性验收信号

门禁 `cargo test --workspace && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` 全 **exit 0**。新增测试:
- `token_rate`:`(0,0)→0`、`(100,1000)→100`、`(50,2000)→25`。
- `ctx_percent`:`(0,x)→0`、`(6000,200000)→3`、`(999999,100)→100`、`(x,0)→0`。
- `fmt_busy_bar`:空 todo 无 todo 段、含 todo 段格式、读秒/速率插值。
- `render_status_template`:占位替换、未知占位原样、无占位原样。
- `est_tokens_is_public`(冒烟:跨 crate 可见)。

## 停机

单轮;连续 2 轮验收不过 → 报告。价值门禁不适用(用户明确需求,非 NLM 臆造)。
