---
id: L3-TOOLS-SAFETY-001
level: L3
parent: L2-TOOLS-001
title: Filesystem jail and command denylist
status: VALID
code_targets:
  - crates/tools/src/lib.rs
  - crates/agent/src/guard.rs
  - crates/agent/src/exec.rs
test_targets:
  - crates/tools/src/lib.rs
  - crates/agent/src/exec.rs
public_interface:
  - tools::jail_path
  - tools::is_dangerous_command
---

# Filesystem jail and command denylist

路径护栏采用词法归一化，拒绝绝对路径与 `..` 越界；命令护栏覆盖根目录删除、块设备覆写、格式化、fork bomb 等灾难模式。该层是轻量护栏，不宣称提供 OS 隔离。
