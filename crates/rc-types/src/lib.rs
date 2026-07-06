//! ridge-code 的纯数据类型(serde),零业务逻辑。所有 crate 依赖它。
//! 对应 PLAN.md §3 / pi-web 的 @pi-web/protocol 角色。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 对话消息角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// 模型发起的一次工具调用(内部表示,provider 无关)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// 原始 JSON 参数字符串,待执行时解析。
    pub arguments: String,
}

/// 一条对话消息(内部表示)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// 仅 assistant 消息可能携带。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// 仅 tool 消息携带,对应被回复的 tool_call。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// tool 角色:回灌一次工具执行结果。
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

/// 给模型看的工具定义。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// 参数的 JSON Schema(object)。
    pub parameters: Value,
}

/// token 用量(成本记账基础,PLAN.md §3 CostRecord 的来源)。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// 模型一次回复的结果(M0:非流式;流式留待 M1)。
#[derive(Debug, Clone)]
pub struct Completion {
    pub message: Message,
    pub usage: Usage,
}

/// 一条验证诊断:哪个检查失败 + 它的输出(回喂修复循环当反馈)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub source: String,
    pub detail: String,
}

/// 客观验证结论(PLAN.md §3、§7)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Verdict {
    /// 全部检查通过。
    Pass,
    /// 有检查失败,reasons 携带可回喂模型的诊断。
    Fail { reasons: Vec<Diagnostic> },
    /// 无可用验证手段,无法判定。
    Uncertain { note: String },
}

impl Verdict {
    pub fn is_pass(&self) -> bool {
        matches!(self, Verdict::Pass)
    }
}

/// 子任务难度,驱动 Router 分流(PLAN.md §2、§3)。默认 Moderate。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Difficulty {
    Trivial,
    #[default]
    Moderate,
    Hard,
}

/// 模型档位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelTier {
    Weak,
    Strong,
}

/// Planner 产出的一个子任务(DAG 节点;M2 暂按顺序串行执行,deps 仅记录)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subtask {
    pub id: String,
    pub description: String,
    #[serde(default)]
    pub deps: Vec<String>,
    #[serde(default)]
    pub difficulty: Difficulty,
}

/// Reviewer 的结构化结论。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReviewResult {
    pub approved: bool,
    #[serde(default)]
    pub issues: Vec<String>,
}

/// 按档位累计的 token 成本账单(PLAN.md §3、§9)。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Cost {
    pub strong_in: u64,
    pub strong_out: u64,
    pub weak_in: u64,
    pub weak_out: u64,
}

impl Cost {
    pub fn add(&mut self, tier: ModelTier, in_tok: u32, out_tok: u32) {
        match tier {
            ModelTier::Strong => {
                self.strong_in += in_tok as u64;
                self.strong_out += out_tok as u64;
            }
            ModelTier::Weak => {
                self.weak_in += in_tok as u64;
                self.weak_out += out_tok as u64;
            }
        }
    }
    pub fn strong_tokens(&self) -> u64 {
        self.strong_in + self.strong_out
    }
    pub fn weak_tokens(&self) -> u64 {
        self.weak_in + self.weak_out
    }
    pub fn total(&self) -> u64 {
        self.strong_tokens() + self.weak_tokens()
    }
    /// 强模型 token 占比(PLAN §9 的关键省钱指标);total 为 0 时返回 0。
    pub fn strong_share(&self) -> f64 {
        let t = self.total();
        if t == 0 {
            0.0
        } else {
            self.strong_tokens() as f64 / t as f64
        }
    }
}

/// 每百万 token 的美元单价。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Rate {
    pub in_per_mtok: f64,
    pub out_per_mtok: f64,
}

impl Rate {
    /// 纯算术:给定输入/输出 token 数,折算成美元。
    pub fn cost_usd(&self, in_tok: u64, out_tok: u64) -> f64 {
        (in_tok as f64) / 1_000_000.0 * self.in_per_mtok
            + (out_tok as f64) / 1_000_000.0 * self.out_per_mtok
    }
}

/// 强 / 弱两档定价。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Pricing {
    pub strong: Rate,
    pub weak: Rate,
}

/// 编排所处阶段(TUI 顶部指示)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Planning,
    Executing,
    Verifying,
    Reviewing,
    Done,
}

/// Planner 产出的子任务摘要(给 TUI 列表用)。
#[derive(Debug, Clone)]
pub struct PlannedSubtask {
    pub id: String,
    pub description: String,
    pub difficulty: Difficulty,
}

/// 编排过程的实时事件(给 TUI / 报告;PLAN.md §3)。不开事件 sink 时不产生。
#[derive(Debug, Clone)]
pub enum Event {
    /// 进入某阶段。
    Phase(Phase),
    /// 规划完成,产出子任务列表。
    Planned(Vec<PlannedSubtask>),
    /// 某子任务开始执行(路由到的档位)。
    SubtaskStarted { id: String, tier: ModelTier },
    /// 某子任务执行完成。
    SubtaskDone { id: String },
    /// 一次工具调用。
    Tool { step: usize, name: String },
    /// 第 N 轮修复。
    Repair { round: usize },
    /// 评审结论。
    Review { approved: bool },
    /// 成本快照(每次模型回复后)。
    Cost(Cost),
    /// 自由文本备注(验证过/不过、评审跳过 等)。
    Note(String),
    /// 模型输出/思考内容。
    Message { role: String, content: String },
    /// 工具调用结果摘要。
    ToolResult { name: String, summary: String },
    /// 执行结果/产出。
    Output(String),
    /// 全流程结束。
    Finished,
}

#[cfg(test)]
mod pricing_tests {
    use super::*;

    #[test]
    fn rate_cost_usd_is_per_million() {
        let r = Rate {
            in_per_mtok: 3.0,
            out_per_mtok: 15.0,
        };
        // 100 万 in * $3 + 100 万 out * $15 = $18
        assert!((r.cost_usd(1_000_000, 1_000_000) - 18.0).abs() < 1e-9);
        assert!((r.cost_usd(0, 0)).abs() < 1e-9);
    }
}
