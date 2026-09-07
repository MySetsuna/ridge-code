# RidgeCode STC 交接记录

更新时间：2026-09-07
当前分支：`main`
基线：`a566ac8`（`v0.5.28`，提交前与 `origin/main` 同步）

## 当前结论

本轮 STC（实现 → 测试 → 证据）已完成本机可执行部分。运行时、工具执行、结构化证据、评测适配、TUI、恢复和发布构建均有确定性验证。计划尚未宣称完成，原因是官方 Harbor/Docker 与 Sonar/coverage 外部环境尚未就绪。

## 已完成能力

- provider/OpenAI-compatible 路由与 OAuth 选择隔离；Ark/GLM 配置走 `RIDGECODE_*` 环境变量。
- 多 tool call 顺序、结构化 `ToolResultV1`、MCP `isError`、网络工具和 sub-agent dispatch 结果均由执行控制流产出，不从展示文本猜测成功。
- `read_file/search` 保留有界窗口、命中数、截断标记；`edit_file/apply_edits` 支持 SHA-256 `expected_hash`，批量陈旧前置条件原子阻断。
- task contract、requirement evidence、workspace revision、确定性 reviewer gate、bounded history 和 durable facts。
- `RunStatus` 与 `verification` 已同时写入 `manifest.json` 和 `trace.json`；旧 `status/approved/halt_reason` 字段保持兼容。
- SWE-bench prediction 导出、官方 `report.json` 计分/比较、Harbor adapter 和 preflight。
- Windows ConPTY Input/Completion+Resize/Stress fixtures、A2A smoke、soak/recovery、release build 均通过。

## 已验证命令

```text
cargo test --workspace --locked --quiet       # 全部通过
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace --locked
cargo build --workspace --release --locked
cargo fmt --all -- --check
git diff --check
scripts/windows-pty-e2e.ps1 -InputFixture -TimeoutMs 12000
scripts/windows-pty-e2e.ps1 -CompletionFixture -ResizeProbe -TimeoutMs 12000
scripts/windows-pty-e2e.ps1 -StressFixture -TimeoutMs 12000
scripts/bounded-soak.ps1 -Iterations 3 -Concurrency 2
scripts/recovery-soak.ps1 -TimeoutMs 20000
cargo run -p agent --bin ridgecode -- a2a smoke
cargo run -p agent --bin ridgecode -- terminal doctor
```

关键结果：agent 263 tests、TUI 509 tests、eval 31 tests；bounded soak 3/3 轮通过；recovery 3/3 恢复通过；ConPTY fixtures 全部通过；release 二进制可执行。

## 当前阻塞

运行以下命令重新检查：

```powershell
.\scripts\harbor-preflight.ps1 -StoragePath C:\code\ridge-code
.\scripts\quality-preflight.ps1
```

当前已确认：Docker engine 不可达、Harbor CLI 不在 PATH、PATH 上的 Python 是不可执行 shim、C 盘约 64 GiB（要求 120 GiB）、`cargo-llvm-cov`/Sonar scanner/`SONAR_TOKEN` 缺失。WSL `Ubuntu-22.04` 存在但没有 cargo，因此 Linux PTY smoke 尚未执行。

## 恢复步骤

1. 在具备 Docker engine、Harbor CLI、可执行 Python 和至少 120 GiB benchmark 存储的 runner 上运行 Harbor preflight。
2. 注入 `RIDGECODE_BINARY_URL` 与对应小写 SHA-256、provider secret，执行 `harbor run -d "<dataset@version>" --agent eval.harbor.ridgecode_agent:RidgeCode`。
3. 在安装 `cargo-llvm-cov`、Sonar scanner 并注入 `SONAR_TOKEN` 的环境运行 `scripts/quality-gate.ps1` 或 `scripts/quality-gate.sh`。
4. 保存 Harbor verifier reward、官方 SWE-bench `report.json`、coverage 和 Sonar 结果；不得用 RidgeCode 的 `approved` 替代外部 verifier。
5. 若所有外部门通过，再将计划状态改为 complete；否则保持 blocked 并记录新的结构化 preflight 输出。

## 安全注意

API key 不写入仓库、manifest、trace、Harbor job 或交接文档。Harbor secret 只通过运行环境注入。不要为满足测试删除/清空测试文件，也不要回滚当前工作树中的用户文档变更。
