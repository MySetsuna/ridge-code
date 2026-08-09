//! # agent —— 跑在 [`langgraph`] 引擎上的最小编码 agent
//!
//! 把「loop engineering」的核心结构落成一张图:
//!
//! ```text
//!   START ─▶ reason ──(action=finish 或 到达回合上限)──▶ verify ──(approved / 到顶)──▶ END
//!             ▲   │                                          │
//!             │   └────────(其它 action)────▶ act ──────────┘(未过 → 回 reason)
//!             └──────────────────(reflection loop)───────────
//! ```
//!
//! 三个原则(见 `docs/REPORT-langgraph-rust.md`):
//! - **maker ≠ checker**:`reason`/`act` 生成,`verify` 独立判定,不让生成者给自己打分。
//! - **确定性验证**:`verify` 只认工具输出里的客观信号(测试是否 passed),不认模型自述。
//! - **停机是设计的一半**:硬回合上限 `MAX_STEPS` + approved 闸门,双保险防跑飞。
//!
//! `Brain` 是接真实 LLM 的接缝;这里给一个离线 `ScriptedBrain`,零联网即可跑通闭环。
//!
//! 富文本输出支持:
//! - 彩色格式化输出（ANSI 颜色）
//! - 表格和结构化展示
//! - 图片/视频/文件路径的直接展示
//! - 交互式输出增强

/// 富文本输出层(彩色 / 表格 / 媒体展示)—— 见 [`rich_output`]。
mod rich_output;
pub use rich_output::{
    Color, Formatter, MediaDisplay, MediaInfo, MediaType, RichOutput, TableDisplay,
};

mod state;
pub use state::*;
mod observe;
pub use observe::*;
mod knowledge;
pub use knowledge::*;
mod config;
pub use config::*;
mod auth;
pub use auth::*;
mod brain;
pub use brain::*;
mod exec;
pub use exec::*;
mod guard;
pub use guard::*;
mod context;
pub use context::*;
mod route;
pub use route::*;
mod signals;
pub use signals::*;
mod mcp_tools;
pub use mcp_tools::*;
mod graph;
pub use graph::*;
mod orchestrate;
pub use orchestrate::*;
mod goal;
pub use goal::*;
