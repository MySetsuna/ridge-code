# CONTRACT —— Iteration 21:harness-aware 系统提示词硬化 + 工具调用鲁棒

- **开工时间戳**: 2026-07-17
- **依据**: `docs/iterations/2026-07-17-notebooklm-guidance-21.md` —— 读 NLM notes(交互/驾驭工程系列)+ 对抗评审。采纳其自陈「模型很蠢」根因 = **系统提示词未把物理契约讲透**;**押后**其巨幅 TUI 重写(打磨非正确性、量大、难无抖测)、**重申驳回** Saga 自动回滚。
- **里程碑**: iter-17/19/20 造出新的**harness 物理事实**(输出截断 / 删测试被拦 / signal 复利),但模型不知。本轮把这些事实**写进系统提示词**对齐模型行为,并补一处工具调用机制鲁棒缺口。
- **焦点**: 用户指定「系统提示词、工具调用方式、交互方式」。本轮攻**前二者**(根因、可测、廉价);交互(TUI)列押后专轨。

## 目标(End State)

模型开局即知其所处 harness 的物理契约:大工具输出被截断(该用 ranged read_file/search 取细节,勿反复巨读)、删/清空测试会被硬拦且算失败(前置对齐反奖励黑客)、可用 signal_write 沉淀跨会话复利。幻觉工具名不再静默空转,而被判为 error 喂熔断/失败信号。

## 任务与验收信号(离线可测、无计时抖动)

| 优先级 | 任务 | 确定性验收信号 |
|---|---|---|
| **P0** | **系统提示词硬化**:`BASE_SYSTEM` 注入三条物理契约(输出截断→ranged read;勿删/清空测试[拦=失败];signal_write 沉淀复利)。保持 terse(利 Claude 缓存、不违 token 北极星) | 单测:`BASE_SYSTEM` 含 `truncated`/`ranged read_file`/`delete or empty`/`signal_write`;`build_system_prompt(&[])` 仍等于 `BASE_SYSTEM`;既有 lean-output 断言(`concisely`/`minimal edit`)不破 |
| **P0** | **工具调用鲁棒**:`execute_tool_call` 未知工具默认臂由 `"unknown tool"` 改 `"tool error: 未知工具 ..."`(含 " error:"),使幻觉工具喂 `is_error_observation`→熔断计数(iter-18)+ 失败信号 | 单测:未知工具调用 → 观察含「未知工具」且 `is_error_observation`=true、`tool_output_failed`=true |

## 押后专轨(经对抗评审,设计留档,不在本轮)

- **TUI 交互重构**:异步事件视口 / `AppMode` 模态状态机(治「滚动即拒绝」)/ `ScrollState` 虚拟列表 / `tui-textarea` 多行输入 / `/`@ 补全悬浮菜单 / 启动 ASCII 动画 + Loading tips。**真价值**,但:①打磨非正确性,对自主循环 token/护栏无核心贡献;②量大违单轮 ponytail;③交互/渲染验收多含计时,难无抖测;④用户在场,紧迫度低于自主循环。俟独立 iteration + 用户在场做物理验收。
- **工具输出 TUI 折叠/预览(claw-tsaver)**:token/上下文**实质**已由 iter-19 `bound_observation` 内核截断解决;TUI 折叠仅显示层装饰,随交互重构再议。
- **Saga/EditBuffer 自动回滚 + 聚合 Diff 单次权限门**:回滚**重申驳回**(数据丢失 + 违 preserve-mistakes + 重造 git);聚合 Diff 属交互专轨。
- **RMCP 深度整合 / 真沙箱 / 时间旅行(扬长 A)**:与本轮焦点正交;需技术选型/违单二进制(归 MCP)/证据中,押后。

## 边界

不破坏现有 66(agent)+ 全工作区测试 + clippy/fmt 净;系统提示词新增**terse**(≤3 短句,英文续 existing 风格),不膨胀冻结前缀、不伤 prompt 缓存;三契约皆为**当下已实现**的真实 harness 事实(非画饼);工具调用改动只动默认臂措辞(归错),不改正常工具路径。

## 交付状态

> ✅ **已交付(2026-07-17)**。`BASE_SYSTEM` 加三条 harness 物理契约(truncation→ranged read / 勿删测试 / signal_write 沉淀)+ `execute_tool_call` 未知工具归 error。`cargo test --workspace` 全绿(agent lib 66→**68**,+2:`base_system_states_harness_contract`、`unknown_tool_is_error_classified`),clippy `-D warnings`、fmt 净。模型↔harness 交互契约对齐(iter-17/19/20 的物理事实终于「讲给模型听」)。TUI 交互重构列押后专轨。见 `docs/LOG.md` iter-21 条。
