//! # langgraph —— 手搓的 Rust 版 LangGraph 核心引擎
//!
//! 对齐 LangGraph 的核心哲学(Pregel 拓扑图 + 状态机),但用 Rust 的强类型 + 无畏并发
//! 把它做得更确定、更快。三个要素 + 一个执行环:
//!
//! - **State**([`GraphState`]):全局共享状态 + reducer(节点返回 delta,`apply` 合并)。
//! - **Node**:`add_node` 注册的异步函数,`state -> Update`。
//! - **Edge**:[`StateGraph::add_edge`] 静态边 / [`StateGraph::add_conditional_edge`] 条件边。
//! - **Runtime**:[`CompiledGraph::invoke_with`] 的 Pregel 超步执行环(BSP 同步点 + 并发节点)。
//!
//! 附赠 [`MemoryCheckpointer`] 做「时间旅行」快照,以及 [`StreamEvent`] 做实时流式观测。
//!
//! ```no_run
//! use langgraph::{GraphState, StateGraph, END};
//!
//! #[derive(Clone)]
//! struct S { n: i64 }
//! impl GraphState for S {
//!     type Update = i64;
//!     fn apply(&mut self, u: i64) { self.n += u; } // reducer:累加
//! }
//!
//! # async fn demo() -> Result<(), langgraph::GraphError> {
//! let mut g = StateGraph::<S>::new();
//! g.add_node("inc", |_s: S| async { Ok::<_, std::convert::Infallible>(1) });
//! g.set_entry("inc");
//! g.add_edge("inc", END);
//! let app = g.compile()?;
//! let out = app.invoke(S { n: 0 }).await?;
//! assert_eq!(out.n, 1);
//! # Ok(()) }
//! ```

mod checkpoint;
mod graph;
mod state;

pub use checkpoint::{Checkpoint, Checkpointer, FileCheckpointer, MemoryCheckpointer};
pub use graph::{CompiledGraph, RunConfig, StateGraph, StreamEvent, END, START};
pub use state::{BoxError, GraphError, GraphState};

#[cfg(test)]
mod tests;
