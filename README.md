# langgraph-rs

手搓的 **Rust 版 LangGraph** 引擎,以及跑在它之上的最小**编码 agent**。

- **`crates/langgraph`** —— 强类型 `StateGraph` + Pregel 超步执行环(BSP)+ checkpoint 时间旅行 + streaming。不含任何 LLM 概念,纯图引擎。
- **`crates/agent`** —— ReAct 循环(reason → act → verify)+ maker-checker 独立验证 + 停机护栏,装配成一张 langgraph 图。二进制名 `ridge`。

设计的「为什么」与来源(NotebookLM「手搓agent」笔记本)见 **[`docs/REPORT-langgraph-rust.md`](docs/REPORT-langgraph-rust.md)**。

## 快速开始

```bash
cargo test --workspace          # 全部单测(6 引擎 + 2 agent + 1 doctest)
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

## 路线图

阶段 1(引擎)与阶段 2(agent)已落地 MVP。阶段 3:`Brain` 接真实 LLM + 内嵌 MCP `rust-sdk` + checkpoint serde 落盘。阶段 4:子智能体真并行 + 沙箱隔离 + 可观测 + eval harness。详见报告 §6。
