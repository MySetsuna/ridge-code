# NotebookLM 指导归档 + 对抗评审 —— 针对 Iteration 05(即 Iteration 06 计划 + Definition of Done)

- **时间戳**: 2026-07-14
- **来源**: NotebookLM「手搓agent」,基于《Ridge迭代报告 Iteration-05》(source `1725d010`)+ 全部来源

## NotebookLM 给的

**Iteration 06 优先级**:P0 官方 rmcp SDK → P1 config.toml → P2 /compact 上下文压缩 → P3 LLM token 流 → P4 单二进制分发。

**交付硬门槛**:① 危险命令拦截层(无沙箱阶段的 AST/正则防火墙,拦 `rm -rf /`/`mkfs` 等)② 物理信号验证(已有)③ 预算硬熔断默认开。

**可标已知限制发布**:重量级沙箱(用「权限门 + Git Worktree」先顶)、子任务并行、TUI。

**Definition of Done(6 条)**:① 调通至少一个第三方非 Rust MCP server(stdio)② 常驻 REPL + /compact + /reset ③ 副作用工具默认 [y/N] + Approver ④ 多轮 role=tool/tool_result ⑤ serde/bincode 检查点 kill-9 可恢复 ⑥ 每轮产 trace.json 含客观证据(cargo 的 stdout+exit code)。

## 对抗评审(不全信 NotebookLM)

- ✅ **采纳硬门槛**:危险命令拦截 = 最高价值、便宜、离线可测的安全项。**本轮已落地**(`tools::is_dangerous_command` + `execute_tool_call` 强制拦截,即使批准也拒绝)。
- ⚠️ **驳回 rmcp 当 P0**:DoD #1 要的是「能调真实第三方 MCP server」,**不等于必须用 rmcp**——我们自写 `StdioTransport` 已说 JSON-RPC,能调就满足 DoD;rmcp 是鲁棒性升级不是交付阻塞。rmcp 降级为可选。真正该先做的是它自己列的**硬门槛**(拦截 + 预算默认 + trace 审计),这些便宜且离线可测。
- 📝 **DoD 现状盘点**:③④⑤ 基本已具备(③权限门✅ ④role=tool✅ ⑤ FileCheckpointer+resume✅,待接进 REPL 会话)。缺口主要在:①真实 MCP server 验证、②/compact、⑥trace.json。
- **修正后的 Iteration 06 顺序**:P0 危险命令拦截(✅本轮)→ P1 trace.json 审计(DoD⑥,便宜)+ 预算默认 → P2 /compact(DoD②)→ P3 config.toml → 机会主义验证真实 MCP server(DoD①)。沙箱/token流/TUI/并行 = 已知限制。

见 `CONTRACT-iteration-06.md`。
