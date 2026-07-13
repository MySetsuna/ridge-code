# NotebookLM 指导归档 —— 针对 Iteration 02 报告(即 Iteration 03 计划)

- **时间戳**: 2026-07-13
- **来源**: NotebookLM「手搓agent」,基于《Ridge迭代报告 Iteration-02》(source `e013202e`)+ 全部来源

---

## Iteration 03 优先级(先对多轮逻辑,再接网络,最后补治理)

| 优先级 | 任务 | 依赖 | 确定性验收信号 |
|---|---|---|---|
| **P0** | ③多轮上下文正确回灌 | Iter02 归一化层 | `to_messages`/build_request 单测:含 tool_result 的历史 → 符合各 provider 的 role/块序列 |
| **P1** | ①真实 HTTP provider 客户端 | 依赖 P0 | `mockito` mock server 返回 200,客户端解析出正确 ToolCall;CI 只跑 mock(exit 0),真网络测项 `#[ignore]` |
| **P2** | ④成本记账 + 预算熔断 | 依赖 P1 的 usage | `total_cost > limit` → `GraphError::BudgetExceeded` 停机 + 落快照 |
| **P3** | ②serde/bincode checkpoint 落盘 | 独立(M3) | kill 进程重启,从 bincode 恢复 state 且 superstep 连续 |

## 真实 HTTP provider 的离线可测方案

- **切分传输与归一化**:别在 `LlmProvider` trait 里硬编码 `reqwest`;内部再抽一个 `HttpClient` trait,测试注入 mock。
- **首选 mock server(`mockito`)**:比录制回放更易定义契约、校验请求头(如 `Authorization`)。
- 结构:`provider/openai.rs` 纯逻辑序列化;`tests/` 起 `mockito::Server` 验证请求体 + 解析。CI 默认只跑 mock 测项。

## 多轮 tool 结果回灌的消息结构

- **OpenAI**:assistant 消息带 `tool_calls`(含 id)→ 随后 `role=tool` 消息带 `tool_call_id` 匹配。
- **Anthropic**:assistant `content` 里 `tool_use` 块(含 id)→ 随后 **user** 消息里 `tool_result` 块引用 `tool_use_id`。
- **归一化层**:统一表达 `ToolResponse{ call_id, content, is_error }`,由各 provider 实现映射成上述格式。
- ✅ 本轮已按此落地 `Message`(assistant `tool_calls` + `role=tool`)+ `openai::build_request` / `anthropic::build_request`。

## 无进展检测

- **归属 agent 层**(引擎只看状态哈希,而「进展」是语义化的:报错是否变、文件行数是否增)。
- 信号:①连续 N 轮 `cargo check` 报错位置/错误码完全一致;②状态哈希在 A→B→A 震荡。
- 熔断:`verify` 维护 `stagnation_counter`,命中即 `approved=false` + 附原因,强制 END。

## 更新的里程碑地图

M1 物理闭环(本阶段,验收:agent 经真实 LLM 修一个真实语法错)→ M2 协议标准(rmcp,调独立 MCP server)→ M3 耐用执行(跨进程恢复/时间旅行)→ M4 权力隔离(独立 checker 拦截删测试作弊)。

**下一个可提 PR 的最小增量**:`feat: RidgeMessage tool_result 归一化 + 多轮上下文累积单测`(仅消息构建逻辑,零网络 IO,100% 离线可测)。✅ 本轮已完成。
