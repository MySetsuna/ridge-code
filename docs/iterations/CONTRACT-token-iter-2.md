# CONTRACT · token 节约之路 · iter-2:加权字符压缩触发器

- **开工时间戳**: 2026-07-16
- **上一轮**: iter-1(Runtime State 首刀 —— 自动上下文压缩,`91` 全绿)
- **依据**: `docs/iterations/2026-07-16-iteration-token-runtime-state.md` + NotebookLM 指导(经对抗评审)

## 目标(End State)

把 iter-1 的自动压缩**触发判据**从「history 条数(24)」改为「按内容体量的加权字符估算」——一条万字日志 ≫ 二十条短问答,条数触发会漏。纯内核、零外依赖、可离线单测验收。

## 任务与验收信号(可自主、可离线判定)

| 优先级 | 任务 | 确定性验收信号 |
|---|---|---|
| **P0** | 本地 `est_tokens(text)` 启发式(复用仓内 `token-count.mjs` 口径:CJK≈1 tok/字,ASCII≈1 tok/4 字),不引 tiktoken | 单测:纯 ASCII 串与等长 CJK 串的估算比 ≈ 4:1 |
| **P0** | `to_messages` 触发器改为「history 各条 `est_tokens` 之和 > `AUTO_COMPACT_TOKENS`(默认 ~6000,可调)」 | 单测:40 条中等消息(总量超阈值)触发压缩;短对话不触发 |
| **P0** | 诚实标注边界 | 报告/注释写明:加权触发改善「多条中等消息」的判准;「少数超大单条消息」仍需**单条内容截断**(属外置 squeez 域,不进内核) |

## 明确不做(经对抗评审,推迟/驳回)

- **动态工具加载(NotebookLM 荐 P1)**:推迟。现仅 ~9 内置工具,「50→3-5」之省不成立;误滤工具 = 任务失败之险更高。待 MCP 工具数显著增大再评。
- **状态快照编译器 / durable 字段(NotebookLM 荐 P2)**:推迟。需先给 `AgentState` 加 `modified_files`/`last_error` 等强类型字段并从工具执行回填,是独立大迭代;当前自动压缩已吃到近期大部分收益。
- **tiktoken-rs / 向量库(Qdrant/LanceDB)**:驳回进内核。精确 token 计数与向量检索是外置能力,走 MCP,不塞 Rust 内核。

## 边界

- 不破坏现有 91 测试 + clippy/fmt 干净。
- 纯 std,不加任何新 crate 依赖。
- 不改图结构/reducer;只动 `to_messages` 的触发判据 + 一个 `est_tokens` 纯函数。
- 密钥不入 trace/日志。

## 停机条件

`cargo test --workspace` 全绿 + clippy `-D warnings` + fmt `--check` 干净,且新增单测覆盖「加权触发」与「est_tokens 比例」两个确定性信号。
