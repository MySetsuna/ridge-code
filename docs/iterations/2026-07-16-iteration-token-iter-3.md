# 迭代报告 · 2026-07-16 · token 节约之路 iter-3:静态底噪清理

> 承 iter-1/2(历史压缩)。本轮转向「静态底噪」——工具 Schema 与 system prompt 每轮都发,是 token 起步价。

## 做了什么

**极简 Tool Schema(NotebookLM P0)**:审计 `builtin_tool_specs` 各 `description`,裁三处最冗者——去「模型无需知的内部机制」与「与 schema 重复的内容」:
- `web_search`:删「探测 GFW/直连→DuckDuckGo、受限→Bing 中国版」的选引擎机制(模型不需知 how)。
- `todo_write`:删「像 Claude Code 的 TodoWrite」+ 删 `status ∈ pending|in_progress|completed`(已在 schema 里)。
- `apply_edits`:删尾部冗句。
- 其余(run_shell/read_file/edit_file 等)本已精简,不动。**只精简文案,不改 name/schema/参数语义**。

**Lean-output 指令(主源 Lean Prompting)**:`BASE_SYSTEM` 加一句「Reply concisely: no filler or restating the task; when changing code, emit only the minimal edit (unique-match replace / diff), not a full-file rewrite」。输出 token 比输入贵 3-4 倍,约束输出端 ROI 高。

## 测试状态(确定性信号)

```
cargo test --workspace          # 全绿(+2 测)
cargo clippy --workspace ... -D warnings   # 净
cargo fmt --all --check         # 净
```

- `tool_descriptions_stay_terse`:每工具 desc `chars().count() < 120`,守静态底噪不回潮。
- `base_system_has_lean_output_directive`:`BASE_SYSTEM` 含 `concisely` + `minimal edit`;无技能时 `build_system_prompt(&[])` 仍等于 `BASE_SYSTEM`(不引额外底噪)。

## 四阶段进度 & 愿景收敛

| 项 | 状态 |
|---|---|
| ① Runtime State 历史压缩 | iter-1/2 **已扎实** |
| ③ 极简 Schema + Lean-output | iter-3 **已做** |
| ① 状态快照编译器(Durable State) | **iter-4 待做**(第一阶段真正补全,长任务 95%) |
| 动态工具加载 / 置信度路由 | **推迟**(YAGNI / 骨架已在 SwapProvider+FastContext) |
| RAG / squeez / AST(syn) / tiktoken | **外置 → MCP**,不入内核 |

## 开放问题(请 NotebookLM 定夺)

1. iter-4 状态快照编译器:对 ridgecode,`AgentState` 该新增哪些**强类型 durable 字段**最值(建议:`modified_files`、`last_error`、`goal` 已有 `task`)?
2. 这些字段编译成的「事实块」应放 system prompt(静态、利缓存)还是每轮 messages 末尾(动态)?主源两处都提过,对 prompt 缓存哪个更优?
3. iter-4 后,内核侧 token 节约愿景是否即可判定「基本完成」(余项皆外置/附条件推迟)?若还有遗漏的纯内核项,请指出。
