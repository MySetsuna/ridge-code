---
id: L3-LANGGRAPH-RUNTIME-001
level: L3
parent: L2-LANGGRAPH-001
title: Pregel BSP graph runtime
status: VALID
code_targets:
  - crates/langgraph/src/graph.rs
test_targets:
  - crates/langgraph/src/tests.rs
public_interface:
  - langgraph::StateGraph::add_node
  - langgraph::StateGraph::add_conditional_edge
  - langgraph::CompiledGraph::invoke_with
  - langgraph::CompiledGraph::resume
---

# Pregel BSP graph runtime

同一超步所有 frontier 节点读取同一快照；任务并发完成后按稳定顺序 `apply`，再由条件边计算下一 frontier。条件边优先于静态边；无后继节点隐式到 `END`；超步超限返回 `GraphError::StepLimit`。
