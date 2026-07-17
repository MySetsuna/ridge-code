# CONTRACT — Iteration 25(分支工作区隔离:BoN 真实接入的物理前提)

## 目标

- **P0 新模块 `crates/agent/src/workspace.rs`**(agent 层,引擎零改动):
  - `Workspace` enum:`GitWorktree{dir}` / `ShadowCopy{dir}`,`dir()` 取路径。
  - `create_isolated(main, dest) -> io::Result<Workspace>`:main 为 git 仓库且 `git worktree add --detach` 成功 → GitWorktree;否则影子拷贝(跳过 `.git`/`target`/`.ridge`/`node_modules`)→ ShadowCopy。git 失败**静默回落**,不报错。
  - `merge_winner(main, branch_dir, modified: &BTreeSet<String>) -> Result<usize, String>`:逐文件整文搬运(自动建嵌套目录),任一失败**回滚已写**,主区要么全收要么原样。
  - `cleanup(main, ws)`:worktree remove --force(败则 remove_dir_all+prune 兜底)/ remove_dir_all;best-effort。
  - `is_git_repo(p)` 探测辅助。

## 边界(不做)

- 不动引擎(RunConfig 不加 cwd —— 见 guidance-25 驳回);不动 execute_tool_call / CLI 接线(BoN 端到端 = 下一刀);不做 OverlayFS/BranchFS/CRIU;不做二进制 diff(整文覆盖);不引新依赖(git 走 std::process::Command)。

## 确定性验收信号(禁计时/PTY/进程信号断言)

1. `cargo test --workspace` exit 0,新增覆盖(全走影子拷贝路径,git 不作测试前提):
   - 隔离:两分支各写 `a.txt` 不同内容,主区**期间不变**;merge 胜者后主区 = 胜者内容。
   - 嵌套:分支新增 `src/inner/mod.rs`,merge 自动建目录合入。
   - 回滚:modified 含分支中不存在的文件 → Err 且主区原样(含「先成后败回滚已写」情形)。
   - 清理:`cleanup` 后分支目录不存在。
   - `is_git_repo` 探测真假目录。
2. fmt + clippy `-D warnings` 净。

## 停机条件

- 触碰范围:`crates/agent/src/workspace.rs`(新)+ `lib.rs` 一行 mod 声明 + docs。越界即回退。
- 验收连续 2 次不过 → 熔断报告。
