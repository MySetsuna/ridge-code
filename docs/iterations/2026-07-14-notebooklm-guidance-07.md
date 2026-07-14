# NotebookLM 指导归档 + 对抗评审 —— 针对 Iteration 07(即 Iteration 08 计划 + 通用框架 DoD)

- **时间戳**: 2026-07-14
- **来源**: NotebookLM,基于《Ridge迭代报告 Iteration-07》(source `b88739d5`)+ 全部来源(含 Studio 架构/沙箱 notes)

## NotebookLM 给的 Iteration 08

P0 rmcp 替换 + 多 server → P1 config.toml → P2 Skills 按需匹配 → P3 沙箱(WASM+Docker/gVisor)→ P4 标准存储库 → P5 并行+kill-9 恢复 → P6 自动化触发器。
硬门槛:WASM 轻量沙箱(默认执行环境)、危险命令拦截(已有)、rmcp 兼容、预算默认。
更新 DoD(7 条):插件式扩展(config 加 MCP 不改代码)、声明式技能按 description 匹配加载、隔离执行(Approver+沙箱)、跨会话鲁棒(kill-9 恢复)、物理信号闭环、上下文治理(/compact)、结构化审计链(trace.json)。

## 对抗评审(不全信 NotebookLM)

- ✅ **采纳**:Skills 按需匹配注入(便宜、省 token、离线可测)、config.toml + 多 MCP server、预算默认门禁。
- ⚠️ **驳回 rmcp 当 P0**:自写 `StdioTransport` 已实测连真实 server(notebooklm-mcp 39 工具);DoD「插件式扩展」要的是**配置驱动多 server**,用现有传输就能做,rmcp 是可选的鲁棒性升级不是交付阻塞。→ **P0 改为 config.toml + 多 MCP server**。
- ⚠️ **驳回 WASM-as-default-sandbox 当硬门槛**:**WASM 跑不了 `cargo build`/shell(需真 OS)**,NotebookLM 自己的 note 都说 WASM 只适合「预编译纯计算工具」。ridge 现无纯计算 skill,拿 WASM 沙箱 shell 是张冠李戴;它给的验收「run_shell 'rm -rf /' 在 WASM <5ms」不自洽。→ **重量级沙箱(尤其 WASM-as-default)延后/重排**;shell 安全 = 危险命令拦截(✅)+ 权限门(✅)+「一次性目录/容器里跑」的约定,真 OS 沙箱(Docker/gVisor)标**已知限制**。
- 📝 触发器(Cron/Webhook)= 让 ridge 变常驻 daemon,是**范围扩张**,Beta 不做,标已知限制。

## 采纳后的 Iteration 08(见 CONTRACT-iteration-08)

P0 `~/.ridge/config.toml`(provider/model/预算/MCP servers/skills 一处配)+ **多 MCP server 并接**(现有 StdioTransport)→ P1 **Skills 按 description 匹配注入**(而非全量,省 token)。沙箱(Docker,可选)、rmcp、触发器、标准存储库、kill-9 REPL 恢复 = backlog / 已知限制。
