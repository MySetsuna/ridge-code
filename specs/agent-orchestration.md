---
id: L3-AGENT-ORCHESTRATION-001
level: L3
parent: L2-AGENT-001
title: Routed sub-agent orchestration and bounded waves
status: VALID
code_targets:
  - crates/agent/src/dispatch_budget.rs
  - crates/agent/src/orchestrate.rs
  - crates/agent/src/route.rs
  - crates/agent/src/graph.rs
  - crates/agent/src/knowledge.rs
test_targets:
  - crates/agent/src/orchestrate.rs
  - crates/agent/src/graph.rs
public_interface:
  - agent::dispatch_agent
  - agent::dispatch_agents
  - agent::route_task
  - agent::Agents
  - agent::DispatchBudget
  - agent::run_planned_with_budget
  - agent::run_planned_routed_with_budget
  - agent::run_planned_routed_with_cancellation
  - agent::run_planned_routed_with_cancellation_and_budget
known_gap:
  - External provider integration, a total wall-clock deadline for streamed runs, and long-duration timeout/fault soak remain outside the deterministic workspace gate. Cross-process dispatch budgets require an external coordinator; the default budget is process-local.
---

# Routed sub-agent orchestration and bounded waves

The graph exposes singular and batch dispatch tools. Planning trims and drops
blank planner entries, then deterministically keeps at most five subtasks.
Legacy and routed planning share that parser; invalid or empty output still
falls back to the original task. Routed execution admits at most three
teammates per wave and aggregates the five-or-fewer results in planner order. A
shared `DispatchBudget` additionally caps planner/worker/A2A permits across
waves and independent runs (default three; override with
`RIDGE_DISPATCH_CONCURRENCY`). Permits are RAII-released on success, fallback,
provider error, or cancellation, so a failed teammate cannot leak capacity.
The cancellable routed entry point propagates one `AgentCancellation` through
the planner, every teammate wave, and the in-process A2A handler; waiting for
a permit is cancellation-aware and cannot trigger a provider fallback.
Budget rejection is returned as structured `GraphError::DispatchBudget`
(`operation`, `limit`, `reason`) rather than requiring display-string parsing.
Skill and agent definitions stay declarative; the runtime enforces read-only
sub-agent tool scopes and both per-wave and shared dispatch budgets. Graph-tool
`dispatch_agent`/`dispatch_agents` calls use the same process-wide default
budget as planned/routed orchestration; explicit test/integration callers may
pass a shared `DispatchBudget` to both paths.
