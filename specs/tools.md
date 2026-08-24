---
id: L2-TOOLS-001
level: L2
parent: L1-PROJECT-001
title: File and shell tools with safety gates
status: VALID
code_targets:
  - crates/tools/src/lib.rs
  - crates/tools/src/job.rs
  - crates/agent/src/exec.rs
  - crates/agent/src/guard.rs
test_targets:
  - crates/tools/src/lib.rs
  - crates/tools/src/job.rs
  - crates/agent/src/exec.rs
public_interface:
  - tools::read_file
  - tools::write_file
  - tools::run_shell
  - tools::jail_path
  - tools::is_dangerous_command
---

# File and shell tools with safety gates

tools crate 仅依赖 std（加输出编码/任务辅助），提供文件读写与跨平台 shell。写入路径先经 `jail_path` 限制在 cwd 子树；灾难性命令即使获批也由 `is_dangerous_command` 拒绝；shell 返回退出码与 stdout/stderr 供 verifier 读取客观信号。
