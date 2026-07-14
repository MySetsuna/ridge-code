# CONTRACT —— Iteration 09:继续压「驾驭工程 + 用户交互」

> ⚠️ **本合同的 P0(多文件批量编辑)被用户 steer 插队顺延。** 用户 2026-07-14 直接要求先做 **web_search**(网络环境探测 + 引擎切换),已作为实际的 Iteration 09 交付,见 `2026-07-14-iteration-09.md`。本文件的批量编辑 / config.toml / TODO / 流式等条目**整体顺延到 Iteration 10**(见 `CONTRACT-iteration-10.md`)。

- **开工时间戳**: 2026-07-14
- **里程碑**: 从「修单点」跃迁到「重构级工程 + Claude Code 式交互」
- **依据**: `docs/iterations/2026-07-14-notebooklm-guidance-08.md`(NotebookLM + 对抗评审)+ 用户 steer(差距在驾驭工程/用户交互)

## 目标(End State)

在 Iter08 的精准 `edit_file` + diff 预览之上,让 agent 能**一次改多处/多文件并汇总成一份 diff 一次确认**(重构级),交互上让用户**看得见计划与进度**;并行落地框架的**配置面**(config.toml + 多 MCP)。

## 任务与验收信号

| 优先级 | 任务 | 确定性验收信号 | 状态 |
|---|---|---|---|
| **P0** | **多文件批量编辑**:`EditBuffer` 收集跨文件多处编辑 → 渲染**合并 diff** → **一次** approve → **原子应用**(全成/全不动) | 单测:buffer 收集 ≥2 文件共 N 处编辑 → 合并 diff 每文件含 `-/+` 段 → approve 后全部落盘;deny 或任一处 `old` 不唯一 → **全不改** | ⬜ |
| **P1** | `~/.ridge/config.toml`(provider/model/base_url/预算/多 `[[mcp]]`/skills_dir,env 仍覆盖)+ **多 MCP 并接**(现有 `StdioTransport`,**非 rmcp**) | 单测:解析含 2 个 `[[mcp]]` 的 config → 2 spec + provider;起 2 个假 stdio server → `list_tools` 并集、`server__tool` 命名空间不撞 | ⬜ |
| **P1** | **计划清单可视化**:planner 拆的子任务在 REPL 渲染成 `[x]/[ ]` markdown 清单,边做边勾 | 单测:3 子任务 + 完成 1 → 渲染出 1 勾 2 空 | ⬜ |
| **P2** | **流式增量输出**:reason 把模型输出按 chunk 经 `StreamEvent` 吐到 REPL(依赖 provider 层 SSE) | 单测:模拟分块回调 → REPL 收 ≥2 个增量文本事件且拼接 == 完整回复,不破坏 history。ponytail:provider 无 SSE 则回落整段 | ⬜ |
| **P2** | **Skills 按 description 匹配注入**:按任务关键词只注入相关技能,省 token | 单测:任务「写诗」→ 注入 haiku、不注入 rust-fix;无匹配 → 回落全部 | ⬜ |

## 「媲美 Claude Code」Definition of Done(勾选即交付)

- [x] 声明式技能加载(SKILL.md 注入)—— Iter07;**按 description 匹配** = 本轮 P2
- [x] 副作用工具 Approver 权限门 + 危险命令拦截
- [x] **精准编辑**(edit_file 唯一匹配)+ 可移植 search + 分段读 —— Iter08(驾驭工程)
- [x] **看着 diff 批准**(权限门 diff 预览)—— Iter08;**多文件汇总一次确认** = 本轮 P0
- [x] 多轮 role=tool 回灌 + 物理信号闭环(verify 认退出码)
- [x] 上下文治理 `/compact`
- [x] 结构化审计链 trace.json
- [ ] **插件式扩展:config 加 MCP server 不改源码即见新工具**(本轮 P1)
- [ ] **计划/进度可见**(TODO 清单)(本轮 P1)
- [ ] 毫秒级流式增量(本轮 P2,体验项)
- [ ] 跨会话鲁棒:kill-9 后 REPL 恢复(引擎级 resume 已具备,待接会话)—— backlog

## 已知限制(Beta 可先发,对抗评审后明确)

- **rmcp 替换自写 stdio** —— 自写传输已连真实 server;rmcp 为可选鲁棒性升级,**非交付阻塞**(Iter07/08 两次驳回其当硬门槛)。
- **重量级沙箱**(Docker/gVisor;WASM 不适合 shell/cargo)—— 先靠危险命令拦截 + 权限门 + diff 确认;真 OS 隔离标已知限制。
- **自动化触发器(Cron/Webhook)** = 范围扩张(常驻 daemon),延后。
- kill-9 REPL 会话恢复(引擎级已具备,待接会话)、标准存储库内建、子任务并行编排。

## 边界

不破坏现有 50 测试 + clippy/fmt 干净;密钥/敏感不写 trace、不写日志;config 解析失败降级到 env;批量编辑必须**原子**(失败不留半成品破坏编译)。
