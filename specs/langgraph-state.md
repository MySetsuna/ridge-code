---
id: L3-LANGGRAPH-STATE-001
level: L3
parent: L2-LANGGRAPH-001
title: Explicit state reducer contract
status: VALID
code_targets:
  - crates/langgraph/src/state.rs
  - crates/agent/src/state.rs
test_targets:
  - crates/langgraph/src/tests.rs
  - crates/agent/src/state.rs
public_interface:
  - langgraph::GraphState::apply
  - agent::AgentState
---

# Explicit state reducer contract

每种图状态必须声明 `Update` 与 `apply` 合并语义；节点不得直接修改共享状态。Agent 状态把消息、动作、工具输出、`approved`、步数和 durable facts 归并为可审计快照，避免并发超步丢更新。
