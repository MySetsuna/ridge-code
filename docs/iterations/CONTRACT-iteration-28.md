# CONTRACT — Iteration 28(UI 三部曲 III:视觉与反馈)

## 目标

- **P0 色角色层**:`Role` enum + `role_color`(ANSI 16 具名色,零 `Color::Rgb`);event_color/状态行/边框/浮窗高亮/模态收口到角色。
- **P0 md 轻渲染(提交时)**:`md_line_spans(line, in_code)` —— ``` 围栏切态、块内 Muted、`#` 粗体 Primary、行内 `` `code` ``(Warn)/`**bold**`(BOLD),未闭合按字面;`flush_commits` 对 `🤖` 终答走 md,余保持单色。流式尾巴**不** md。
- **P0 摘要折叠**:`fold_lines(text, FOLD_MAX=20)` 于 flush 前应用(头 20 行 + `… (+N 行已折叠)`)。
- **P1 启动帧序列**:`splash_frame(tick, total)` 列渐显 banner,tick 驱动 ≈1s,末帧入历史。
- **P1 流式游标**:busy 时流尾缀 `█`(Primary 色,frame 奇偶 BOLD/DIM 呼吸)。

## 边界(不做)

- 不引 md 解析库;不做表格/嵌套列表/HTML/Mermaid;不做 24-bit RGB;不做 `/view` 调阅与 Live 折叠切换;不自写 wcwidth;不动引擎/agent/headless。

## 确定性验收信号

1. `cargo test --workspace` exit 0,新增覆盖:`role_color` 映射;md 围栏切态/块内色/标题/行内 code/未闭合字面;`fold_lines` 限内不动、超限头保留 + `+N` 标;`splash_frame` 首帧无字形、末帧全幅、宽度单调不减。
2. 结构核(grep 人工):tui.rs 零 `Color::Rgb`。
3. fmt + clippy `-D warnings` 净。

## 停机条件

只碰 `crates/agent/src/tui.rs` + docs;验收连续 2 次不过 → 熔断报告。
