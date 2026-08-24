---
id: L2-EVAL-001
level: L2
parent: L1-PROJECT-001
title: Verification-first evaluation harness
status: VALID
depends_on:
  - L2-AGENT-001
  - L2-PROVIDER-001
code_targets:
  - crates/eval/src/lib.rs
  - crates/eval/src/main.rs
test_targets:
  - crates/eval/src/lib.rs
public_interface:
  - eval::run_eval
  - eval::run_eval_with_options
  - eval::EvalReport
  - eval::Invariant
---

CLI consumers may pass `--fail-on-unapproved` for a fail-closed gate: an empty
suite or any case that fails an invariant returns a non-zero exit status after
emitting the same structured report.

# Verification-first evaluation harness

eval harness 批量运行 agent，统计 pass-rate、steps、tokens，并只以 `approved` 与显式 invariant 计入通过。`run_eval` 走默认有界 `HarnessOptions`，`run_eval_with_options` 可显式收紧并发上限与 case timeout；两者保持输入顺序稳定和有界 evidence，失败/超时不泄漏原始错误文本。`ridgecode-eval --json` 提供可供 CI/审计消费的结构化结果，避免从展示文本反推质量闸。
