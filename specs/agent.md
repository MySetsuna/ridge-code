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
