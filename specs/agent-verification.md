---
id: L3-AGENT-VERIFY-001
level: L3
parent: L2-AGENT-001
title: Deterministic verification gate
status: VALID
code_targets:
  - crates/agent/src/brain.rs
  - crates/agent/src/graph.rs
  - crates/agent/src/state.rs
test_targets:
  - crates/agent/src/brain.rs
  - crates/agent/src/graph.rs
public_interface:
  - agent::AgentState
  - agent::build_llm_agent_with
---

# Deterministic verification gate

`verify_node` 独立检查 `verify_ok`；通过才写入 `approved=true`，失败则记录 issue 并回到 reason。停止由 `MAX_STEPS`/图运行上限与 `approved` 双保险控制；模型不能自行宣称通过。
