# NotebookLM 指导归档 + 对抗评审(iter-23)

> 来源:notebook「手搓agent」,conversation `68791fb7`。NLM 是计划 maker 非裁判,下经 checker(引用支撑 / 第一性原理 / 当前代码现实)逐条裁决。

## NLM 初稿要点

- **P0 异步事件循环「脑手解耦」**:弃固定轮询,`tokio::select!` 监听 EventStream + mpsc;引 TUI 性能来源(`d7831e30`)与 Rebuild Plan(`669602c2`)。
- **P1 主屏幕内联 REPL + 段落静态提交**:弃备用屏,静态行直写终端历史,Live Window 只渲染尾 1-2 行。
- **P1 虚拟化滚动缓冲**:`VecDeque` 环形有界 + 视口切片,渲染 O(N)→O(K)。
- 验收信号(NLM 版):CPU<1%、10 万行注入后内存 O(1) 饱和、单帧 <16ms、抓 `\x1b[?2026h/l` 同步序列。
- 边界:不做容器化沙箱 / Mermaid / 多租户。
- 差集清单:异步解耦 ❌、内联 REPL ❌、CSI u/BPM ❌、GEPA 自改进 ❌、分支探索 ❌;BSP 引擎 ✅、MCP ✅、确定性 verify ✅。

## 对抗评审裁决

### ✅ 采纳(P0,修形):事件驱动环替换 50ms 轮询
- **现实核验为真**:`tui.rs::run` 主环 `event::poll(Duration::from_millis(50))` + 五路 `try_recv` 排空 + **每圈无条件 `terminal.draw`** —— 空闲时 20fps 纯烧 CPU,输入延迟最高 50ms。NLM 根因判断成立。
- **修形**:不引 crossterm `event-stream` feature(新增 futures 依赖,违最小依赖)。改:**阻塞读线程**(std thread `event::read()` → tokio mpsc)+ 主环 `tokio::select!` 多路复用(输入/token/图事件/审批/完成/tick);**dirty 标记**——仅脏或 busy(spinner 动画)才重绘,空闲零重绘。审批通道改 tokio unbounded(`unbounded_send` 同步上下文可用),应答仍 std sync_channel(agent 侧行为不动)。
- **验收修正**:NLM 的 CPU%/16ms/逃逸序列抓取全是**计时与环境抖动**,违「确定性可测」铁律,**驳回**;改为抽纯决策函数(`input_action` 键位路由、`should_draw` 重绘判定)单测覆盖 —— 续 iter-22 `approval_action` 同一模式。

### ✅ 采纳(P1,缩形):有界日志 + 视口尾窗
- **现实核验为真**:`Ui.log` 无界 `Vec`,`output_lines()` 每帧 flat_map 全量重建(长会话 O(N)/帧 + 内存无界)。
- **缩形**:`VecDeque` + `LOG_CAP` 环形淘汰(合 ARCHITECTURE §8.6「有界一切」不变量);纯函数 `tail_window(len, rows, scroll)` 算可见逻辑行区间,`output_lines` 只建窗口内行。不引 `ratatui_widget_scrolling` 外部库(几行码可解,违 ponytail 梯 5)。
- 验收:环形上界测试 + 窗口区间纯函数测试,零计时。

### ❌ 驳回(押后):主屏幕内联 REPL + 段落静态提交
- 巨型重写(渲染模型整个换),验收本质依赖真实终端行为(闪烁/原生历史/选取),**无法确定性测**;iter-21 已押后同类,单刀原则 —— TUI 专轨一轮一刀,本轮刀在事件环。留档不弃。

### ❌ 驳回:CSI u / bracketed paste / 动态输入高度 / 补全浮窗
- 打磨类,不阻正确性;推后。

### ⚠ 引用错位实录(反面教材续录)
- NLM 给「crossterm EventStream」引了 `91397bf0` 的 **BranchFS/branch() 系统调用**段落(引 11)—— 张冠李戴,与 TUI 无关。再证:引用须逐条核,不可默认成立。
