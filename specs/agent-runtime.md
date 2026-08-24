---
id: L3-AGENT-RUNTIME-001
level: L3
parent: L2-AGENT-001
title: Agent graph assembly and CLI modes
status: VALID
code_targets:
  - crates/agent/src/main.rs
  - crates/agent/src/graph.rs
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
