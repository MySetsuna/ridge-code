# CONTRACT —— Iteration 06:收尾到「可交付」(安全硬门槛 + DoD 缺口)

- **开工时间戳**: 2026-07-14
- **里程碑**: M6 收尾 → 交付 Beta
- **依据**: `docs/iterations/2026-07-14-notebooklm-guidance-05.md`(NotebookLM + 对抗评审)

## 目标(End State)

补齐 NotebookLM 的**交付硬门槛**与 **Definition of Done** 的可低成本关闭的缺口,达到「可对外说交付了一个像 Claude Code 的 CLI agent(Beta)」。重量级沙箱等标为已知限制。

## 任务与验收信号

| 优先级 | 任务 | 确定性验收信号 | 状态 |
|---|---|---|---|
| **P0** | 危险命令拦截层:`tools::is_dangerous_command` + `execute_tool_call` 强制拦截(即使批准也拒绝) | 单测:`rm -rf /` / `mkfs` / fork 炸弹 → 拦截,`cargo build` / `rm -rf target/debug` 放行 | ✅ 本轮完成 |
| **P1** | trace.json 审计(DoD⑥):每轮把动作 + 工具输出 + 退出码写盘 | 跑一轮 → 生成 `trace.json`,含 run_shell 的 exit code 与 stdout | ⬜ |
| **P1** | 预算默认熔断:REPL/CLI 默认给一个 token 预算 | 超预算触发熔断(已有逻辑,补默认值 + 提示) | ⬜ |
| **P2** | `/compact` 上下文压缩(DoD②):把旧 history 压成摘要,保留关键进度 | `/compact` 后 `history` 长度显著减少,仍能续跑 | ⬜ |
| **P3** | `~/.ridge/config.toml`:provider/model/base_url/预算持久化 | `ridge config init` 生成默认文件;存在时自动加载 | ⬜ |
| **机会** | 验证真实第三方 MCP server(DoD①):`StdioTransport` 挂一个真实 stdio MCP server | `tools/list` 拿到该 server 的工具;调用成功 | ⬜(需真实 server) |

## Definition of Done(交付清单,勾选即达成最终目标)

- [x] 常驻 REPL + 多轮 + `/reset`(`/compact` 待补)
- [x] 副作用工具默认 `[y/N]` 权限门 + `Approver` 可配
- [x] 多轮 `role=tool` / `tool_result` 正确回灌
- [x] **危险命令拦截**(本轮)
- [x] serde 检查点 + resume(引擎级已具备;待接进 REPL 会话恢复)
- [ ] 每轮 trace.json 客观证据审计(P1)
- [ ] 调通至少一个真实第三方 MCP server(机会主义)

## 已知限制(Beta 可先发)

重量级沙箱(Docker/gVisor/WASM)—— 用「危险命令拦截 + 权限门 + Git Worktree」先顶;LLM token 逐字流(现为节点级流式);子任务并行;ratatui TUI;rmcp 替换自写 stdio。

## 边界

不破坏现有 ~39 测试 + clippy/fmt 干净;密钥永不写日志;trace.json 不写密钥。
