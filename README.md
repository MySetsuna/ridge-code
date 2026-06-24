# ridge-code

成本优化的编码 agent CLI(Rust)。方向与架构见 [PLAN.md](./PLAN.md),动手指南见 [HANDOFF.md](./HANDOFF.md)。

## 现状(M2:强/弱编排)

完整流水线已跑通:**Planner(强模型)分解任务 → Router 按难度路由强/弱模型 → Worker 执行 → 客观验证(`cargo build`/`test` 等)+ 失败自动修复(强) → Reviewer(强)评审 → 输出结果与成本账单(强模型 token 占比)**。

跨 provider 混合上线:难子任务/规划/修复/评审走强模型,其余走便宜的弱模型。

**已知局限(驱动后续里程碑):** 子任务按规划顺序串行(并行 + worktree 隔离待做);`write_file` 整文件覆盖(结构化 patch 工具待做);build/test 门槛盖不住"能编译但偏离规格"的问题、评审器自身也可能误判(语义正确性靠 M3 eval 度量改进)。

## 跑起来

```bash
cp config.example.toml ~/.ridge/config.toml   # 填 [strong] / [weak] 的 base_url 与 model

# 提供 key(任一):
#   1) 在仓库根放 .env.local,写一行:RIDGE_API_KEY=nvapi-...(启动时自动加载)
#   2) export RIDGE_API_KEY=nvapi-...          (已导出的环境变量优先于 .env.local)
#   3) 在 config.toml 各 provider 段填 api_key

# 在仓库根目录运行(.env.local 按「启动目录」查找):
cargo run -p ridge-code -- --cwd /path/to/target/project "实现 add/mul 两个函数并各写一个单元测试"
RUST_LOG=debug cargo run -p ridge-code -- "..."   # 详细日志
```

目标项目可放一个 `ridge.toml` 声明验证命令(否则按 `Cargo.toml`/`package.json` 自动探测):

```toml
[verify]
build = "cargo build"
test  = "cargo test"
# lint = "cargo clippy"
```

## workspace 布局

| crate | 角色 | 里程碑 |
|---|---|---|
| `rc-types` | 纯数据类型(serde) | M0 ✅ |
| `rc-providers` | provider 抽象 + OpenAI 兼容实现 | M0 ✅ |
| `rc-tools` | 内置工具(fs / shell) | M0 ✅ |
| `rc-cli` | 二进制入口(薄壳:配置 + 报告) | M0 ✅ |
| `rc-verify` | 验证器(build/test/lint)+ Verdict | M1 ✅ |
| `rc-core` | 编排大脑(Planner/Router/Worker/Verify+Repair/Reviewer/Cost) | M2 ✅ |
| `rc-mcp` | MCP 客户端(rmcp) | M4(占位) |
| `rc-eval` | eval harness(成本-质量度量) | M3(占位) |
