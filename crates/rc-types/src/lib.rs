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
        Self { role: Role::System, content: content.into(), tool_calls: Vec::new(), tool_call_id: None }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self { role: Role::User, content: content.into(), tool_calls: Vec::new(), tool_call_id: None }
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

/// 子任务难度,驱动 Router 分流(PLAN.md §2、§3)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Difficulty {
    Trivial,
    Moderate,
    Hard,
}

impl Default for Difficulty {
    fn default() -> Self {
        Difficulty::Moderate
    }
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
