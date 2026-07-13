# CONTRACT —— Iteration 03:多轮上下文 + 真实网络(M1 收尾)

- **开工时间戳**: 2026-07-13
- **里程碑**: M1 物理闭环(收尾)→ 迈向生产可用
- **依据**: `docs/iterations/2026-07-13-notebooklm-guidance-01.md` 之后的 NotebookLM Iteration 03 指导(见 LOG)

## 目标(End State)

让 agent 具备**真正的多轮工具循环**:工具结果按各厂商正确格式回灌,再接真实 HTTP provider,补齐成本护栏。

## 任务与验收信号(可验证)

| 优先级 | 任务 | 确定性验收信号 | 状态 |
|---|---|---|---|
| **P0** | provider 侧多轮消息构建:`Message` 支持 assistant `tool_calls` 与 `role=tool` 结果;`openai::build_request` / `anthropic::build_request` 把统一历史铺成各自 wire(OpenAI `role=tool`+`tool_call_id`;Anthropic `tool_use`/`tool_result` 块 + 合并相邻同角色 + system 顶层),纯函数 | `cargo test -p provider`:给定含 tool_call+tool_result 的多轮历史,两个 build_request 产出正确的角色/块序列 | ✅ 本轮完成 |
| **P1** | 真实 HTTP provider 客户端:切分「传输」与「归一化」,`reqwest` 打 Anthropic `/messages` + OpenAI `/chat/completions` | `cargo test -p provider` 全绿:捕获替身校验 auth 头/url/请求体;OpenAiProvider/AnthropicProvider 端到端解析 | ✅ 本轮完成 |
| **P2** | 成本记账 + 预算熔断(**agent 层**,不进引擎)+ 无进展检测 | `cargo test -p agent`:预算耗尽/连续 MAX_STALL 轮输出不变 → 早于回合上限停机、approved=false | ✅ 本轮完成 |
| **P3** | serde checkpoint 落盘 + 跨进程恢复(M3 起步) | `cargo test -p langgraph`:`FileCheckpointer` 落盘 JSONL + `CompiledGraph::resume` 从磁盘快照续跑到同一终态 | ✅ 本轮完成(JSON Lines;bincode 留作优化) |
| **P2** | 无进展检测(agent 层):verify 维护 `stagnation_counter`,连续 N 轮工具输出/报错不变 → 强制 END | 单测:工具输出连续 N 轮相同 → 在到 MAX_STEPS 之前就停机并标注原因 | ⬜ 下轮 |

## 边界(Constraints)

- 不破坏现有 17 项测试与 clippy/fmt 干净。
- **传输与归一化分层**:`LlmProvider` trait 不硬编码 reqwest;HTTP 细节藏在实现里,CI 保持离线绿。
- provider 边界:wire 类型/HTTP 客户端不外泄,对上只暴露 `Message`/`ToolCall`/`Completion`。
- 密钥永不写日志。

## 停机 / 预算

沿用 `MAX_STEPS`;P2 加 token 预算熔断 + 无进展检测,形成 loop engineering 要求的「多层独立退出」。

## 对抗评审留痕(不全信 NotebookLM)

- NotebookLM 荐 `mockito` 真打本地 server 测 HTTP。**实测在本机(有系统代理)请求挂起 ~127s 后 EOF**。→ 驳回真实 socket 测,改用**捕获型 `HttpClient` 替身**:同样校验 auth 头/url/请求体,但确定性、零网络、瞬时。移除 `mockito` 依赖。
- NotebookLM 荐 `GraphError::BudgetExceeded`(把 app 层预算塞进通用引擎错误)+ 给成本记账引了不相关 IoT 论文。→ 驳回,预算/成本归 agent 层,不进 langgraph。

## 授权阶梯

保持 **Level 2 (Draft)**。
