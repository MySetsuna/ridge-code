---
id: L3-AGENT-LIFECYCLE-001
level: L3
parent: L2-AGENT-001
title: Agent configuration, sessions, goals, and signals
status: VALID
code_targets:
  - crates/agent/src/auth.rs
  - crates/agent/src/config.rs
  - crates/agent/src/context.rs
  - crates/agent/src/goal.rs
  - crates/agent/src/login.rs
  - crates/agent/src/observe.rs
  - crates/agent/src/rich_output.rs
  - crates/agent/src/session.rs
  - crates/agent/src/signals.rs
test_targets:
  - crates/agent/src/goal.rs
  - crates/agent/src/session.rs
  - crates/agent/src/signals.rs
public_interface:
  - agent::Config
  - agent::Goal
  - agent::SessionRecord
  - agent::Signal
---

# Agent configuration, sessions, goals, and signals

配置与认证由 agent 入口统一加载；会话、目标与跨运行 signal 均采用有界
持久化，失败降级不掀翻主流程。上下文/观测/富输出模块只负责把运行事实
转换为 TUI、headless 与审计层可消费的结构。
