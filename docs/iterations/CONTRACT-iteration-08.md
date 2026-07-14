# CONTRACT —— Iteration 08:插件式扩展(config + 多 MCP + Skills 匹配)

- **开工时间戳**: 2026-07-14
- **里程碑**: 通用框架「插件式扩展」可交付(Beta)
- **依据**: `docs/iterations/2026-07-14-notebooklm-guidance-07.md`(NotebookLM + 对抗评审)

## 目标(End State)

让「加新能力 = 改配置 / 加 SKILL.md,不改源码」真正成立:一处配置文件管 provider/预算/**多个 MCP server**/skills;Skills 按任务**匹配**注入。

## 任务与验收信号

| 优先级 | 任务 | 确定性验收信号 | 状态 |
|---|---|---|---|
| **P0** | `~/.ridge/config.toml`:provider/model/base_url/预算/`[[mcp]]` 多 server/skills_dir 一处配(env 仍可覆盖) | 单测:解析一份含 2 个 `[[mcp]]` 的 config → 得到 2 个 server spec + provider 设置 | ⬜ |
| **P0** | **多 MCP server 并接**(现有 `StdioTransport`,`resolve_mcp` 已支持 Vec)| 起 2 个 stdio server(可用假 server)→ `list_tools` 合并、命名空间不撞;实测再连一次真实 notebooklm-mcp | ⬜ |
| **P1** | **Skills 按 description 匹配注入**:按任务关键词只注入相关技能(而非全量),省 token | 单测:任务「写诗」→ 注入 haiku 技能、不注入 rust-fix 技能;无匹配 → 回落全部或空 | ⬜ |
| **P1** | 预算默认门禁:config 默认给 token 预算,超即熔断 | 单测:config 默认预算 + 跑飞任务 → 触发熔断(已有 `over_budget`,补默认装配) | ⬜ |

## 已知限制(Beta 可先发,对抗评审后明确)

- **重量级沙箱**(Docker/gVisor;WASM 不适合 shell/cargo)—— shell 安全先靠**危险命令拦截 + 权限门 + 一次性目录约定**;真 OS 隔离标已知限制。
- **rmcp 替换自写 stdio** —— 自写传输已连真实 server,rmcp 为可选鲁棒性升级。
- **自动化触发器(Cron/Webhook)** —— 会让 ridge 变常驻 daemon,范围扩张,延后。
- 子任务并行、kill-9 REPL 会话恢复、标准存储库内建、TUI、LLM token 流。

## 通用框架 Definition of Done(NotebookLM 7 条,勾选即交付)

- [x] 声明式技能加载(SKILL.md 注入)—— ✅ Iter07(**按 description 匹配** = 本轮 P1)
- [x] 副作用工具 Approver 权限门 + 危险命令拦截
- [x] 多轮 role=tool 回灌 + 物理信号闭环(verify 认退出码)
- [x] 上下文治理 `/compact`
- [x] 结构化审计链 trace.json
- [ ] **插件式扩展:config 加 MCP server 不改源码即见新工具**(本轮 P0)
- [ ] 跨会话鲁棒:kill-9 后 REPL 恢复(引擎级 resume 已具备,待接会话)—— backlog

## 边界

不破坏现有 42 测试 + clippy/fmt 干净;密钥/敏感不写 trace、不写日志;config 解析失败降级到 env。
