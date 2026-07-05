# ridge-code

成本优化的编码 agent CLI(Rust)。方向与架构见 [PLAN.md](./PLAN.md),动手指南见 [HANDOFF.md](./HANDOFF.md)。

## 现状(M4:MCP 客户端 + eval 闭环 + 强/弱编排)

完整流水线已跑通:**Planner(强模型)分解任务 → Router 按难度路由强/弱模型 → Worker 执行 → 客观验证(`cargo build`/`test` 等)+ 失败自动修复(强) → Reviewer(强)评审 → 输出结果与成本账单(强模型 token 占比)**。跨 provider 混合上线:难子任务/规划/修复/评审走强模型,其余走便宜的弱模型。

**M3 eval 闭环已落地(`rc-eval`):** 在内置小任务集上跑「全程强模型单 agent」基线 vs「混合编排」两种模式,用注入的隐藏验收测试客观判定,产出成功率 / 每任务成本(USD)/ 强模型 token 占比 / 延迟的对照表 + JSON 存档。支持真实 provider 与离线 StubProvider(零联网零成本验证管道)两套运行。设计见 `docs/superpowers/specs/2026-06-25-m3-eval-design.md`。

**M4 MCP 客户端已落地(`rc-mcp`):** 在 `~/.ridge/config.toml` 声明 `[[mcp]]` 外部服务器(子进程 stdio,基于官方 `rmcp`),启动时连上、把它们的工具以 `<name>__<tool>` 命名空间接进 Worker 工具集;调用按名路由回对应服务器。单个服务器连不上会告警跳过、不影响用内置工具跑任务。设计见 `docs/superpowers/specs/2026-07-05-m4-rc-mcp-design.md`。

**多供应商 / 多模型 + 原生 Anthropic:** provider 从「强/弱两段」升级为**命名注册表** `[[providers]]`——声明任意 N 个命名 provider,由 `[roles]` 按名选强/弱,可选 `[routing]` 按难度把 worker 路由到任意命名模型(用上 >2 个模型);Cost 仍按难度档位记账,eval 不受影响。新增**原生 Anthropic provider**(`kind = "anthropic"`,Messages API,`tool_use`/`tool_result`/`system` 归一化),strong=Claude 可直连原生、不必走兼容网关。旧 `[strong]`/`[weak]`/`[provider]` 写法全兼容。设计见 `docs/superpowers/specs/2026-07-05-provider-registry-design.md`。

**M4 ratatui 实时视图已落地:** `ridge-code --tui` 开启终端实时视图——顶部阶段 + 实时成本(强/弱 token、强占比),左侧子任务 DAG(○待办/▶运行/✓完成 + 难度/档位),右侧最近工具调用 + 事件日志;`q`/Ctrl-C 退出。编排器发结构化 `Event`(`rc-types`),不开 `--tui` 时行为与之前完全一致。设计见 `docs/superpowers/specs/2026-07-05-m4-ratatui-tui-design.md`。**至此 M0–M4 全部完成。**

**已知局限(驱动后续里程碑):** 子任务按规划顺序串行(并行 + worktree 隔离待做);`write_file` 整文件覆盖(结构化 patch 工具待做);eval 任务集目前仅 2 个内置小任务(待扩充更真实的任务);MCP 目前只接 tools、只做子进程 stdio 传输(resources/prompts、SSE/HTTP 传输待做)。

## 跑起来

方式一(推荐):从内置供应商目录挑,不用手写 base_url —

```bash
ridge-code providers                 # 列出供应商(anthropic/deepseek/qwen/zhipu/moonshot/openai/openrouter/groq/nvidia/ollama)
ridge-code models deepseek           # 看内置示例模型
ridge-code models deepseek --online  # 从 models.dev 拉真实当前模型(id+工具调用+上下文+价格)
ridge-code init deepseek             # 一键生成 ~/.ridge/config.toml(再设置对应 key 环境变量)
# 本地免费(无需付费 key):ridge-code init ollama qwen2.5-coder  → 先 `ollama pull qwen2.5-coder`
```

方式二:从模板手动改 —

```bash
cp config.example.toml ~/.ridge/config.toml   # 填 [strong]/[weak],或 [[providers]]+[roles] 命名注册表

# 提供 key(任一):
#   1) 在仓库根放 .env.local,写一行:RIDGE_API_KEY=nvapi-...(启动时自动加载)
#   2) export RIDGE_API_KEY=nvapi-...          (已导出的环境变量优先于 .env.local)
#   3) 在 config.toml 各 provider 段填 api_key

# 在仓库根目录运行(.env.local 按「启动目录」查找):
cargo run -p ridge-code -- --cwd /path/to/target/project "实现 add/mul 两个函数并各写一个单元测试"
cargo run -p ridge-code -- --tui --cwd /path/to/project "..."   # 实时终端视图(DAG/进度/成本)
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

## 构建 / 跨平台发布

本地出单二进制(release 已 strip + thin LTO):

```bash
cargo build --release -p ridge-code   # 产物:target/release/ridge-code[.exe]
```

跨平台发布走 GitHub Actions(`.github/workflows/release.yml`):打一个 `v*` 标签即为
五个目标构建并把归档(`.tar.gz` / `.zip`)上传到对应 Release。

```bash
git tag v0.1.0 && git push origin v0.1.0
```

覆盖目标:Linux x86_64 / aarch64(gnu)、macOS x86_64 / aarch64、Windows x86_64。
非本机目标(如 aarch64-linux)由 CI 自动经 `cross` 交叉编译。`.github/workflows/ci.yml`
在 push/PR 时于三平台跑 fmt + clippy + build + test,作为「能跨平台」的护栏。

## workspace 布局

| crate | 角色 | 里程碑 |
|---|---|---|
| `rc-types` | 纯数据类型(serde) | M0 ✅ |
| `rc-providers` | provider 抽象 + OpenAI 兼容 + 原生 Anthropic 实现 | M0 ✅ / Anthropic ✅ |
| `rc-tools` | 内置工具(fs / shell) | M0 ✅ |
| `rc-cli` | 二进制入口(薄壳:配置 + 报告) | M0 ✅ |
| `rc-verify` | 验证器(build/test/lint)+ Verdict | M1 ✅ |
| `rc-core` | 编排大脑(Planner/Router/Worker/Verify+Repair/Reviewer/Cost) | M2 ✅ |
| `rc-mcp` | MCP 客户端(rmcp,子进程 stdio + tools) | M4 ✅ |
| `rc-eval` | eval harness:基线 vs 编排成本-质量对照(真实/离线两套) | M3 ✅ |
