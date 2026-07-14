# NotebookLM 指导归档 + 对抗评审 —— 针对 Iteration 04(即 Iteration 05 计划)

- **时间戳**: 2026-07-14
- **来源**: NotebookLM「手搓agent」,基于《Ridge迭代报告 Iteration-04》(source `127678f5`)+ 全部来源

## NotebookLM 给的 Iteration 05 优先级

P0 多轮 `role=tool` 回灌 → P1 交互式 REPL → P2 实时流式 → P3 权限门/确认 → P4 配置文件+斜杠命令 → P5 TUI(延后)。
必须有:多轮会话持久化、权限确认(human-in-loop)、上下文重置(Ralph/compact)、确定性验证。
锦上添花:复杂 TUI、Skill 系统、子任务极致并行。
收尾里程碑:M6 交互式演进 / M7 安全与沙箱 / M8 生产级分发。

## 对抗评审(不全信 NotebookLM)

- ✅ **采纳主线**:P0 role=tool → P1 REPL → P3 权限门。这条把「批处理内核」变「常驻状态机」的思路正确。
- ⚠️ **修正 1 —— 拆分「流式」**:NotebookLM 把流式当一件事,实为两件:①**引擎事件流**(哪个节点在跑,现成 `StreamEvent`,便宜)②**LLM token 流**(需 provider SSE,是新活)。本轮只做 ①(REPL 里显示实时进度),② 延后。别让「流式」膨胀。
- ⚠️ **修正 2 —— 沙箱延后,权限门优先**:它把权限门 + Docker/WASM 沙箱捆在 M7。**沙箱是跨平台大工程、离线几乎不可测**;**权限确认门用 5% 成本拿 80% 安全价值,且用可注入 `Approver` trait 离线可测**。本轮做权限门,沙箱进 backlog。
- ➕ **补 1 —— 剥 thinking 标签**:实测中 GLM 把 `</think>` 漏进 `content`(不影响闭环但输出脏)。`parse_response` 该剥 `<think>...</think>`,便宜,纳入 P0。
- 📝 role=tool 回灌**非阻塞**:GLM 实测已证明「assistant 文本铺轨迹」对简单任务够用;role=tool 是长多工具对话的鲁棒性改进,做但别过度投入。

## 采纳后的 Iteration 05 计划(见 CONTRACT-iteration-05)

P0 剥 think 标签 + 多轮 role=tool 回灌(接进 `build_llm_agent` 的 `to_messages`)→ P1 交互式 REPL(多轮上下文 + `/exit` `/reset`)→ P2 REPL 里渲染引擎 `StreamEvent` 实时进度 → P3 权限门(`Approver` trait,写文件/shell 前确认)。沙箱/TUI/配置文件/LLM token 流/rmcp 进 backlog。
