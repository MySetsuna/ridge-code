# langgraph-rs

手搓的 **Rust 版 LangGraph** 引擎,以及跑在它之上的**编码 agent**,目标是媲美 Claude Code。
用 NotebookLM 驱动持续迭代(见 [`docs/WORKFLOW.md`](docs/WORKFLOW.md)),关键决策过对抗评审。

- **`crates/langgraph`** —— 强类型 `StateGraph` + Pregel 超步执行环(BSP)+ checkpoint(内存 + `FileCheckpointer` 落盘 + `resume` 耐用执行)+ streaming。纯图引擎,零 LLM 概念。
- **`crates/provider`** —— `LlmProvider` trait + Anthropic/OpenAI 工具调用归一化 + 多轮请求构建 + token 用量 + 真实 HTTP 客户端(传输/归一化解耦)+ 离线 `ScriptedProvider`。
- **`crates/tools`** —— 真实文件读写 + 跨平台 shell。
- **`crates/mcp`** —— 最小 MCP 客户端(JSON-RPC:initialize/tools/list/tools/call + `server__tool` 命名空间)+ 可插拔传输。
- **`crates/agent`** —— ReAct 循环(reason → act → verify),装配成 langgraph 图。二进制 `ridge`。
  - 结构化 tool_call 驱动真实工具 + MCP 工具;**maker≠checker**(确定性闸 + 可选独立模型 reviewer 抓作弊);
  - 多层停机:回合上限 / token 预算熔断 / 无进展检测;`plan()` 目标→子任务分解。

设计的「为什么」与来源(NotebookLM「手搓agent」笔记本)见 **[`docs/REPORT-langgraph-rust.md`](docs/REPORT-langgraph-rust.md)**。

## 快速开始

```bash
cargo test --workspace          # 全部单测(32 项:引擎/provider/tools/mcp/agent + doctest)
cargo run -p agent --bin ridge  # 跑通 agent 闭环,打印轨迹 + 每个超步的 checkpoint
```

预期输出(节选):

```
== agent trace ==
  reason#1: -> write_code
  act: write_code -> tests: 1 failed
  reason#2: -> fix
  act: fix -> tests: passed
  reason#3: -> finish
  verify: PASS (deterministic gate)
== result: approved=true steps=3 ==
```

## 引擎用法

```rust
use langgraph::{GraphState, StateGraph, END};

#[derive(Clone)]
struct S { n: i64 }
impl GraphState for S {
    type Update = i64;
    fn apply(&mut self, u: i64) { self.n += u; } // reducer:累加而非覆盖
}

let mut g = StateGraph::<S>::new();
g.add_node("inc", |_s: S| async { Ok::<_, std::convert::Infallible>(1) });
g.set_entry("inc");
g.add_edge("inc", END);
let out = g.compile()?.invoke(S { n: 0 }).await?; // out.n == 1
```

四个要素:**State**(`GraphState` + reducer)、**Node**(`add_node` 异步函数)、**Edge**(`add_edge` / `add_conditional_edge`)、**Runtime**(`invoke` / `invoke_with`,后者可挂 checkpointer 与 streaming)。

## 里程碑现状

| 里程碑 | 状态 |
|---|---|
| M1 物理闭环(真实工具 + 真实 LLM 结构化 tool_call + HTTP 传输) | ✅ |
| M2 MCP 协议(客户端 + agent 命名空间路由) | ✅ 核心 |
| M3 耐用执行(checkpoint 落盘 + resume) | ✅ 起步(JSONL;bincode 待优化) |
| M4 独立模型 checker(抓作弊) | ✅ |
| M5 规划器(目标→子任务) | ✅ 核心(分解;子任务 DAG 编排待接) |
| 停机护栏(回合上限 / 预算 / 无进展) | ✅ |

待接:MCP 真实 stdio(可换官方 `rmcp`)、子任务并行编排、沙箱隔离、流式 TUI、eval harness。详见 `docs/WORKFLOW.md` 与 `docs/iterations/`。
