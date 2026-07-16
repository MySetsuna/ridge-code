# 迭代报告 · 2026-07-16 · iter-5:轻量内核安全护栏(写操作 jail + denylist)

> 方向:用户择「轻量内核护栏」(而非重量 Docker/gVisor —— 后者 gVisor 仅 Linux、用户在 Windows、需环境决策)。现有安全 = 危险命令拦截 + 权限门 + diff 确认;本轮加**写操作沙箱**。

## 做了什么(纯 std,跨平台,可离线单测)

**写操作路径 jail(信任边界,核心)** `tools::jail_path(root, target)`:
- 把目标路径解析进 `root`(= 进程 cwd,`--cwd` 所设)子树;**绝对路径 / `..` 越界 → 拒**。
- **纯词法规整**(逐 `Component` 处理,`..` 用 pop),**不碰文件系统** —— write_file 要新建的文件尚不存在,不能用 `canonicalize`;规整后 `starts_with(root)` 统一裁决。
- agent 侧 `jail()` 守卫接进 `execute_tool_call` 的 write_file/edit_file/apply_edits 三臂(apply_edits 任一路径越狱则整批拒,与其原子性一致);越狱回 `BLOCKED (jail): …`,深度防御。

**危险命令 denylist 补漏**:
- 修 `dd` 漏洞:此前 `dd of=/dev/` 漏了 `dd if=/dev/zero of=/dev/sda` → 改为 `of=/dev/sd|nvme|hd|vd|mmcblk`(不误伤 `of=/dev/null`/`of=/dev/zero`)。
- 加 `wipefs`、`shred /dev/`、无空格重定向变体 `>/dev/sd`。

## 测试状态(确定性信号)

```
cargo test --workspace          # 全绿(+2 测)
cargo clippy --workspace ... -D warnings   # 净
cargo fmt --all --check         # 净
```

- `jail_path_confines_writes_to_root`:子树内(含 `a/../b`)放行;`../`、`../../etc`、绝对路径逃逸全拒。
- `jail_blocks_write_outside_cwd`(agent):越 cwd 绝对路径写经 `execute_tool_call` → `BLOCKED` 且**不落盘**。
- `dangerous_commands_are_flagged` 补:`dd if=/dev/zero of=/dev/sda`、`of=/dev/nvme0n1`、`wipefs` 拦;`of=/dev/null` 不误伤。
- 沙箱后 3 个既有写测试改用 cwd 相对路径(temp_dir 在 cwd 外会被 jail 正确拦下)。

## 对抗评审 / 取舍

- **jail 用词法规整而非 `canonicalize`**:新建文件不存在,canonicalize 会失败;词法 + `starts_with` 对「写前校验」既够严又不依赖 fs。符号链接逃逸是已知残余(词法不解析 symlink)—— 真 OS 隔离才根治,轻量层不追。
- **jail root = 进程 cwd**:`--cwd` 已 `set_current_dir`;agent 在项目目录内作业天然合理。
- **denylist 仍是「宁可漏拦不误伤」的 best-effort**,非安全边界;jail 才是写操作的硬边界。

## 下一步

`docs/iterations/CONTRACT-iteration-06-readonly.md`:`--read-only` 只读模式。本轮**未做**——其正确实现需把 flag 穿线过 `build_core` + 全部 `build_llm_agent_*` + main(只读时只 offer 只读工具 + 深度防御拒写),用 env 全局会致测试并发竞态。分轮做保每轮正确可测。
