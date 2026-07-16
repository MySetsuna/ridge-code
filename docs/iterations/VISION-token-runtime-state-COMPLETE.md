# 愿景收束 · token 节约之路(内核侧)· 已完成

- **日期**: 2026-07-16
- **驱动**: NotebookLM 笔记本「token节省之道」;主源《rust agent开发下一步的token 节约之路》
- **裁决**: NotebookLM 终审「**是**」—— ridgecode **内核侧** token 节约愿景**正式收口**,到边际效益递减点。

## 完成判据(4/4 全绿)

| # | 判据 | 落地轮次 | 确定性验收 |
|---|---|---|---|
| 1 | **历史有界性** —— 长对话 history 自动压缩,上下文估算 token 恒定 | iter-1/2 | `to_messages_auto_compacts_when_history_heavy`、`est_tokens_*`、`compact_history_*` |
| 2 | **静态底噪极小化** —— 工具 Schema + system 指令审计剪枝 | iter-3 | `tool_descriptions_stay_terse`(<120 字/工具) |
| 3 | **输出端 Lean-output** —— 强制简洁 + 最小 diff | iter-3 | `base_system_has_lean_output_directive` |
| 4 | **事实驱动而非消息驱动** —— Durable State 编事实块,不靠全量历史 | iter-4 | `durable_state_backfill_from_tools`、`durable_state_block_stays_bounded_over_steps`(50 步 O(1)) |

四轮全程:纯内核、零新依赖、未改图结构、`cargo test --workspace` + clippy `-D warnings` + fmt 全绿。

## 迭代轨迹

| 轮 | 内容 | commit |
|---|---|---|
| iter-1 | 自动上下文压缩(O(n)→有界快照)+ compact_history 悬空 tool 护栏 | `bbe7016` |
| iter-2 | 压缩触发器改加权字符估算(est_tokens,不引 tiktoken) | `775f89e` |
| iter-3 | 极简 Tool Schema 审计 + BASE_SYSTEM Lean-output 指令 | 本轮 |
| iter-4 | 状态快照编译器(modified_files/last_error + 事实块注入末尾) | 本轮 |

## 归口:非内核事项(不算未竟)

**外置能力 → 走 MCP / SKILL(内核不做)**:
- 向量检索 / 语义缓存(Qdrant/LanceDB/candle Embedding)。
- 终端去噪 squeez(外置 hook;token-saver 已覆盖)。
- AST 代码骨架提取(syn/tree-sitter)—— Local Skill / MCP。
- 精确 token 计数(tiktoken)—— 已用内核原生加权字符估算替代。

**附条件推迟(YAGNI / 骨架已在)**:
- 动态工具加载 —— 现仅 ~9 内置工具,「50→3-5」不成立、误滤=任务失败之险高;待 MCP 工具数显著增大再评。
- 置信度分级模型路由 —— SwapProvider(热切换)+ FastContext(廉价档)已具骨架;全自动路由是较大架构件,非 token 节约瓶颈。

## 贯穿原则

- **内核精简**:外置可安装工具走 MCP/SKILL,不塞 Rust 内核(契合项目北极星四层解耦)。
- **maker≠checker**:NotebookLM 为规划 maker,每轮建议均过第一性原理 + 代码现实的对抗评审;驳回记录在案(tiktoken 依赖、内核向量库、squeez 内核重造、HashSet→BTreeSet、cd 追踪错误、过早动态工具加载)。
- **确定性验收**:每项 token 节约都落一个可跑单测,不认模型自述。
