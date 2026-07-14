# Ridge 的方向 —— 模块化、跨领域可扩展的通用 agent 框架

> 来源:NotebookLM 笔记本「手搓agent」的 Studio 产出(2026-07-14,13 篇 notes,核心是《基于 MCP 与 Rust 的模块化智能体架构指南》)。这是项目的**北极星**,后续每次迭代都对齐它。

## 一句话

Ridge 不只是「更会写代码的 CLI」,而是一个**通用 agent 执行框架**:**加新能力 = 加一个 MCP server 配置 或 一个 `SKILL.md` 文件,而不是改 Rust 源码**。目标是让它既能写代码(像 Claude Code),又能做**编程以外**的事(调研/日程/电商/生活服务)。

## 四层解耦架构

| 层 | 职责 | 现状 |
|---|---|---|
| **内核层** langgraph-rs 引擎 | 状态机 + 超步执行 + checkpoint 时间旅行,不感知业务 | ✅ 已有 |
| **协议层** MCP(USB-C 接口) | 深集成官方 `rmcp` / 自写 stdio,接入上万个 MCP server(dev / 通用 / 生活电商) | ✅ 已连真实 server;自写 stdio,rmcp 待换 |
| **知识层** 声明式 Skills | `~/.ridge/skills/*/SKILL.md` 动态加载领域知识/行为 → 做非编程任务不改代码 | ✅ 本轮落地(load_skills + 注入 system prompt) |
| **协作层** 多智能体编排 | orchestrator 拆 DAG → subagent;maker≠checker,敏感操作过权限门 | ✅ reviewer/planner/run_planned 已有 |
| **安全** 沙箱 | WASM(Wasmtime/Extism,<1ms)+ Docker/gVisor(预热池)双轨;三层隔离 | ⬜ 大工程;当前用权限门 + 危险命令拦截先顶 |

## 内置模块(让跨领域任务好用)

- **持久化检查点**(serde/bincode 落盘)—— 调研类长任务中断可续。✅ 引擎级已有(JSONL;bincode 待优化)。
- **上下文压缩**(Ralph 技术 / `/compact`)—— 防长会话上下文腐烂。✅ 已有。
- **标准存储库**(`Artifacts / Contracts / Logs`)—— 让不同领域的 loop「共享大脑」。✅ 已有(docs/iterations + LOG)。
- **自动化触发器**(Cron / Webhook)—— 变常驻助手。⬜ backlog。

## 原则

1. **定义胜于描述**:加新领域工具前,先用可衡量术语定义「完成」(如「CSV 导出成功」「邮件回执」)。
2. **最小权限**:用 MCP 能力令牌 + 权限门限制;别给 root。
3. **从 Triage 开始**:新领域先做只读、生成 Markdown 报告,稳了再放开写/执行权限。
4. **不信模型自述,只信物理信号**(退出码 / 哈希)—— 贯穿所有层。

## 收尾里程碑(对齐方向)

M7 沙箱(WASM + Docker/gVisor)· M8 Skills 生态 + 配置文件 + 自动化触发器 · rmcp 替换自写 stdio · 子任务并行编排。当前:**知识层(Skills)已通,框架雏形成型**。
