# CONTRACT — Iteration 26(UI 三部曲 I:主屏内联 REPL + 静态提交)

## 目标

- **P0 内联视口**:`Viewport::Inline(LIVE_HEIGHT)` 替代备用屏;`TerminalGuard` 只管 raw mode + BPM。
- **P0 静态提交**:`Ui.note` → `commits` 队列;主环 drain 经 `Terminal::insert_before` 历史化(高度由 `commit_height(text,width)` 纯函数算);行入 scrollback 后永不重绘。
- **P0 Live 视口布局**:状态行(1,含 provider/model/tokens/cwd/spinner/todo 计数)+ 流式尾巴(`stream_tail` 尾 K 行)+ 输入框(`input_height` 3..=8);审批模态覆整个视口(滚动保留)。
- **减法(同权)**:删 AlternateScreen 进出、`Ui.log`/`LOG_CAP`/`tail_window`/`output_lines`、输出/TODO/工具三面板、主输入自定义滚动(`InputAction::Scroll` 移除,Up/Down 暂 Ignore);todos 变更时提交清单快照入历史。
- 折行计数抽 `wrapped_rows(text,width)`,`input_height`/`commit_height` 共用。

## 边界(不做)

- 不做 CSI u/历史召回/补全(iter-27);不做 markdown 样式/折叠/启动动画(iter-28);不开 `EnableMouseCapture`(原生选取神圣);不动 headless/引擎/agent 图。

## 确定性验收信号

1. `cargo test --workspace` exit 0;新增/更新覆盖:`wrapped_rows`(空/折行/多行/宽 0)、`commit_height`、`stream_tail`(少于 K 全量、多于 K 取尾)、commits 队列 drain 有序清空、`input_action` 新键位表(Up/Down/PageUp/PageDown → Ignore)、审批模态滚动回归保留。
2. 被删符号零残留:源码 grep 无 `AlternateScreen`/`tail_window`/`LOG_CAP`(人工核)。
3. fmt + clippy `-D warnings` 净。

## 停机条件

- 只碰 `crates/agent/src/tui.rs` + docs;验收连续 2 次不过 → 熔断报告。
