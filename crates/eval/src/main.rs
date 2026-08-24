use std::sync::Arc;

use eval::{run_eval_with_options, EvalCase, HarnessOptions};
use provider::{Completion, ScriptedProvider, ToolCall, Usage};
use std::time::Duration;

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

    let cli = Cli::parse(std::env::args().skip(1))?;
    if cli.help {
        println!("ridgecode-eval [--json] [--fail-on-unapproved] [--concurrency N] [--timeout-ms N] [--max-steps N] [--max-tokens N] [--marker TEXT] [--manifest PATH] [--recovery-fixture]");
        return Ok(());
    }
    let cases = if cli.recovery_fixture {
        recovery_fixture_cases()
    } else {
        vec![
            EvalCase::new("build-green", "make the build pass", pass_provider()),
            EvalCase::new(
                "write-then-pass",
                "implement add and test it",
                pass_provider(),
            ),
            EvalCase::new("stuck", "do the impossible", stuck_provider()),
        ]
    };

    let report = run_eval_with_options(cases, cli.options).await?;

    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "pass_rate": report.pass_rate(),
                "report": &report,
            }))?
        );
    } else {
        println!("== eval report ==");
        for r in &report.results {
            let mark = if r.approved { "PASS" } else { "FAIL" };
            let evidence = r.invariants.iter().filter(|item| item.passed).count();
            println!(
                "  [{mark}] {:<18} status={:?} steps={} tokens={} duration_ms={} evidence={}/{}",
                r.name,
                r.status,
                r.steps,
                r.tokens,
                r.duration_ms,
                evidence,
                r.invariants.len()
            );
        }
        println!(
            "\n== {}/{} passed  ({:.0}%)  total_tokens={} resumed={} executed={} ==",
            report.passed,
            report.total,
            report.pass_rate() * 100.0,
            report.total_tokens,
            report.resumed,
            report.executed
        );
    }
    if cli.fail_on_unapproved && (report.total == 0 || report.passed != report.total) {
        anyhow::bail!(
            "eval gate failed: {}/{} cases approved",
            report.passed,
            report.total
        );
    }
    Ok(())
}

struct Cli {
    help: bool,
    json: bool,
    fail_on_unapproved: bool,
    recovery_fixture: bool,
    options: HarnessOptions,
}

impl Cli {
    fn parse(args: impl IntoIterator<Item = String>) -> anyhow::Result<Self> {
        let mut cli = Self {
            help: false,
            json: false,
            fail_on_unapproved: false,
            recovery_fixture: false,
            options: HarnessOptions::default().require_approved(),
        };
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--help" | "-h" => cli.help = true,
                "--json" => cli.json = true,
                "--fail-on-unapproved" => cli.fail_on_unapproved = true,
                "--recovery-fixture" => cli.recovery_fixture = true,
                "--manifest" => {
                    let value = args
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--manifest needs a path"))?;
                    cli.options = cli.options.with_manifest_path(value);
                }
                "--concurrency" => {
                    let value = next_usize(&mut args, "--concurrency")?;
                    cli.options = cli.options.with_max_concurrency(value);
                }
                "--timeout-ms" => {
                    let value = next_u64(&mut args, "--timeout-ms")?;
                    cli.options = cli.options.with_case_timeout(Duration::from_millis(value));
                }
                "--max-steps" => {
                    let value = next_usize(&mut args, "--max-steps")?;
                    cli.options = cli.options.require_max_steps(value);
                }
                "--max-tokens" => {
                    let value = next_usize(&mut args, "--max-tokens")?;
                    cli.options = cli.options.require_max_tokens(value);
                }
                "--marker" => {
                    let value = args
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--marker needs a value"))?;
                    cli.options = cli.options.require_marker(value);
                }
                other => return Err(anyhow::anyhow!("unknown argument: {other}")),
            }
        }
        Ok(cli)
    }
}

fn next_usize(args: &mut impl Iterator<Item = String>, flag: &str) -> anyhow::Result<usize> {
    args.next()
        .ok_or_else(|| anyhow::anyhow!("{flag} needs a value"))?
        .parse()
        .map_err(|_| anyhow::anyhow!("{flag} expects a non-negative integer"))
}

fn next_u64(args: &mut impl Iterator<Item = String>, flag: &str) -> anyhow::Result<u64> {
    next_usize(args, flag).map(|value| value as u64)
}

fn recovery_fixture_cases() -> Vec<EvalCase> {
    (0..3)
        .map(|index| {
            EvalCase::new(
                format!("recovery-{index}"),
                "make the build pass",
                recovery_pass_provider(),
            )
            .with_revision("recovery-fixture-v1")
        })
        .collect()
}

fn recovery_pass_provider() -> Arc<ScriptedProvider> {
    Arc::new(
        ScriptedProvider::new(vec![
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
        ])
        .with_delay(Duration::from_millis(250)),
    )
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

#[cfg(test)]
mod tests {
    use super::Cli;

    #[test]
    fn cli_accepts_explicit_fail_closed_gate() {
        let cli = Cli::parse(["--json".to_string(), "--fail-on-unapproved".to_string()])
            .expect("valid CLI");
        assert!(cli.json);
        assert!(cli.fail_on_unapproved);
    }

    #[test]
    fn cli_accepts_manifest_and_recovery_fixture() {
        let cli = Cli::parse([
            "--manifest".to_string(),
            "target/quality/recovery.jsonl".to_string(),
            "--recovery-fixture".to_string(),
        ])
        .expect("valid CLI");
        assert!(cli.recovery_fixture);
        assert_eq!(
            cli.options
                .manifest_path
                .as_deref()
                .and_then(|path| path.to_str()),
            Some("target/quality/recovery.jsonl")
        );
    }
}
