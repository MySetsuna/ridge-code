---
id: L3-LANGGRAPH-CHECKPOINT-001
level: L3
parent: L2-LANGGRAPH-001
title: Checkpoint and resume
status: VALID
code_targets:
  - crates/langgraph/src/checkpoint.rs
  - crates/langgraph/src/graph.rs
test_targets:
  - crates/langgraph/src/tests.rs
public_interface:
  - langgraph::Checkpoint
  - langgraph::MemoryCheckpointer
  - langgraph::FileCheckpointer
  - langgraph::CompiledGraph::resume
---

# Checkpoint and resume

每个超步可保存 `Checkpoint { step, frontier, state }`。内存 checkpointer 支持历史回读；文件 checkpointer 支持新进程从快照恢复。checkpoint 是运行事实，不替代语义规格。
