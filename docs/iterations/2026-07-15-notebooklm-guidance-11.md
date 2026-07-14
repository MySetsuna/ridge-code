# NotebookLM 指导归档 + 对抗评审 —— 针对 Iteration 11(即 Iteration 12 计划)

- **时间戳**: 2026-07-15
- **来源**: NotebookLM,基于《Ridge迭代报告 Iteration-11》(source `433474ee`)+ loop-engineering / MCP notes
- **conversation_id**: `68791fb7-659a-4ad6-a86c-beb7ac694781`

## NotebookLM 给的 Iteration 12(「硬化内核,准备发布」)

P0 Docker/gVisor 沙箱 → P1 标准存储库(Artifacts/Contracts/Logs)→ P2 rmcp 替换自写 stdio → P3 发布打磨 + 官方样例 → P4 子智能体并行(延后)。结论:UX 已封顶,转向工程加固 + 标准化 + 发布。给了 1.0 的 7 条 DoD。

## 对抗评审(不全信 NotebookLM)

- ✅ **采纳大方向**:UX 已达 Claude Code → 转发布打磨 + 官方样例(安全、离线可测、当下最高性价比)。本轮已落地 `--help/--version` + `samples/`(skills/config/README)。
- ⚠️ **驳回 Docker/gVisor 沙箱当 P0(尤其在自主 loop 里)**:①**gVisor 只跑 Linux**,当前是 **Windows** 机;②Docker 沙箱是**重量级、平台相关、无法离线/自主验收**的基建(它给的验收「宿主机 rm -rf / 物理无损」我没法在自主循环里真跑真验);③危险命令拦截(✅)+ 权限门(✅)+ diff 确认(✅)已挡住 80%。→ **沙箱标已知限制,需用户的环境与技术选型决策**(WASM 跑不了 shell、Docker/gVisor 选谁、预热池),不适合无人值守自主做。
- ⚠️ **再次驳回 rmcp 当优先**:自写 `StdioTransport` **已连真实 server**(notebooklm-mcp、AnySearch via mcp-remote)。rmcp 是**可选鲁棒性升级 + 有风险的依赖替换**,不是门槛。→ backlog。
- 📝 **标准存储库**:可离线测、中等价值(更服务于多 loop 自治,非 CLI 交互刚需)。可选做。
- ✅ 引用这次基本干净(没再引 Ruh.AI 脏页)。

## 采纳后的 Iteration 12(见 CONTRACT-iteration-12)

**核心目标已达成**(Claude Code UX 全套)。本轮起转「发布打磨」安全轨:`--help/--version` + 官方样例 skills/config(✅ 本轮)→ 可继续:更多样例技能、更丰富斜杠命令、docs/README 打磨、(可选)标准存储库。**Docker 沙箱 / rmcp / 子智能体并行 = 需用户环境与决策的框架轨,标已知限制,不在自主 loop 内盲做。**
