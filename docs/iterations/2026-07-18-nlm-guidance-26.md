# NotebookLM 指导归档 + 对抗评审(iter-26,UI 三部曲第一刀)

> 用户新指令:iter-26/27/28 主方向 = 界面与交互重构(UX/性能/优雅/带点炫酷)—— 价值门禁按此重校准:**用户点名的美 = 实质价值**;仍拒无谓花哨。
> NLM 三刀路线:26 主屏内联 REPL + 静态提交(范式转向)→ 27 CSI u + 补全浮窗 → 28 Markdown 渲染 + 折叠 + 启动动效。依赖顺序合理,采纳为总路线。

## 对抗评审裁决(iter-26)

### ✅ 采纳(P0):主屏内联模式 + 静态提交 —— 但用 ratatui 原生机制,不手搓
- **价值门禁过**:内联恢复终端原生滚动/选取/全局搜索(`669602c2` 引谷歌备用屏 TUI 遭抵制回滚实例,论据真);历次押后仅因体量,今用户明令 UX 主向,正当其时。
- **❌ 驳回 NLM 的手写 `StaticCommitter`(裸 `write!` + 自管光标)**:重造轮子 —— ratatui 0.29 **原生** `Viewport::Inline(h)` + `Terminal::insert_before`(把行插入内联视口上方、进终端 scrollback)正是「段落静态提交」的官方实现:行一经 insert_before 即成原生历史,不再参与差分重绘。少数十行、零逃逸序列工程、抗终端差异。
- **落地形态**:`TerminalGuard` 去 Enter/LeaveAlternateScreen(保 raw mode + BPM);`Terminal::with_options(Viewport::Inline(LIVE_HEIGHT))`;`Ui.note` 改压 `commits` 队列,主环 drain → `insert_before` 历史化;Live 视口只渲染:状态行 + 流式尾巴 + 输入框(+审批模态覆层)。

### ✅ 采纳(减法,NLM 主动列出且成立):内联落地即删旧渲染栈
- 删 `EnterAlternateScreen`/`LeaveAlternateScreen`;删 `Ui.log` 环形缓冲 + `LOG_CAP` + `tail_window` + `output_lines`(原生 scrollback 取代自绘滚动);删输出/TODO/工具三面板(TODO 收进状态行计数 + 变更时提交清单快照入历史);删主输入区自定义滚动键(终端原生滚轮/滚动条接管;审批模态内滚动**保留**)。
- 附带修正:`input_height` 折行计数抽 `wrapped_rows` 纯函数,与新的 `commit_height`(insert_before 高度计算)共用。

### ❌ 驳回:NLM 验收信号 2/3
- 「验证输出流不含 \x1b[2J」→ 需捕获真实 PTY 输出流,环境断言;改结构性证据(源码零 `Clear`/`AlternateScreen` 引用,grep 人工核)+ 纯函数测试。
- 「manifest.json 记录所有行物理同步」→ manifest 是 run 审计物,与渲染无涉,概念错位。
- 「输入框高度参考光标 Y 轴」→ Inline 视口下 ratatui 自管光标,无此需要。

### 确定性验收(替代):
`wrapped_rows`/`commit_height`/`stream_tail`(Live 区尾 K 行)纯函数测试;commits 队列 drain 有序且清空;`input_action` 键位表更新(主输入 Up/Down 归 Ignore,History 召回留 27);审批模态滚动回归测保留。
