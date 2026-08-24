---
id: L3-EVAL-HARNESS-001
level: L3
parent: L2-EVAL-001
title: Bounded evaluation and evidence
status: VALID
code_targets:
  - crates/eval/Cargo.toml
  - crates/eval/src/lib.rs
  - crates/eval/src/main.rs
  - scripts/bounded-soak.ps1
  - scripts/recovery-soak.ps1
  - scripts/windows-pty-e2e.ps1
test_targets:
  - crates/eval/src/lib.rs
  - crates/agent/src/tui/idle_submit_tests.rs
  - crates/agent/src/tui/tests.rs
  - scripts/windows-pty-e2e.ps1
public_interface:
  - eval::HarnessOptions
  - eval::CaseResult
  - eval::InvariantEvidence
  - eval::EvalReport
  - "ridgecode-eval --json"
  - "ridgecode-eval --manifest"
  - "ridgecode-eval --recovery-fixture"
  - "npm run eval:soak"
known_gap:
  - The bounded soak and kill/restart recovery fixture are finite probes, not a 24-hour endurance test or long external A2A fault injection campaign.
  - ConPTY gates draw-render p95/max and total session output, but snapshot serialization/write, snapshot-byte percentiles, and event-loop latency remain ungated.
---

# Bounded evaluation and evidence

`scripts/bounded-soak.ps1` runs the JSON harness repeatedly with fixed
concurrency and timeout bounds (default 10 iterations, 3 workers, 5 seconds).
Each eval child also has a 30-second process deadline; a hung process is killed
and the soak fails instead of blocking the quality gate indefinitely.
It requires every iteration to emit the expected number of structured cases,
retain at least one approved case, and avoid an all-case timeout. It records
only stable aggregate evidence and writes `target/quality/bounded-soak.json`;
it is a finite regression probe, not a substitute for an unbounded or 24-hour
endurance run.

`scripts/recovery-soak.ps1` starts the real eval binary with an append-only
JSONL manifest, waits for durable completed records, kills that exact process
tree, then restarts against the same manifest. The resumed report must preserve
input order, reuse at least one completed case, execute only unfinished work,
and finish three unique approved fingerprints. A torn final line is recoverable;
middle corruption, conflicting duplicates, and unsupported versions fail
closed. Manifest paths are confined to the working tree, and records contain
fingerprints plus bounded results rather than raw task or marker bodies.

The `CompletionFixture` may run together with `ResizeProbe` to prove one
hermetic PTY session's real `read_file -> edit_file -> final` flow, edit target
path, folded tool output, answer table/highlight rendering, and changing
`snapshot.rect` dimensions. This combined evidence does not close the open
24-hour/restart, snapshot serialization/write, snapshot-byte percentile, or
event-loop latency gaps. Its opt-in snapshot telemetry retains exact draw-render
samples up to a fixed 4096-frame capacity; missing/truncated samples or p95/max
budget violations fail the probe.

The harness refills a bounded concurrency pool as each case completes, while
retaining input-order results. Slow cases no longer idle otherwise available
slots; case timeout, invariant evidence, and ConPTY output bytes remain bounded
and deterministic (`-MaxOutputBytes`, default 4 MiB). `ridgecode-eval` exposes
the same controls through `--concurrency`, `--timeout-ms`, `--max-steps`,
`--max-tokens`, and `--marker`; `--json` emits a structured report with
per-case status and invariant evidence, so a harness consumer need not parse
human prose. Every result includes wall-clock `duration_ms`; timeout/failure
results recover the latest in-memory checkpoint, retaining already observed
steps and tokens while still failing every invariant closed. The soak records
per-iteration process and case durations plus observed timeout cost.
`--fail-on-unapproved` is the explicit CI gate: it prints the
same report but exits non-zero when the suite is empty or any case fails an
invariant, so report-only runs cannot be mistaken for a passing gate.
The Windows quality gate runs the bounded soak plus phased Input and
Completion+Resize ConPTY fixtures after building the workspace; the Unix gate
runs the dependency-free Linux PTY replay.

每个 case 经 `build_llm_agent` 执行；可并发但结果按输入索引复原。invariant 数量、并发度与超时均有上限，最终报告仅保留稳定的类别与数值证据。Windows PTY 默认将 `status=partial` 视为非零失败；仅显式 `-AllowPartial` 可作诊断运行。
