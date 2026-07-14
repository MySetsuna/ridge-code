# CONTRACT —— Iteration 05:把内核变成「像 Claude Code 的 CLI」

- **开工时间戳**: 2026-07-14
- **里程碑**: M6 交互式演进(收尾冲刺)
- **依据**: `docs/iterations/2026-07-14-notebooklm-guidance-04.md`(NotebookLM + 对抗评审)

## 目标(End State)

agent 内核已完整并真实实测通过。本轮把它从「一次性批处理」变成**常驻、可对话、可观测、可控**的 CLI —— 用起来像 Claude Code。

## 任务与验收信号(可验证)

| 优先级 | 任务 | 确定性验收信号 | 状态 |
|---|---|---|---|
| **P0a** | `parse_response` 剥 `<think>...</think>` / 游离标签 | 单测:含 `<think>x</think>pong` 的响应 → `text == "pong"` | ✅ |
| **P0b** | 多轮 `role=tool` 回灌:`AgentState.history: Vec<Message>`,reason 推 assistant(tool_calls)、act 推 tool_result,`to_messages = [system] + history` | 单测:一次工具调用后 history 出现 `role=tool` + 匹配 `tool_call_id` + 带 tool_calls 的 assistant | ✅ |
| **P1** | 交互式 REPL:`ridge` 无任务参数 → 进对话循环,跨轮携带 `history`;`/exit` 退出、`/reset` 清上下文 | **GLM 实测**:`ridge>` 提示符、跑完回提示符、`/exit` 退出 | ✅ |
| **P2** | REPL 渲染引擎 `StreamEvent` 实时进度(哪个节点在跑) | **GLM 实测**:`· reason#1 · act#2 · reason#3 · verify#4` 边跑边流出 | ✅ |
| **P3** | 权限门:`Approver` trait,`write_file`/`run_shell` 执行前确认;REPL 用 stdin y/n,测试用 auto | 单测 AutoDeny → `permission denied`;**GLM 实测**:`run_shell` 前 `[y/N]` 确认 | ✅ |

## 边界(Constraints)

- 不破坏现有 34 测试 + clippy/fmt 干净。
- **LLM token 流、Docker/WASM 沙箱、ratatui TUI、~/.ridge/config.toml、rmcp** 进 backlog(本轮不做)。
- 权限门用可注入 `Approver`,保证离线可测(别只做 stdin 版)。
- 密钥永不写日志。

## 停机 / 授权阶梯

REPL 是 Level 0-1(人每轮驱动);权限门把危险操作卡在人手里 = 授权阶梯的地基。

## backlog(交付前的收尾,Iteration 06+)

M7 沙箱隔离(Docker/gVisor/WASM)、LLM token 流(provider SSE)、`~/.ridge/config.toml` + 更多斜杠命令、`/compact` 上下文压缩、ratatui `--tui`、MCP 真实传输换 rmcp、子任务并行编排、单二进制发布打磨。
