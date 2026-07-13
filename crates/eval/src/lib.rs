//! # eval —— agent 的 eval harness
//!
//! loop engineering 的核心是「验证者才是瓶颈」:光跑通一次不算数,要能**批量**度量成功率与成本。
//! 这里给最小的度量闭环:一组 case,每个跑一遍 agent,统计 pass-rate + token 成本。
//!
//! case 的 provider 可以是离线 `ScriptedProvider`(CI 零联网、确定性),也可以是真实模型
//! (量真实成功率/成本)。验收仍走 agent 的**确定性闸**(`approved`),不看模型自述。

use std::sync::Arc;

use agent::{build_llm_agent, AgentState};
use provider::LlmProvider;

/// 一个 eval case:名字 + 任务 + 用哪个 provider 跑。
pub struct EvalCase {
    pub name: String,
    pub task: String,
    pub provider: Arc<dyn LlmProvider>,
}

impl EvalCase {
    pub fn new(
        name: impl Into<String>,
        task: impl Into<String>,
        provider: Arc<dyn LlmProvider>,
    ) -> Self {
        Self {
            name: name.into(),
            task: task.into(),
            provider,
        }
    }
}

/// 单个 case 的结果。
#[derive(Clone, Debug)]
pub struct CaseResult {
    pub name: String,
    pub approved: bool,
    pub steps: usize,
    pub tokens: usize,
}

/// 整批 eval 的报告。
#[derive(Clone, Debug)]
pub struct EvalReport {
    pub results: Vec<CaseResult>,
    pub passed: usize,
    pub total: usize,
    pub total_tokens: usize,
}

impl EvalReport {
    pub fn pass_rate(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.passed as f64 / self.total as f64
        }
    }
}

/// 批量跑:每个 case 跑一遍 agent(确定性闸判 pass),聚合成功率 + 成本。
pub async fn run_eval(cases: Vec<EvalCase>) -> anyhow::Result<EvalReport> {
    let mut results = Vec::with_capacity(cases.len());
    let mut passed = 0;
    let mut total_tokens = 0;

    for case in cases {
        let app = build_llm_agent(case.provider)?;
        let out = app.invoke(AgentState::new(case.task)).await?;
        if out.approved {
            passed += 1;
        }
        total_tokens += out.total_tokens;
        results.push(CaseResult {
            name: case.name,
            approved: out.approved,
            steps: out.steps,
            tokens: out.total_tokens,
        });
    }

    let total = results.len();
    Ok(EvalReport {
        results,
        passed,
        total,
        total_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use provider::{Completion, ScriptedProvider, ToolCall, Usage};
    use serde_json::json;

    fn tool_call(cmd: &str) -> ToolCall {
        ToolCall {
            id: "1".to_string(),
            name: "run_shell".to_string(),
            arguments: json!({ "cmd": cmd }),
        }
    }

    #[tokio::test]
    async fn eval_reports_pass_rate_and_cost() {
        // pass:exit 0(确定性通过)→ 收尾。
        let pass = Arc::new(ScriptedProvider::new(vec![
            Completion {
                tool_calls: vec![tool_call("exit 0")],
                usage: Usage {
                    prompt_tokens: 10,
                    completion_tokens: 0,
                },
                ..Default::default()
            },
            Completion {
                text: "done".to_string(),
                ..Default::default()
            },
        ]));
        // fail:一直 exit 1(无进展熔断)→ 不通过。
        let fail = Arc::new(ScriptedProvider::new(
            (0..8)
                .map(|_| Completion {
                    tool_calls: vec![tool_call("exit 1")],
                    ..Default::default()
                })
                .collect(),
        ));

        let report = run_eval(vec![
            EvalCase::new("pass", "make build pass", pass),
            EvalCase::new("fail", "impossible", fail),
        ])
        .await
        .unwrap();

        assert_eq!(report.total, 2);
        assert_eq!(report.passed, 1);
        assert!((report.pass_rate() - 0.5).abs() < 1e-9);
        assert!(report.total_tokens >= 10);
    }
}
