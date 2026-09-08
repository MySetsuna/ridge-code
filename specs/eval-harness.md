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
  - eval/harbor/ridgecode_agent.py
  - scripts/harbor-preflight.ps1
  - scripts/quality-preflight.ps1
  - scripts/quality-preflight.sh
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
  - eval::ExternalEvalOptions
  - eval::ExternalEvalCaseV1
  - eval::run_external_eval
  - eval::run_swebench_export
  - eval::SweBenchPredictionV1
  - eval::SweBenchScoreV1
  - eval::score_swebench_reports
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
invariant, so report-only runs cannot be mistaken for a passing gate. The JSON
top level also exposes `timeout_rate`, `resumed_rate`, and `average_tokens`;
`resumed_rate` measures manifest reuse only and is not a benchmark success
signal.
The local PowerShell quality gate runs the bounded soak plus phased Input and
Completion+Resize ConPTY fixtures after building the workspace; the GitHub
Windows matrix additionally runs Completion+Resize and the kill/restart
recovery soak on every push. The Unix gate runs the dependency-free Linux PTY
replay.

每个 case 经 `build_llm_agent` 执行；可并发但结果按输入索引复原。invariant 数量、并发度与超时均有上限，最终报告仅保留稳定的类别与数值证据。Windows PTY 默认将 `status=partial` 视为非零失败；仅显式 `-AllowPartial` 可作诊断运行。

`ridgecode-eval external` 是真实 provider 的 batch 入口：输入 JSON case 文件和
corpus root，显式提供 `ridgecode` 可执行文件。每个 case 将 task 写到自己的工作
目录，并以 `--isolate-runtime --require-api-key --read-only` 或调用者明确选择的权限模式运行；随后执行
同一 case 的 verifier argv。verifier 退出码 `0` 才映射为 `Passed`，其余退出码映射
为 `Failed`，进程错误或 deadline 映射为 `Error`。这个路径的 `--fail-on-unverified`
检查的是 external verifier，而不是 `approved`；空 case 集也必须失败。CLI 只输出
版本化 `ExperimentManifestV1` JSON，供 A/B 实验与回归 CI 直接比较。

`ridgecode-eval swebench-export` 消费含 `instance_id` 和 `problem_statement` 的本地
JSONL（其他 SWE-bench dataset 字段可存在但被忽略），约定每个已 checkout 的实例工作区
位于 `<workspaces-root>/<instance_id>`。它运行 machine-run 后以非 shell `git diff --binary`
导出官方 prediction JSONL，不运行或模拟 SWE-bench 测试容器。`--predictions` 文件是可
直接传给 `swebench eval ... -p` 的工件；需要真实 resolved 率时必须由官方 Docker/云端
harness 的结果文件给出。输出路径和 workspace 都受 root containment 验证，避免 dataset
字段驱动任意路径读写。

`ridgecode-eval swebench-score` 递归读取某一个官方 run/model 目录下的 `report.json`，
只接受 `{instance_id: {"resolved": bool}}` 这一官方判定形状，并输出版本化 scorecard；
`swebench-compare` 对两个 scorecard 计算 resolved delta 与 percentage-point delta。它们
只读官方评测工件，不会调用模型、Docker 或重跑测试。

`eval/harbor/ridgecode_agent.py::RidgeCode` 是 Harbor 的
`BaseInstalledAgent` adapter：只允许从 HTTPS 下载显式 pin 的 Linux 二进制，并在
安装前用 `RIDGECODE_BINARY_SHA256` 验证；运行时将 task 写入容器临时文件，调用
`ridgecode run --jsonl --no-persist --require-api-key`。它不把 trace、`approved` 或
进程退出码翻译为 Harbor reward，reward 仍仅来自 Harbor task verifier。API key 必须走
Harbor secret 注入；adapter 不读取或写入 host 配置，也不负责安装 Docker。

`scripts/harbor-preflight.ps1` 是只读 host 前置检查：以 JSON 输出 adapter 是否存在、
Harbor/Docker CLI 与 engine 是否可用，以及指定 benchmark 存储盘是否至少有 120 GiB 可用

完整质量门另有 `scripts/quality-preflight.ps1` 只读检查。它验证 Cargo/npm、
`cargo-llvm-cov`、Sonar scanner 与 `SONAR_TOKEN` 是否具备；token 只输出存在性，
不输出值。预检失败时应先补齐环境，再运行 `scripts/quality-gate.ps1`。
PowerShell 质量门会自动先执行该预检并在缺依赖时立即失败；手动执行预检用于在 CI
或本地收集结构化诊断。
Unix 质量门使用同目录的 `quality-preflight.sh`，输出相同 schema。
空间；任一必需检查失败时返回非零。它不安装依赖，也不读取或输出 provider credential。
