---
id: L3-AGENT-A2A-001
level: L3
parent: L2-AGENT-001
title: Authenticated A2A communication boundary
status: VALID
code_targets:
  - crates/agent/src/communication.rs
  - crates/agent/src/main.rs
  - crates/agent/src/open_vision.rs
  - crates/agent/src/orchestrate.rs
test_targets:
  - crates/agent/src/communication.rs
  - crates/agent/src/open_vision.rs
public_interface:
  - agent::AgentEnvelope
  - agent::AgentClientSession
  - agent::AuthenticatedAgentTransport
  - agent::ReplayGuard
  - agent::StoreForwardQueue
  - agent::AGENT_HANDSHAKE_TIMEOUT
  - ridgecode a2a smoke
known_gap:
  - Long-duration external transport fault-injection soak remains outside the deterministic CLI gate; the default smoke now reuses one external JSON-RPC peer for two tasks, tears it down, and reconnects through a fresh peer/session.
---

# Authenticated A2A communication boundary

Agent-to-agent messages use bounded envelopes with role, autonomy, governance,
security stamps, replay protection, and an in-process or JSON-RPC transport.
Store-and-forward persistence validates and deduplicates envelopes before an
atomic replacement. `AgentClientSession` performs one handshake for repeated
bounded tasks and requires a new instance for reconnect; its JSON-RPC loopback
regression covers both paths. `a2a smoke` now sends two bounded tasks over one
real external fixture peer, then starts a fresh peer after teardown, exercising
both transport reuse and reconnect;
`RIDGE_A2A_SECRET` applies HMAC, time-window, and nonce replay protection to
both sessions. The CLI client path also goes through `AgentClientSession`, so
the shipped transport uses the same handshake/session validation as the
library boundary. In-process routed teammates share `AgentCancellation`,
which emits a correlated Cancel envelope and prevents fallback after user
takeover. The graph orchestration layer remains the policy caller; the
transport does not grant write or shell authority.
All client, server, and paired in-process handshake paths bound the complete
send/receive phase to 15 seconds and return `AgentProtocolError::Timeout` for a
silent peer; the longer task exchange deadline begins only after negotiation.
