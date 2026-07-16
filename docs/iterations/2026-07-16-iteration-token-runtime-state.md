# 迭代报告 · 2026-07-16 · Runtime State 首刀:自动上下文压缩

> 轨道:**token 节约之路**(由 NotebookLM 笔记本「token节省之道」驱动)。
> 主源:《rust agent开发下一步的token 节约之路》。

## 本轮目标(来自主源)

主源为 ridgecode(langgraph-rust)量身规划「下一步 token 节约」四阶段路线图:

1. **Runtime State Machine** —— O(n) 全量历史 → O(1) 状态快照(**主源明荐第一步,收益最高**)。
2. Context Engineering / RAG —— 向量检索替代全量注入 + 渐进式总结。
3. 动态工具加载(50 工具只发相关 3-5)+ 极简 Schema。
4. 模型路由(廉价模型接简单任务)。

本轮只取**第一阶段的最小可落地切片**。

## 做了什么

**问题(代码现实,codegraph 定位)**:`to_messages`(crates/agent/src/lib.rs)每轮把 `[system] + 全部 s.history` 发给 LLM,无界。压缩此前**仅 `/compact` 手动**(tui.rs)。长任务多轮下历史随步数线性膨胀 → 爆 token 预算 + 反复击穿 prompt 缓存。

**改动(内核原生,零外依赖)**:
- `to_messages` 增**自动压缩**:`history` 超 `AUTO_COMPACT_AT=24` 条时,发 LLM 前先 `compact_history(.., AUTO_COMPACT_KEEP=8)`,把全量历史收敛为「原任务 + 摘要标记 + 最近 8 条」的有界快照。一处收口,每轮 reason 皆经。
- `compact_history` 加 **API 正确性护栏**:裁掉保留窗口首端的悬空 `role=tool`(其配对 assistant 已被压掉),否则 OpenAI 兼容端点会因「tool 无前置 tool_calls」返回 400。手动 `/compact` 与自动两路皆受益。
- 阈值/保留数留作**可调校准旋钮**(常量),按 token 精算(tiktoken)是后续升级,现避免外依赖。

**复用而非重造**:直接用仓内既有 `compact_history`(已测),未加任何新依赖、未改图结构、未动 reducer。

## 测试状态(确定性信号)

```
cargo fmt --all --check         # 净
cargo clippy --workspace --all-targets -- -D warnings   # 净
cargo test --workspace          # 91 passed / 0 failed(+2 新测)
```

新增两测:
- `to_messages_auto_compacts_long_history`:41 条历史 → 发出消息收敛为 ≤ 11 条(system+task+摘要+8),短历史不动。
- `compact_history_drops_dangling_tool_result`:窗口首端悬空 tool 被裁,首条保留非 `role=tool`。

## 能力对照(距四阶段终点差什么)

| 阶段 | 状态 | 说明 |
|---|---|---|
| ① Runtime State | **本轮首刀** | 自动压缩落地;**完整状态快照编译**(enum 状态 → 只注入当前状态相关最小变量,而非消息历史)尚未做 |
| ② Context RAG | 未做 | 向量检索/渐进摘要;**注意**:Qdrant/LanceDB 是外置可装工具,按内核精简原则应走 MCP,不进内核 |
| ③ 动态工具/极简 Schema | 部分 | 内置工具 desc 已较简;**动态按意图加载工具**未做(现全量 offer) |
| ④ 模型路由 | 部分 | 已有 FastContext 廉价档(sub-agent),但无「按复杂度自动分级路由」 |

## 驳回 NotebookLM 的建议(对抗评审)

- **驳回 `tiktoken-rs` 依赖**:主源/前源都荐引 tiktoken 做 token 计数。ridgecode 内核走 std、按**条数**触发已足;精确 token 计数是外置能力,不为此引重依赖(YAGNI + 内核精简)。
- **驳回「把 squeez 终端去噪重实现进内核」**:本轮初版曾做,后**回退**。squeez 是 token-saver 的外置可装工具,主源开篇已归其为「静态外挂层已覆盖」;在内核重造违背「外置工具走 MCP/SKILL、不塞内核」原则。
- **对②的 RAG**:采纳「按需检索替代全量注入」的**思想**,但驳回「把向量库编进内核」——应作为 MCP server 接入。

## 开放问题(请 NotebookLM 定夺)

1. 第一阶段的「**完整状态快照编译**」(主源的 enum + transition + 只注入状态相关最小变量)——对一个**已是状态机**(langgraph AgentState/Patch reducer)的引擎,值得再抽一层「状态→prompt 编译器」吗?还是当前「自动压缩历史」已吃到该阶段 80% 的收益、边际递减?
2. 下一轮该推**③ 动态工具加载**(按意图只 offer 相关工具,收益明确、纯内核可做、可离线验收)还是**④ 模型路由**(按复杂度分级)?哪个对「Rust agent 内核」ROI 更高?
3. 自动压缩阈值(24 条 / 保留 8)缺乏真实 token 校准。在不引 tiktoken 的前提下,有无「按字符数估算 + CJK 权重」的轻量本地判据可作触发条件?
