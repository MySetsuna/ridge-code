# 全局工作日志(append-only)

跨迭代的长期记忆。开工前读末尾 5–10 条;完成大块工作后追加一条。最新在最上面。

---

## 2026-07-13 · M4:独立模型 checker(maker≠checker 强形式)

- `build_llm_agent_reviewed(provider, mcp, reviewer)`:确定性 verify 通过后,再让一个**独立的** reviewer 模型看轨迹复核有没有作弊(删/跳测试、伪造输出),打回则 approved=false 回 reason。用**不同的** provider,别让写代码的模型自审。
- `build_core` 统一装配,verify 节点按有无 reviewer 分支;`review_request` 给 reviewer 铺 system(角色)+ user(任务+轨迹)。
- 测试:确定性闸通过但 reviewer REJECT(发现删测试)→ 最终不批准;reviewer APPROVE → 批准。
- `cargo test --workspace` = **30 项全绿**,clippy/fmt 干净。里程碑 M4 达成。

## 2026-07-13 · M2 接线:MCP 工具接进 agent

- agent 依赖 mcp;新增 `resolve_mcp(clients)`(各 initialize+list_tools,归一化成 ToolSpec + 命名空间路由表,降级不崩)+ `build_llm_agent_with(provider, McpTools)`。
- reason 把 内置 + MCP 工具一起 offer 给 LLM;act 按 `<server>__<tool>` 命名空间路由到对应 MCP 客户端(async),否则走内置工具。
- mcp 加 `FnTransport`(闭包充当传输,免 async-trait 造假服务器)。
- 端到端离线测:LLM 发 `ci__check` → act 路由到假 MCP 服务器 → 返回 `tests: passed` → verify approved。
- `cargo test --workspace` = **28 项全绿**,clippy/fmt 干净。**M2 从独立 crate 变成 agent 真能用的能力**。

## 2026-07-13 · M2 起步:最小 MCP 客户端(crates/mcp)

- 新增 `crates/mcp`:MCP = JSON-RPC 2.0。`McpClient` 做 initialize / tools/list / tools/call + `<server>__<tool>` 命名空间;`McpTransport` trait 把传输与协议解耦;`StdioTransport`(tokio 子进程,按 id 关联、跳通知)是生产传输。
- 离线测:`FakeTransport` 校验握手/列举/调用 + RPC 错误映射。协议核心 100% 离线可测。
- **对抗评审**:NotebookLM 荐官方 `rmcp` SDK,但其 stdio 传输离线无法单测、是重依赖 → 本轮先落可测的协议核心 + 最小 stdio 传输;要上生产把 `StdioTransport` 换 rmcp 即可(`McpTransport` 不变)。留待:把 MCP 工具并入 agent 的 `builtin_tool_specs` + 在 act 里按 `server__tool` 路由(async)。
- `cargo test --workspace` = **27 项全绿**,clippy/fmt 干净。工作区现 **5 crate**。

## 2026-07-13 · Iteration 03 P2(成本护栏 + 无进展检测 / 停机是设计的一半)

- provider `Completion` 加 `Usage`(prompt/completion tokens),两个 parse_response 从响应读用量。
- agent 加多层独立退出:`total_tokens`/`budget_tokens`(预算熔断)+ `stall`/`MAX_STALL`(无进展检测)。`must_stop` 汇总「回合上限 | 超预算 | 僵局」,shared 路由用它——scripted 路径两值恒 0,行为不变(对抗评审:预算放 agent 层,不进 langgraph 引擎)。
- 测试:预算耗尽 / 连续 MAX_STALL 轮输出不变 → 早于回合上限停机、approved=false。
- `cargo test --workspace` = **25 项全绿**,clippy/fmt 干净。

## 2026-07-13 · Iteration 03 P3(耐用执行 / M3 起步)

- `crates/langgraph` 新增 `FileCheckpointer`(每超步 append 一行 JSON,JSON Lines 版本日志)+ `CompiledGraph::resume(checkpoint)`(把主循环抽成 `run_loop`,invoke 从头 / resume 从快照共用)。`Checkpoint` 加条件 serde 派生。
- 测试:跑完落盘 → 全新 checkpointer 从磁盘读回超步 1 快照 → `resume` 续跑到同一终态(模拟崩溃后跨进程恢复,超步连续)。
- `cargo test --workspace` = **23 项全绿**,clippy/fmt 干净。提交前一条 aa881a6。
- 里程碑:M3(耐用执行)起步完成基础;bincode 落作后续优化。下一步候选:成本记账+预算熔断(agent 层)、无进展检测、或 M2(rmcp MCP 客户端)。

