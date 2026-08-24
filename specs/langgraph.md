---
id: L2-LANGGRAPH-001
level: L2
parent: L1-PROJECT-001
title: LangGraph state engine
status: VALID
code_targets:
  - crates/langgraph/src/lib.rs
  - crates/langgraph/src/state.rs
  - crates/langgraph/src/graph.rs
  - crates/langgraph/src/checkpoint.rs
test_targets:
  - crates/langgraph/src/tests.rs
public_interface:
  - langgraph::GraphState
  - langgraph::StateGraph
  - langgraph::CompiledGraph::invoke_with
  - langgraph::Checkpointer
---

# LangGraph state engine

纯 Rust 图引擎，不依赖 LLM。节点接收不可变状态快照并返回 `Update`；`GraphState::apply` 在同步点执行 reducer。`StateGraph::compile` 校验入口与静态边，`CompiledGraph::invoke_with` 以 Pregel/BSP 超步并发运行节点、合并更新、再路由。

`RunConfig::max_supersteps` 是硬停机门；`MemoryCheckpointer` 提供内存时间旅行，`FileCheckpointer` 提供可恢复快照。
