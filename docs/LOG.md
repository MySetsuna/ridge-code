# 全局工作日志(append-only)

跨迭代的长期记忆。开工前读末尾 5–10 条;完成大块工作后追加一条。最新在最上面。

---

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
