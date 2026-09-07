---
id: L2-AGENT-001
level: L2
parent: L1-PROJECT-001
title: Agent runtime and interaction shell
status: VALID
depends_on:
  - L2-LANGGRAPH-001
  - L2-PROVIDER-001
  - L2-MCP-001
  - L2-TOOLS-001
code_targets:
  - crates/agent/src/main.rs
  - crates/agent/src/lib.rs
  - crates/agent/src/graph.rs
  - crates/agent/src/state.rs
  - crates/agent/src/tui/mod.rs
test_targets:
  - crates/agent/src/graph.rs
  - crates/agent/src/tui/tests.rs
public_interface:
  - agent::AgentState
  - agent::build_llm_agent
  - agent::build_llm_agent_with
  - ridgecode binary
---

# Agent runtime and interaction shell

`ridgecode` 从 `run_cli` 进入：处理 meta/special command、配置与认证，再按 provider 或离线路径运行。agent 图把 reason、act、verify、MCP 工具和 durable state 装配到 langgraph；TTY 走 ratatui TUI，非 TTY 走 headless。

maker/checker 分离：reason/act 产生动作，verify 依据确定性信号判定；真实模型 provider 通过 `LlmProvider` 接缝注入，不改变图结构。

对于显式 task contract，checker 还要求每条 requirement 是 `satisfied`，并引用
`EvidenceRef` ledger 中属于当前 workspace revision 的成功 tool call。模型文本、伪造的
call id、旧 revision 的成功测试和 `waived` 均不能绕过该门；一次成功 edit 后会递增 revision，
从而使先前证明自动失效直到重新验证。

每次 act 还会产出有界、版本化的 `ToolResultV1`，把工具状态、exit code、可重试性、effect、
变更路径和 workspace revision 与展示用原始输出分离。checker 使用此结构化状态：任意非
success 结果都不能完成，running 不能冒充成功；durable context 只投放最近的关键结果，防止长任务的工具历史失控。
