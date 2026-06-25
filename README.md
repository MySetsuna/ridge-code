# ridge-code

成本优化的编码 agent CLI(Rust)。方向与架构见 [PLAN.md](./PLAN.md),动手指南见 [HANDOFF.md](./HANDOFF.md)。

## 现状(M3:eval 闭环 + 强/弱编排)

完整流水线已跑通:**Planner(强模型)分解任务 → Router 按难度路由强/弱模型 → Worker 执行 → 客观验证(`cargo build`/`test` 等)+ 失败自动修复(强) → Reviewer(强)评审 → 输出结果与成本账单(强模型 token 占比)**。跨 provider 混合上线:难子任务/规划/修复/评审走强模型,其余走便宜的弱模型。

**M3 eval 闭环已落地(`rc-eval`):** 在内置小任务集上跑「全程强模型单 agent」基线 vs「混合编排」两种模式,用注入的隐藏验收测试客观判定,产出成功率 / 每任务成本(USD)/ 强模型 token 占比 / 延迟的对照表 + JSON 存档。支持真实 provider 与离线 StubProvider(零联网零成本验证管道)两套运行。设计见 `docs/superpowers/specs/2026-06-25-m3-eval-design.md`。

**已知局限(驱动后续里程碑):** 子任务按规划顺序串行(并行 + worktree 隔离待做);`write_file` 整文件覆盖(结构化 patch 工具待做);eval 任务集目前仅 2 个内置小任务(待扩充更真实的任务);`rc-mcp`(M4)仍占位。

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

## 跑 eval(M3)

```bash
cargo run -p rc-eval -- --offline   # 离线:脚本化假模型跑通管道,零联网零成本(CI 友好)
cargo run -p rc-eval                # 真实:读 ~/.ridge/config.toml + key,真实调强/弱模型量成本-质量
```

输出「基线 vs 编排」对照表(成功率 / 总成本 USD / 强模型 token 占比 / 耗时),并写一份带时间戳的 JSON 到 `target/eval/`。真实模式需在 config 的 `[strong]`/`[weak]` 段填 `price_in`/`price_out`(USD/百万 token)才能算出非零成本。

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
| `rc-eval` | eval harness:基线 vs 编排成本-质量对照(真实/离线两套) | M3 ✅ |
