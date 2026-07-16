# CONTRACT · token 节约之路 · iter-3:静态底噪清理(极简 Schema + Lean-output 指令)

- **开工时间戳**: 2026-07-16
- **上一轮**: iter-2(加权字符压缩触发器,`92` 全绿)
- **依据**: NotebookLM 指导(P0 = 极简 Tool Schema 审计)+ 主源 Lean Prompting(经对抗评审)

## 目标(End State)

从「压缩历史」转向「清理**静态底噪**」——工具 Schema 与 system prompt 每轮都发,是 token 起步价。审计并守住其精简,再给 system prompt 加一条 Lean-output 指令(输出端省钱)。纯内核、可离线单测验收。

## 任务与验收信号(可自主离线判定)

| 优先级 | 任务 | 确定性验收信号 |
|---|---|---|
| **P0** | 审计 `builtin_tool_specs` 各工具 `description`:砍冗余自然语言,只留核心约束 | 单测:每个内置工具 `description` 字符数 < `TOOL_DESC_MAX`(如 160);全部 desc 合计 < 硬上限 |
| **P0** | `BASE_SYSTEM` 加 **Lean-output 指令**(如「简洁作答;改代码只出 diff/唯一匹配替换,勿整文件重写;无用客套省去」)若尚无 | 单测:`BASE_SYSTEM` 含精简约束关键词;`build_system_prompt(&[])` 仍等于 `BASE_SYSTEM` |
| **P1(可选)** | 加一个「Schema 总量守护」测试,防未来工具描述膨胀回潮 | 单测:所有内置 spec 序列化后总字符 < 预设 `HARD_LIMIT` |

## 明确不做(对抗评审)

- 不改工具的 `name`/`schema` 结构、不删工具、不改参数语义 —— 只精简 `description` 文案。
- 不做动态工具加载(推迟:现 ~9 工具,YAGNI)。
- 不做输出人格 config 注入(已被 `load_project_rules` 注入 CLAUDE.md/Skills 覆盖)。

## 边界

- 不破坏现有 92 测试 + clippy/fmt 干净;纯 std,无新依赖。
- 工具 description 精简后仍须**语义完整**(模型能据以正确调用)——精简是去客套/去冗词,不是去信息。
- 密钥不入 trace/日志。

## 停机条件

`cargo test --workspace` 全绿 + clippy `-D warnings` + fmt `--check` 干净,且新增单测覆盖「每工具 desc 上限」与「BASE_SYSTEM 含 Lean 指令」两个确定性信号。
