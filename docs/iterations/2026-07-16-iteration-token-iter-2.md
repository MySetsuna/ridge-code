# 迭代报告 · 2026-07-16 · token 节约之路 iter-2:加权字符压缩触发器

> 轨道:token 节约之路(笔记本「token节省之道」驱动)。承 iter-1(自动上下文压缩)。

## 本轮目标(承 CONTRACT-token-iter-2 + NotebookLM 指导)

iter-1 的自动压缩用「history 条数(24)」触发,过于粗放——一条万字日志仅计 1 条。本轮改为**按内容体量的加权字符估算**触发,更准,仍纯内核、零外依赖。

## 做了什么

- 新增 `est_tokens(text)`:本地 token 启发式,CJK 等非 ASCII ≈ 1 token/字,ASCII ≈ 1 token/4 字符(口径同仓内 `bin/token-count.mjs`)。不引 tiktoken。
- `to_messages` 触发器:`AUTO_COMPACT_AT`(条数)→ `AUTO_COMPACT_TOKENS=6000`(history 各条 `est_tokens` 之和)。超阈值才 `compact_history`。
- 保留数 `AUTO_COMPACT_KEEP=8` 不变;阈值是可调校准旋钮。

## 诚实边界(对抗评审自纠)

加权触发改善「**多条中等消息**」的判准(总量准了)。但「**少数超大单条消息**」下,`compact_history` 按**条数**裁(留末 8 条),消息数 ≤ keep+1 时它 early-return 减不动——那需**单条内容截断**,属外置 squeez 域,不进内核。故本轮是触发**精度**改良,非万能。测试如实只验「多条重消息触发压缩」,不谎称少数超大消息被压。

## 测试状态(确定性信号)

```
cargo fmt --all --check              # 净
cargo clippy --workspace ... -D warnings   # 净
cargo test --workspace               # 全绿(92 测,+1 净)
```

- `est_tokens_weights_cjk_heavier_than_ascii`:同 400 字符,ASCII→100、CJK→400(4:1)。
- `to_messages_auto_compacts_when_history_heavy`:40 条重消息(总量超阈值)触发压缩收敛为有界;20 条轻消息(总量未超)不误伤、全量带过。

## 四阶段路线图进度

| 阶段 | 状态 |
|---|---|
| ① Runtime State | iter-1 自动压缩 + iter-2 加权触发**已扎实**;完整「状态快照编译器」(durable 字段 → 事实块)仍待(需先加强类型字段,独立大迭代) |
| ② Context RAG | 未做;向量库(Qdrant/LanceDB)走 MCP,不进内核 |
| ③ 动态工具加载 / 极简 Schema | **极简 Schema** 可纯内核做、可离线验收(断言各工具 desc 精简);**动态加载**推迟(现仅 ~9 工具,YAGNI) |
| ④ 模型路由 | 已有 FastContext 廉价档;「按复杂度分级路由」是较大架构件 |

## 开放问题(请 NotebookLM 定夺)

1. ① 的「状态快照编译器」值得作为独立大迭代做吗?对已是状态机的引擎,ROI 相比后续项如何?
2. 下一轮选 **③ 极简 Schema 审计**(纯内核、可离线验收、立即可做)对不对?还是有更高 ROI 的纯内核项?
3. 除路线图四阶段,笔记本主源里还有哪些**纯内核、可离线确定性验收**的 token 节约点是我漏掉的?请给一份「剩余可自主内核项」清单,好收敛「愿景全清」。
