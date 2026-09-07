---
id: L3-AGENT-RUNTIME-001
level: L3
parent: L2-AGENT-001
title: Agent graph assembly and CLI modes
status: VALID
code_targets:
  - crates/agent/src/main.rs
  - crates/agent/src/graph.rs
  - crates/agent/src/state.rs
  - crates/agent/src/context.rs
  - crates/agent/src/exec.rs
  - crates/agent/src/run.rs
test_targets:
  - crates/agent/src/graph.rs
  - crates/agent/src/main.rs
public_interface:
  - agent::build_llm_agent
  - agent::build_llm_agent_with
  - agent::AgentState
known_gap:
  - Streamed runs have bounded graph steps and tool timeouts but no single total wall-clock deadline; process kill/restart/resume remains a 24-hour harness gap.
---

# Agent graph assembly and CLI modes

入口固定为 `crates/agent/src/main.rs::run_cli`；TTY/非 TTY 分流在入口层完成。`build_llm_agent_with` 注入 provider 与 MCP 工具并装配核心图；`run_with_provider` 负责真实 provider 闭环，离线路径保留确定性 demo/test。

`ridgecode run --max-turns N` 对 machine-run 是严格的 reasoning 回合预算：每次
`reason` 节点完成后递增一次，达到 `N` 后不再发起新的 provider reasoning 请求。已在
最后一回合产生的工具调用仍必须排空，再进入 verify/wrapup；普通交互路径仍保持其
长任务默认上限。该隔离使外部 benchmark 的成本/回合对照可复现，且不能借由图引擎
固定收尾余量绕过用户指定预算。

`ridgecode run --isolate-runtime` 保留显式选择的 provider 与认证，却不加载用户配置中的
MCP、Skills、sub-agent、hooks 或通知；它使用默认安全 runtime 配置。所有 benchmark adapter
和本仓库的 external/SWE-bench evaluator 都传此开关，避免机器局部扩展改变工具表、prompt、
成本或副作用。交互式运行及没有该开关的 machine-run 保持可扩展的正常行为。

复杂任务可调用 `contract_write` 写入 `TaskContract`（objective、requirements、
constraints、deliverables、non_goals），再使用 `requirement_update` 将每个
`satisfied` requirement 绑定到先前真实工具调用的 `evidence_call_ids`。运行时将每个
工具结果写入有界 `EvidenceRef` ledger，并用成功 edit 递增 `workspace_revision`；完成
门只接受当前 revision 的成功 evidence。后续 edit 会使旧 evidence 过期，未满足、失败、
blocked 或 waived requirement 均不能被当作成功。没有显式 contract 的简单任务保持现有
完成语义，便于 checkpoint 兼容迁移。

工具观察同时保留面向 UI 的原始文本和 `ToolResultV1` 的机器可读结果。内置工具、内置网络工具
以及 MCP 协议的 `isError`/超时/传输结果在执行边界直接产出后者；协作等尚未声明同一 schema
的外部传输才走受限兼容投影。它固定为
schema version 1，记录 call id、工具名、声明 effect、`success/error/blocked/running` 状态、可选
exit code、retryable、变更路径及对应 workspace revision；历史最多保留 64 条，长字段截断。
对 `read_file`/`search` 还记录有界的读取窗口、命中数量与输出截断标记，并注入 durable
facts，模型可据此继续窗口化读取或缩小搜索范围，而不是猜测结果是否完整。
`running`、`blocked` 绝不等价于成功，任何非 success 状态都会直接阻断 checker。模型下一回合只收到最近一次重要的
结构化事实，并得到确定性恢复边界：running 只能 poll 原 job、blocked 不得原样重试、
retryable error 只允许在检查后重试幂等调用一次、non-retryable error 必须改变参数或策略；
因此可依据重试性和变更范围恢复，而不是从人类展示文本中猜测结果。

`dispatch_agent` 与 `dispatch_agents` 的 completed/failed、fallback、预算拒绝和超时状态也由
分发控制流直接产出结构化结果；展示文本中的 `[dispatch_status=...]` 仅为兼容模型/UI 的报告，
不再作为完成闸门的判据。

`edit_file` 与 `apply_edits` 接受可选 `expected_hash`（文件内容 SHA-256）。提供时，执行器在
任何写入前校验当前内容；批量编辑只要一项陈旧就整体 `BLOCKED`，要求重新读取锚点，避免长任务
或并发修改把旧上下文静默覆盖到工作区。

侦察护栏不再按 read/search 的固定次数截断：`explore_streak` 只累计未取得新证据的连续侦察。
新定位路径或不同观察会重置该计数，重复同路径且同观察才触发 handoff；因此大型仓库的有效定位
可以继续，反复无信息调用仍会被收敛为一次明确的 edit/verify/blocker 决策。

read-only 模式仍向模型暴露 MCP 已明确声明 `effect=explore` 的工具；`unknown`、edit 和
verify MCP 能力保持隐藏并在执行边界 fail closed。这样受限任务保有真实只读信息源，同时不
以名称或描述猜测动态工具的安全性。

运行落盘 manifest 同时提供兼容字段 `status` 和规范字段 `run_status`。后者只描述运行生命周期
（`running`、`completed`、`stopped`、`interrupted`、`cancelled`、`blocked`），由执行器统一归一化，
watchdog 不应再自行解释任意字符串。`verification.status` 独立描述 reviewer/最终验证（`pending`、
`passed`、`failed`），其 `approved` 与 `halt_reason` 均来自确定性完成闸和停机分类；模型文本或旧的
展示状态不能伪造 `passed`。

同一组 `run_status` 与 `verification` 字段也必须写入同一 run 的 `trace.json`；trace 是脱离
heartbeat manifest 的完整审计工件，消费者只读取 trace 时仍不得退回解析模型文本或旧的
`approved` 字段推断 reviewer 结论。
