---
id: L1-PROJECT-001
level: L1
title: RidgeCode modular agent framework
status: VALID
code_targets:
  - Cargo.toml
  - README.md
  - AGENTS.md
  - docs/ARCHITECTURE.md
  - docs/REQUIREMENTS-SPEC.md
  - .spectree/config.json
  - package.json
  - package-lock.json
  - scripts/spectree-export.mjs
  - scripts/spectree-check.mjs
  - scripts/quality-gate.ps1
  - scripts/quality-gate.sh
  - scripts/install.ps1
  - scripts/install.sh
public_interface:
  - ridgecode binary
  - "workspace crates: langgraph, agent, provider, mcp, tools, eval"
---

# RidgeCode modular agent framework

RidgeCode 是单二进制 `ridgecode` 的模块化 agent 框架。当前代码基线以 Rust workspace 为权威，SpecTree 只声明可核验的边界、依赖与测试证据。

## Runtime contract

`crates/agent/src/main.rs::run_cli` 负责启动与模式分流；agent 图由 `build_llm_agent` 装配；图引擎以 `GraphState` reducer 和 Pregel/BSP 超步执行；provider 归一化模型协议；MCP 与 tools 提供外部能力。`approved` 与确定性约束仅是进程内回归信号；跨进程外部评测必须以独立 verifier 为准，SWE-bench 的 `resolved` 仅可由官方 harness 生成的 `report.json` 计分，二者不得互相替代。

## Safety and completion gates

- 图引擎受 `RunConfig::max_supersteps` 限制，避免无限运行。
- `verify` 只接受客观工具/测试信号，不接受模型自述。
- tools 层对写路径执行 `jail_path`，对灾难性 shell 命令执行 `is_dangerous_command` 硬拦截。
- 本树的 `code_targets` / `test_targets` 是当前代码证据，不等同于用户批准；批准与锁定仍走 SpecTree change 流程。
- `scripts/quality-gate.ps1`/`.sh` 先运行 `npm run spectree:check`，文档树与 Obsidian 投影漂移即阻断质量闸。
