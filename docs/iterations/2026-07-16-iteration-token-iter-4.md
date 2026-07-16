# 迭代报告 · 2026-07-16 · token 节约之路 iter-4:状态快照编译器(Durable State)

> 第一阶段 Runtime State **真正补全**:从「消息驱动」迈向「事实驱动」。

## 做了什么(纯内核,零外依赖)

**AgentState 加两个强类型 durable 字段**:
- `modified_files: BTreeSet<String>` —— 本任务已成功改动的文件。用 `BTreeSet`(非 NotebookLM 建议的 `HashSet`)保证**有序稳态**:编进 prompt 事实块时字节稳定、不抖动、利 Claude 缓存。
- `last_error: Option<String>` —— 上次工具错误的去噪首行,用于「重锚定」。

**确定性回填** `durable_updates(call, obs)`(act 节点执行工具后):
- 工具错误(前缀 ` error:` / `BLOCKED` / `permission denied`)→ 置 `last_error`;
- 写类工具成功(write_file/edit_file/apply_edits)→ 记入 `modified_files` 并清 `last_error`;
- 其余工具不动 durable 状态。

**事实块编译 + 注入** `durable_state_block(s)`:把 durable 字段编成紧凑 `<durable_state>` 块,注入 messages **末尾**(role=system)——**不进冻结的 system prompt**(缓存锚定:首部前缀稳定,动态状态后置),仅在有事实时注入。体量 O(去重文件数 + 一条报错),**不随步数膨胀**。

**对抗评审驳回**:①NotebookLM 建议 `HashSet` → 改 `BTreeSet`(缓存要确定性有序);②驳回 `environment_context`/`cd` 追踪 —— ridgecode 每次 `run_shell` 是全新 `sh -c`,`cd` **根本不跨调用持久**,追踪它是错的。

## 测试状态(确定性信号)

```
cargo test --workspace          # 全绿(+3 测)
cargo clippy --workspace ... -D warnings   # 净
cargo fmt --all --check         # 净
```

- `durable_state_backfill_from_tools`:write_file 成功记入 `modified_files` 并清 `last_error`;工具错误置 `last_error`、失败不记文件。
- `durable_state_block_stays_bounded_over_steps`:反复改同两文件 **50 步**,事实块字符数**恒定**(O(1),不随步数长)。
- `to_messages_appends_durable_fact_block`:事实块注入 messages 末尾(role=system),首部 system prompt 冻结不动;无事实则不加尾块。

## 内核愿景完成判据(NotebookLM 给的 4 条,逐条核对)

| # | 判据 | 状态 |
|---|---|---|
| 1 | **历史有界性**:长对话 history 自动压缩,上下文 token 估算恒定 | ✅ iter-1/2 |
| 2 | **静态底噪极小化**:工具定义 + system 指令审计剪枝 | ✅ iter-3 |
| 3 | **输出端 Lean-output**:强制简洁 + 最小 diff | ✅ iter-3 |
| 4 | **事实驱动而非消息驱动**:从 Durable 字段编事实块,不靠全量历史 | ✅ iter-4 |

**四判据全绿 → 内核侧 token 节约愿景基本达成、到边际效益递减点。**

## 归为外置(走 MCP/SKILL,内核不做)

- 向量检索 / 语义缓存(Qdrant/LanceDB/candle Embedding)—— MCP server。
- 终端去噪 squeez —— 外置 hook(token-saver 已覆盖)。
- AST 代码骨架提取(syn/tree-sitter)—— Local Skill / MCP。
- 精确 token 计数(tiktoken)—— 已用本地加权估算替代。

## 附条件推迟(YAGNI / 骨架已在)

- 动态工具加载:现仅 ~9 内置工具,「50→3-5」不成立、误滤=任务失败之险更高;待 MCP 工具数显著增大再评。
- 置信度分级模型路由:SwapProvider(热切换)+ FastContext(廉价档)已具骨架;全自动路由是较大架构件,非 token 节约必需。

## 请 NotebookLM 终审

四判据全绿、余项皆外置/附条件推迟。请确认:**本笔记本「token 节约之路」的内核侧愿景是否可判定「已完成」**?若仍有遗漏的**纯内核、可离线确定性验收**的 token 节约项,请明确指出;否则请确认收束。
