# NotebookLM Notes 对抗评审(iter-21:系统提示词 / 工具调用 / 交互)

> 本轮起**不再上传迭代记录为 NLM 来源**(用户定),仅读既有 notes + 本地归档。NLM notes 是计划 **maker**,非裁判;下逐条经 checker(来源支撑 / 第一性原理 / 当前代码现实)。
> 相关 notes(切「系统提示词/工具调用/交互」):`e86f5681`(交互加固与审美升级)、`ec06937a`(交互重构与驾驭工程)、`1e0eb443`(TUI 性能优化)、`dc367e33`(终端审美/动画)、`e928a1a4`(原子化提交/EditBuffer)。

## 关键判定:notes 多为 **Iteration-12 时代**所拟

这些 notes 自陈语境为「M1-M11 成果」「Iteration 12」「交互一团糟」,写于 UX 打磨期。须对**当下**(已到 iter-20)代码现实重估,非照单全收 —— 其中多项已做/已被更优方案取代/已被对抗评审驳回。

## 逐条裁决

### ✅ 采纳(本轮 P0):harness-aware 系统提示词硬化 —— notes 自陈「模型很蠢」根因
- **来源真支撑**:`ec06937a`/`e86f5681` 明确「模型表现蠢 = 驾驭工程没把物理约束讲透」,荐在 `BASE_SYSTEM` 注入「物理信号契约」:告知输出会被截断、分段 read_file、Exit Code 0 是唯一成功证明。
- **当前现实校验**:`BASE_SYSTEM` 已covers edit>write / search+ranged read / web / 确定性验证 / lean。**缺**的恰是 iter-17/19/20 **新成的**物理事实:①工具输出被 `bound_observation` 截断(iter-19)——模型不知,可能反复巨读;②删/清空测试被约束守卫硬拦(iter-18)——告知可**前置**对齐反奖励黑客;③`signal_write` 可沉淀跨会话复利(iter-16/20)——模型少主动用。三者皆**当下为真**、注入即对齐。
- **落地**:`BASE_SYSTEM` 加三句(英文,续 existing 风格,保持 terse 利缓存):truncation→ranged read、勿删测试(拦=失败)、signal_write 沉淀。确定性测断言含关键短语。

### ✅ 采纳(本轮 P0,顺带):工具调用鲁棒 —— 未知工具归错
- **第一性 + 现实**:`execute_tool_call` 默认臂回 `"unknown tool \`x\`"`,**不含**判据词 → `is_error_observation`/`tool_output_failed` 均 false → 幻觉工具名**静默空转**,不喂 iter-18 熔断计数。改回 `"tool error: 未知工具 ..."`(含 " error:")→ 幻觉工具连发即触熔断早停 + 落失败信号。小而实的机制修,契合本轮「工具调用方式」。

### ❌ 驳回/押后:巨幅 TUI 交互重写(视口/模态状态机/虚拟列表/slash 菜单/ASCII splash)
- `1e0eb443`/`e86f5681`/`ec06937a`/`dc367e33` 大篇幅荐:异步事件视口、`AppMode` 模态状态机、`ScrollState` 虚拟列表、`tui-textarea`、`/`@ 悬浮菜单、启动 ASCII 动画、Loading tips。
- **对抗评审**:①**属打磨非正确性** —— 对「无人值守自主循环」的正确性/token 北极星无核心贡献;TUI 是**用户在场**场景,紧迫度低于自主循环护栏。②**工作量巨**(多 iteration UI 重构),违 ponytail 单轮最小。③**难无抖测** —— 交互/渲染/滚动的验收多含计时/人操作,违工作流「确定性、无计时抖动」本旨。④notes 自身把「模型很蠢」根因归于**系统提示词**(已采纳),UI 是另一层。
- **结论**:列**押后专轨**(真价值,但需独立 iteration + 用户在场验收);本轮不碰。其中「工具输出折叠/预览(claw-tsaver)」的**token/上下文实质**已由 iter-19 `bound_observation` 内核截断解决,TUI 折叠仅剩显示层装饰。

### ❌ 驳回(重申):Saga/EditBuffer 自动回滚(`e928a1a4`)
- 与 iter-19 对 guidance-18 的驳回**一致**:自动 `git checkout .` 毁用户未提交改动 + 失败现场(违 preserve-mistakes);分布式 Saga 语境误套单用户本地 CLI;重造 git。合理内核(记录改动文件)已由 durable state/manifest 实现。**单次权限门/聚合 Diff** 属 TUI 专轨,随交互重构再议。

### ◻ 未采纳(超范围/证据弱):RMCP SDK 深度整合、真沙箱、时间旅行
- 多篇 notes(`16f006ef`/serde 持久化系列/沙箱系列)重弹 RMCP/沙箱/时间旅行。与本轮「系统提示词/工具调用/交互」焦点正交;RMCP 替换/真沙箱需技术选型 + 违单二进制(归 MCP 铁律),时间旅行(扬长 A)证据中、押后。

## iter-21 最终设计
- **P0(本轮实现)**:harness-aware 系统提示词硬化(truncation/勿删测试/signal_write 三契约)+ 未知工具归错。cheap、确定性可测、直击「模型很蠢」根因,服务「系统提示词 + 工具调用」。
- **押后专轨(设计留档,俟用户/独立 iteration)**:TUI 交互重构(视口/模态/滚动/补全菜单/审美)—— 用户在场、工作量大、难无抖测;非本轮。
