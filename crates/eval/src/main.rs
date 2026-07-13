use std::sync::Arc;

use eval::{run_eval, EvalCase};
use provider::{Completion, ScriptedProvider, ToolCall, Usage};

/// ridge-eval —— 离线 eval demo:跑一小组 case,打印成功率 + 成本。
/// 真实评测把 EvalCase 的 provider 换成真实模型即可(量真实成功率/成本)。
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();

    let cases = vec![
        EvalCase::new("build-green", "make the build pass", pass_provider()),
        EvalCase::new(
            "write-then-pass",
            "implement add and test it",
            pass_provider(),
        ),
        EvalCase::new("stuck", "do the impossible", stuck_provider()),
    ];

    let report = run_eval(cases).await?;

    println!("== eval report ==");
    for r in &report.results {
        let mark = if r.approved { "PASS" } else { "FAIL" };
        println!(
            "  [{mark}] {:<18} steps={} tokens={}",
            r.name, r.steps, r.tokens
        );
    }
    println!(
        "\n== {}/{} passed  ({:.0}%)  total_tokens={} ==",
        report.passed,
        report.total,
        report.pass_rate() * 100.0,
        report.total_tokens
    );
    Ok(())
}

fn tool_call(cmd: &str) -> ToolCall {
    ToolCall {
        id: "1".to_string(),
        name: "run_shell".to_string(),
        arguments: serde_json::json!({ "cmd": cmd }),
    }
}

/// 会通过的假模型:跑 exit 0 → 收尾。
fn pass_provider() -> Arc<ScriptedProvider> {
    Arc::new(ScriptedProvider::new(vec![
        Completion {
            tool_calls: vec![tool_call("exit 0")],
            usage: Usage {
                prompt_tokens: 12,
                completion_tokens: 3,
            },
            ..Default::default()
        },
        Completion {
            text: "done".to_string(),
            ..Default::default()
        },
    ]))
}

/// 会卡住的假模型:一直 exit 1 → 无进展熔断。
fn stuck_provider() -> Arc<ScriptedProvider> {
    Arc::new(ScriptedProvider::new(
        (0..8)
            .map(|_| Completion {
                tool_calls: vec![tool_call("exit 1")],
                usage: Usage {
                    prompt_tokens: 12,
                    completion_tokens: 3,
                },
                ..Default::default()
            })
            .collect(),
    ))
}
