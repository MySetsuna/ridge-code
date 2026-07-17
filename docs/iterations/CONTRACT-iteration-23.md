# CONTRACT — Iteration 23(TUI 专轨第二刀:事件驱动环 + 有界日志)

## 目标(经对抗评审后)

- **P0 事件驱动主环**:`tui.rs::run` 弃 `event::poll(50ms)` 固定轮询。阻塞读线程转发键盘事件入 tokio mpsc;主环 `tokio::select!` 多路复用 {键盘, token 流, StreamEvent, 审批请求, 任务完成, tick};**dirty 标记**——仅状态变更或 busy(spinner)时 `terminal.draw`,空闲零重绘、零轮询。
- **P0 纯决策函数抽取**(续 iter-22 `approval_action` 模式):`input_action(key, busy) -> InputAction`(字符/退格/滚动/提交/中断/忽略)+ `should_draw(dirty, busy)`。主环按其返回值执行副作用。
- **P1 有界日志 + 视口尾窗**:`Ui.log` 改 `VecDeque` + `LOG_CAP` 环形淘汰;纯函数 `tail_window(len, rows, scroll)` 算可见逻辑行区间,`output_lines` 只构建窗口内行(O(K+scroll)/帧)。

## 边界(不做)

- 不做主屏幕内联 REPL / 静态提交(押后,见 guidance-23)。
- 不做 CSI u / bracketed paste / 输入高度 / 补全浮窗。
- 不引新依赖(不开 crossterm `event-stream` feature,不引滚动 widget 库)。
- 不动 agent 图 / 引擎 / Approver 语义(审批**应答**通道保持 std sync_channel)。

## 确定性验收信号(cargo 可判,零计时抖动)

1. `cargo test --workspace` exit 0,且新增测试覆盖:
   - `input_action`:busy 时 Enter 不提交;Ctrl-C → Interrupt;字符/退格/滚动键位路由;非 Press 忽略。
   - `should_draw`:dirty ∨ busy 才画。
   - `tail_window`:len<rows 全量;scroll=0 取尾 rows 行;scroll 回滚窗口正确;越界饱和不 panic。
   - 日志环形:push 超 LOG_CAP 后 len == LOG_CAP,留存的是**最新**条目。
2. `cargo fmt --all --check` + `cargo clippy --workspace --all-targets -- -D warnings` 干净。
3. 源码不再含 `event::poll`(轮询根除的结构性证据,由 grep 人工核,不入测试)。

## 停机条件

- 本轮只改 `crates/agent/src/tui.rs`(必要时 ARCHITECTURE.md/docs);触碰引擎或 agent 图即越界回退。
- 验收连续 2 次不过 → 停,报告熔断。
