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
  - eval/harbor/ridgecode_agent.py
test_targets:
  - crates/eval/src/lib.rs
public_interface:
  - eval::run_eval
  - eval::run_eval_with_options
  - eval::run_external_eval
  - eval::run_swebench_export
  - eval::score_swebench_reports
  - eval::compare_swebench_scores
  - eval::EvalReport
  - eval::ExternalEvalOptions
  - eval::ExternalEvalCaseV1
  - eval::ExperimentManifestV1
  - eval::SweBenchPredictionV1
  - eval::SweBenchScoreV1
  - eval::Invariant
---

CLI consumers may pass `--fail-on-unapproved` for a fail-closed gate: an empty
suite or any case that fails an invariant returns a non-zero exit status after
emitting the same structured report.

# Verification-first evaluation harness

eval harness 批量运行 agent，统计 pass-rate、steps、tokens，并只以 `approved` 与显式 invariant 计入本地 harness 通过。`EvalReport` 还提供 `timeout_rate`、`resumed_rate` 和 `average_tokens`，用于区分稳定性、恢复覆盖和成本。`run_eval` 走默认有界 `HarnessOptions`，`run_eval_with_options` 可显式收紧并发上限与 case timeout；两者保持输入顺序稳定和有界 evidence，失败/超时不泄漏原始错误文本。`ridgecode-eval --json` 提供可供 CI/审计消费的结构化结果，避免从展示文本反推质量闸。

外部 benchmark 使用独立的 `run_external_eval`：它逐 case 启动隔离的
`ridgecode run --jsonl --no-persist`，随后以**非 shell** argv 方式启动 case
声明的 verifier。`CaseResultV1::externally_verified_success` 只能在 verifier
退出成功时成立；agent 自己的 `approved`、stdout 文案和模型 final 都只能作为
诊断字段。每个子进程均有 deadline，stdout/stderr 仅取有界摘要；启动失败、超时
或无法解析 machine result 均 fail closed。输入 case/workspace/verifier 路径必须
在显式 corpus root 内，防止批量评测越界执行任意路径。

SWE-bench 接入只导出官方 prediction JSONL：每行固定为 `instance_id`、
`model_name_or_path`、`model_patch`，其中 patch 由受限 workspace 的
`git diff --binary` 直接取得。RidgeCode 不将 `approved`、本地 verifier 或
export 成功解释为 SWE-bench resolved；用户必须把生成的 JSONL 交给官方的 Docker
harness（或官方云端提交）评分。实例 id 不得含路径分隔符，workspace 必须在显式
root 中，空或 agent 失败的任务也要写一条空 patch prediction，确保官方结果能区分
“未解决”与“未提交”。

官方 SWE-bench harness 运行结束后，RidgeCode 只读取各实例 `report.json` 中由
harness 写入的 `resolved` 布尔值，形成 `SweBenchScoreV1`（total/resolved/
unresolved/rate）并可比较两个 scorecard 的 resolved 差值与百分点差值。无效 JSON、
缺失 `resolved`、重复 instance id 均 fail closed；不会从 agent trace、prediction
patch 或模型自述推断 resolved。

为执行 Terminal-Bench/Harbor 数据集，`eval/harbor/ridgecode_agent.py` 以
`BaseInstalledAgent` 形式运行已 pin 且校验摘要的 RidgeCode Linux binary。adapter 的
machine-run 以 `--isolate-runtime` 启动，trace 只供诊断，Harbor verifier 的 reward 才是标准任务的结果；因此同模型
scaffold A/B 不会把 agent 自己的完成宣称混入 benchmark 得分。
