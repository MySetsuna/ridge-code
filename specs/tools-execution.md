---
id: L3-TOOLS-EXECUTION-001
level: L3
parent: L2-TOOLS-001
title: Bounded shell execution
status: VALID
code_targets:
  - crates/tools/src/lib.rs
  - crates/tools/src/job.rs
test_targets:
  - crates/tools/src/lib.rs
  - crates/tools/src/job.rs
public_interface:
  - tools::run_shell
  - tools::run_or_park_shell
  - tools::poll_shell_job
  - tools::cancel_shell_job
---

# Bounded shell execution

同步与可暂停 shell 任务都保留退出码、标准输出、标准错误和超时边界；长任务通过 job API 轮询/取消，避免 agent 永久卡在工具调用。
