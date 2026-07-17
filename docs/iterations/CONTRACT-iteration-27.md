# CONTRACT — Iteration 27(UI 三部曲 II:高级输入 —— CSI u 多行 / 历史召回 / 补全浮窗)

## 目标

- **P0 CSI u**:enter 时 best-effort `PushKeyboardEnhancementFlags(DISAMBIGUATE_ESCAPE_CODES)`,Drop `Pop`;Shift+Enter / Alt+Enter / Ctrl+J → 插入换行(多行任务书写)。
- **P0 InputState 状态机**:`{buffer, cursor, history, hist_idx, draft}`;光标编辑(Left/Right/Home/End/行内插删/多行 Up/Down 列钳位);**首逻辑行 Up = 历史召回**(召回前存 draft,Down 到底还原 draft);提交入 history;粘贴并入 `insert_str`;渲染真光标(`Frame::set_cursor_position`,逻辑行列)。
- **P1 补全浮窗**:Tab 触发 —— 行首 `/` 词补斜杠命令(静态表),`@` 词补路径(单层 `read_dir`);`starts_with` 过滤;浮窗态 ↑↓/Tab 选、Enter 应用(替换当前词)、Esc 关;Live 视口内输入框上方渲染。
- **键位模态优先级**:审批模态 > 补全浮窗 > 输入编辑;路由收进纯函数(矩阵可测)。
- **减法**:`input_action` Up/Down 空桩删除;`ui.input: String` 字段由 InputState 取代。

## 边界(不做)

- 不引 tui-textarea 等新依赖;不做折行内光标微移(逻辑行粒度);不做递归路径搜索/异步补全;不做语法高亮(iter-28);不做 Esc+Enter 序列态(Alt+Enter 已覆盖);不动 headless/引擎。

## 确定性验收信号

1. `cargo test --workspace` exit 0,新增覆盖:
   - 路由矩阵:Shift+Enter/Alt+Enter/Ctrl+J → NewLine;裸 Enter(idle)→ Submit、(busy)→ Ignore;浮窗态 ↑↓/Tab/Enter/Esc 归浮窗;审批态不受影响(回归)。
   - InputState:光标处插删、Left/Right/Home/End、多行 Up/Down 列钳位、首行 Up 召回 + draft 存还、提交入 history 清 buffer。
   - 补全:`current_word` 提取(行首 `/`、任意处 `@`)、`filter_prefix` 稳序、`apply` 替换当前词保留前后文。
2. fmt + clippy `-D warnings` 净;被删符号零残留。

## 停机条件

- 只碰 `crates/agent/src/tui.rs` + docs;验收连续 2 次不过 → 熔断报告。
