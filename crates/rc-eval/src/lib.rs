//! rc-eval — eval harness(M3 最小闭环)。详见 docs/superpowers/specs/2026-06-25-m3-eval-design.md。

pub mod reporter;
pub mod runner;
pub mod tasks;

use rc_types::Cost;
use serde::Serialize;

/// 一次运行采用的模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RunMode {
    /// 全程强模型单 agent(基线)。
    Baseline,
    /// 强/弱混合编排。
    Orchestrated,
}

/// 「一个任务 × 一种模式」的结果。
#[derive(Debug, Clone, Serialize)]
pub struct TaskOutcome {
    pub task: String,
    pub mode: RunMode,
    pub success: bool,
    pub cost: Cost,
    pub usd: f64,
    pub elapsed_ms: u128,
    pub error: Option<String>,
}
