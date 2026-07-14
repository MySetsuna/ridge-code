# NotebookLM 指导归档 + 对抗评审 —— 针对 Iteration 08(即 Iteration 09 计划)

- **时间戳**: 2026-07-14
- **来源**: NotebookLM,基于《Ridge迭代报告 Iteration-08》(source `68a6b7cf`)+ 全部来源(含 Studio notes:loop engineering / context rot / MCP / rmcp)
- **conversation_id**: `68791fb7-659a-4ad6-a86c-beb7ac694781`

## NotebookLM 给的 Iteration 09

- **P0** 多文件批量编辑 + diff 汇总一次确认(硬门槛,降认知负载)
- **P1** config.toml + 多 MCP server 并接(硬门槛,插件式扩展)
- **P2** 流式增量 token 输出(体验核心)
- **P3** Skills 按 description 匹配注入(成本控制)
- **P4** 任务列表/TODO 可视化(可延后)
- 更新 DoD 硬门槛:批量原子确认、**「彻底放弃自写 stdio、完全由官方 rmcp 驱动」**、config.toml 持久配置、毫秒级流式、危险命令拦截、kill-9 resume。

## 对抗评审(不全信 NotebookLM)

- ✅ **采纳**:多文件批量编辑 + 汇总 diff 一次确认 —— 直接长在本轮 `edit_file` + `preview_call` 上,纯「驾驭工程 + 用户交互」,且**离线可测**(EditBuffer 原子应用)。列 Iteration 09 P0。
- ✅ **采纳**:config.toml + 多 MCP 并接(顺延自 08)—— 框架「插件式扩展」DoD 项,该落。
- ✅ **采纳**:Skills 按 description 匹配注入、TODO 清单可视化 —— 都便宜、离线可测;TODO 是用户明确点名的「用户交互」可见项,NotebookLM 把它排 P4 偏低,**据用户 steer 上调进 09 范围**。
- ⚠️ **再次驳回 rmcp 当硬门槛**(Iter07 已驳过一次):自写 `StdioTransport` **已实测连真实 server**(notebooklm-mcp 39 工具)。NotebookLM 引「10,000+ MCP server」证的是**协议 MCP** 的价值,而我的客户端**已经说 MCP JSON-RPC 2.0** —— 兼容性来自协议,不来自换 SDK。「彻底放弃自写 stdio、完全 rmcp 驱动」是**可选的鲁棒性升级,不是交付阻塞**。→ Iteration 09 用**现有传输**做多 server 并接;rmcp 标已知限制/可选。
- ⚠️ **下调流式输出:从「硬门槛」→ 体验增强**:NotebookLM 自己 Iter05 的 note 就把「LLM token 流」评为**低(体验)**,这轮却升成硬门槛,前后不一致。它还需 provider 层 SSE(现在是单请求/单响应),工作量不小。→ 列 09 但标**体验项、依赖 provider SSE**,不是「必须有才算交付」。
- 📝 **P0 排序**:NotebookLM 把 batch-edit 排在 config 前。这与用户 steer(重点=驾驭工程+用户交互)一致 —— batch-edit 两头都占,config 是纯 plumbing。→ **09 主线继续压驾驭工程+交互**(batch-edit / TODO / 流式),config.toml + 多 MCP 作**并行框架轨**紧随其后落地。

## 采纳后的 Iteration 09(见 CONTRACT-iteration-09)

P0 多文件批量编辑(EditBuffer + 汇总 diff + 一次原子确认)→ P1 config.toml + 多 MCP(现有 StdioTransport,非 rmcp)+ TODO 清单可视化 → P2 流式增量输出(provider SSE)+ Skills 按 description 匹配。rmcp、重量级沙箱、kill-9 REPL 恢复、自动化触发器 = backlog / 已知限制。
