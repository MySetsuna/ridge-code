# CONTRACT —— Iteration 12:发布打磨(UX 已达标,转向可交付)

- **开工时间戳**: 2026-07-15
- **里程碑**: Claude Code **核心用户体验全套达成**(CONTRACT-11 6/6),转向「让它能被别人用起来」
- **依据**: `docs/iterations/2026-07-15-notebooklm-guidance-11.md`(NotebookLM + 对抗评审)

## 目标(End State)

UX 不再是瓶颈。本轮做**安全、离线可测、能自主验收**的发布打磨,让 RidgeCode 有 1.0 的样子;把**需用户环境/决策的重活**(沙箱/rmcp)诚实标为已知限制。

## 任务与验收信号(发布打磨轨 —— 可自主做)

| 优先级 | 任务 | 确定性/可离线单测验收信号 | 状态 |
|---|---|---|---|
| **P0** | `--help`/`--version` + **官方样例** skills/config | `--version` 打印版本;`--help` 打印用法;单测 load_skills 解析 `samples/skills` | ✅ `c1fccda` |
| **P1** | 更多**官方样例技能**(triage / summarize / translate,含非编程域)| 单测:5 个样例都被 load_skills 解析、desc/body 非空 | ✅ `58875e0` |
| **P2** | 更丰富**斜杠命令**:`/tools`(列内置+MCP 工具)、`/model`(当前 provider/model/base_url) | GLM 实测:`/tools`→9 工具、`/model`→openai·glm-4.5-air·url | ✅ `d50eaeb` |
| **P2** | (可选)**标准存储库**:任务结束在 `.ridge/runs/<id>/` 落 contract/signals(loop-engineering 底座) | 单测:跑一个任务 → 目录物理生成、含结构化文件 | ⬜ |

## 已知限制(需**用户环境/决策**,不在自主 loop 内盲做)

- **重量级沙箱(Docker/gVisor)**:gVisor 仅 Linux、当前 Windows;重量级平台相关基建,无法离线/自主验收。需用户定技术选型(WASM 不适合 shell / Docker / gVisor)与环境。危险命令拦截 + 权限门 + diff 确认先顶着。
- **rmcp 替换自写 stdio**:自写已连真实 server(notebooklm-mcp / AnySearch),rmcp 是可选升级 + 有风险的依赖替换。
- **子智能体并行编排接进 REPL**:引擎支持 fan-out;串行够用,并行是性能上限非刚需。

## 边界

不破坏现有 81 测试 + clippy/fmt 干净;密钥不写 trace/日志/config;新样例必须能被 load_skills 解析。

## 交付状态

> **RidgeCode 已达成用户目标:媲美 Claude Code 的全部核心用户体验。** 后续为可选的发布打磨 + (需用户决策的)框架加固。