## 2026-07-13 · 工作流加对抗评审 + Iteration 03 P1(真实 HTTP provider)

- **工作流升级**:给 NotebookLM 驱动的循环加了**对抗评审**步骤(step 7)——不全信 NotebookLM(它是 maker 不是裁判,会张冠李戴引用、把概念放错层、过度设计)。关键决策要独立 checker + 高影响决策另起干净上下文当对抗评审员。写进全局 skill `notebooklm-iteration-loop` 与 `docs/WORKFLOW.md`。
  - 对抗评审实例(驳回):NotebookLM 建议把预算做成 `GraphError::BudgetExceeded`(app 层塞进通用引擎)+ 给「成本记账」引了不相关的 IoT 论文 → 驳回,预算归 agent 层。
- **Iter03 P1**:`crates/provider` 新增 `http::HttpClient` trait(分离传输与归一化)+ `ReqwestClient` + `OpenAiProvider`/`AnthropicProvider`(`build_request`→HTTP→`parse_response`)。测试:stub 传输走全链路(零网络)+ `mockito` 本地 server 校验 Authorization 头。首提交 9bd4464。
- 提交策略:用户授权直接提交 main(不走 PR)。

## 2026-07-13 · Iteration 02 P1 + Iteration 03 P0 完成

- **Iter02 P1**:新增 `crates/provider`(`LlmProvider` trait + Anthropic/OpenAI 工具调用**归一化**纯函数 + 离线 `ScriptedProvider`)。agent 新增 `build_llm_agent`:provider 吐结构化 tool_call → act 调真实 `tools` → verify 认 `exit 0` → approved,端到端离线可测。
- **闭环**:上传 Iter02 报告到 NotebookLM,取得 Iteration 03 指导(归档 `2026-07-13-notebooklm-guidance-02.md`)。核心:先做多轮 tool 结果回灌(离线可测),再接真实 HTTP(mockito),再成本熔断,最后 serde 落盘。
- **Iter03 P0**:`Message` 升级支持 assistant `tool_calls` 与 `role=tool` 结果;`openai::build_request` / `anthropic::build_request` 把统一历史铺成各自 wire(OpenAI role=tool + tool_call_id;Anthropic tool_use/tool_result 块 + 合并相邻同角色 + system 顶层)。纯函数,离线单测。
- 质量闸:`cargo test --workspace` = **19 项全绿**,clippy/fmt 干净。
- 下一步(Iter03 P1):真实 HTTP provider 客户端 —— 抽 `HttpClient` trait 分离传输与归一化,`mockito` 离线测。

## 2026-07-13 · Iteration 02 开工 + P0 完成

- 建 NotebookLM 驱动的迭代工作流(`docs/WORKFLOW.md` + `docs/iterations/`),沉淀为全局 skill `notebooklm-iteration-loop`。
- 上传 Iteration 01 报告到 NotebookLM,取得下一步指导:**先做物理闭环(P0 真实工具 → P1 真实 LLM)**。归档在 `docs/iterations/2026-07-13-notebooklm-guidance-01.md`。
- **P0 落地**:新增 `crates/tools`(真实 `read_file`/`write_file`/`run_shell`,跨平台),并把 `run_shell` 包成 agent 的 `shell_tool()`。工具层测试全绿。
- 决策:`run_shell` M1 阶段不做沙箱,只在受控命令上用;沙箱留到 harness 阶段。
- 下一步(Iteration 02 P1):`crates/provider` 的 `LlmProvider` trait + 真实实现,结构化 tool_call,把 `Brain` 换成真实模型。

## 2026-07-13 · Iteration 01(推倒重来)

- 删旧 ridge-code(rc-* 成本优化编码 agent),重建为 langgraph-rs 两层:`crates/langgraph`(手搓 Rust 版 LangGraph 引擎:GraphState+reducer、Pregel 超步+BSP、checkpoint 时间旅行、streaming、防跑飞)+ `crates/agent`(ReAct 循环 + maker-checker + 双保险停机,二进制 `ridge`)。
- 质量闸:`cargo test --workspace` 9 项全绿,clippy/fmt 干净。
- 旧码在 git 提交 `f0e65e6`,可 `git restore` 找回。
- 报告:`docs/REPORT-langgraph-rust.md`。
