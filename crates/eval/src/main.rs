use std::sync::Arc;

use eval::{
    compare_swebench_scores, run_eval_with_options, run_external_eval, run_swebench_export,
    score_swebench_reports, write_swebench_predictions, EvalCase, ExternalEvalOptions,
    ExternalEvalSuiteV1, HarnessOptions, SweBenchExportOptions, SweBenchInstanceV1,
    MACHINE_EVAL_SCHEMA_VERSION,
};
use provider::{Completion, ScriptedProvider, ToolCall, Usage};
use std::path::PathBuf;
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

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("external") {
        return run_external_cli(&args[1..]).await;
    }
    if args.first().map(String::as_str) == Some("swebench-export") {
        return run_swebench_export_cli(&args[1..]).await;
    }
    if args.first().map(String::as_str) == Some("swebench-score") {
        return run_swebench_score_cli(&args[1..]);
    }
    if args.first().map(String::as_str) == Some("swebench-compare") {
        return run_swebench_compare_cli(&args[1..]);
    }
    let cli = Cli::parse(args)?;
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
                "timeout_rate": report.timeout_rate(),
                "resumed_rate": report.resumed_rate(),
                "average_tokens": report.average_tokens(),
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
            "\n== {}/{} passed  ({:.0}%)  timeout={:.0}%  avg_tokens={:.1}  total_tokens={} resumed={} executed={} ==",
            report.passed,
            report.total,
            report.pass_rate() * 100.0,
            report.timeout_rate() * 100.0,
            report.average_tokens(),
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

async fn run_external_cli(args: &[String]) -> anyhow::Result<()> {
    let cli = ExternalCli::parse(args)?;
    if cli.help {
        println!("Usage: ridgecode-eval external --cases PATH --corpus-root PATH --ridgecode PATH [--max-turns N] [--timeout-ms N] [--budget-tokens N] [--read-only] [--allow-oauth] [--fail-on-unverified]\n\nReads a versioned external-eval suite JSON and writes only an ExperimentManifestV1 JSON result. The benchmark pass gate is the independent verifier, never RidgeCode's approved bit.");
        return Ok(());
    }
    let source = std::fs::read_to_string(&cli.cases)
        .map_err(|error| anyhow::anyhow!("cannot read external eval cases: {error}"))?;
    let suite: ExternalEvalSuiteV1 = serde_json::from_str(&source)
        .map_err(|error| anyhow::anyhow!("invalid external eval cases JSON: {error}"))?;
    if suite.schema_version != MACHINE_EVAL_SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported external eval schema version: {}",
            suite.schema_version
        );
    }
    let options = ExternalEvalOptions::new(cli.corpus_root, cli.ridgecode, suite.experiment_id)
        .with_max_turns(cli.max_turns)
        .with_timeout(cli.timeout)
        .with_budget_tokens(cli.budget_tokens)
        .read_only(cli.read_only)
        .require_api_key(!cli.allow_oauth);
    let report = run_external_eval(suite.cases, options).await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if cli.fail_on_unverified
        && (report.results.is_empty()
            || report.externally_verified_passed() != report.results.len())
    {
        anyhow::bail!(
            "external eval gate failed: {}/{} independently verified",
            report.externally_verified_passed(),
            report.results.len()
        );
    }
    Ok(())
}

async fn run_swebench_export_cli(args: &[String]) -> anyhow::Result<()> {
    let cli = SweBenchCli::parse(args)?;
    if cli.help {
        println!("Usage: ridgecode-eval swebench-export --instances PATH --workspaces-root PATH --ridgecode PATH --model-name NAME --predictions PATH [--limit N] [--max-turns N] [--timeout-ms N] [--budget-tokens N] [--allow-oauth]\n\nReads local SWE-bench dataset JSONL and emits official prediction JSONL. It does not calculate a SWE-bench score; run `swebench eval ... -p <predictions>` with the official Docker/cloud harness.");
        return Ok(());
    }
    let source = std::fs::read_to_string(&cli.instances)
        .map_err(|error| anyhow::anyhow!("cannot read SWE-bench instances: {error}"))?;
    let mut instances = Vec::new();
    for (line_number, line) in source.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let instance: SweBenchInstanceV1 = serde_json::from_str(line).map_err(|error| {
            anyhow::anyhow!(
                "invalid SWE-bench JSONL at line {}: {error}",
                line_number + 1
            )
        })?;
        instances.push(instance);
        if instances.len() == cli.limit.unwrap_or(usize::MAX) {
            break;
        }
    }
    let options = SweBenchExportOptions::new(&cli.workspaces_root, &cli.ridgecode, &cli.model_name)
        .with_max_turns(cli.max_turns)
        .with_timeout(cli.timeout)
        .with_budget_tokens(cli.budget_tokens)
        .require_api_key(!cli.allow_oauth);
    let predictions = run_swebench_export(instances, options).await?;
    let output = write_swebench_predictions(&cli.workspaces_root, &cli.predictions, &predictions)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 1,
            "kind": "swebench_prediction_export",
            "predictions": predictions.len(),
            "predictions_path": output,
            "official_score": serde_json::Value::Null,
        }))?
    );
    Ok(())
}

fn run_swebench_score_cli(args: &[String]) -> anyhow::Result<()> {
    let cli = SweBenchScoreCli::parse(args)?;
    if cli.help {
        println!("Usage: ridgecode-eval swebench-score --reports-root PATH\n\nReads official SWE-bench report.json files only and emits a resolved-rate scorecard. It does not invoke Docker or an LLM.");
        return Ok(());
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&score_swebench_reports(&cli.reports_root)?)?
    );
    Ok(())
}

fn run_swebench_compare_cli(args: &[String]) -> anyhow::Result<()> {
    let cli = SweBenchCompareCli::parse(args)?;
    if cli.help {
        println!("Usage: ridgecode-eval swebench-compare --baseline-reports PATH --candidate-reports PATH\n\nCompares two official SWE-bench report roots; positive deltas favour the candidate.");
        return Ok(());
    }
    let baseline = score_swebench_reports(&cli.baseline_reports)?;
    let candidate = score_swebench_reports(&cli.candidate_reports)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&compare_swebench_scores(baseline, candidate)?)?
    );
    Ok(())
}

struct Cli {
    help: bool,
    json: bool,
    fail_on_unapproved: bool,
    recovery_fixture: bool,
    options: HarnessOptions,
}

struct ExternalCli {
    help: bool,
    cases: PathBuf,
    corpus_root: PathBuf,
    ridgecode: PathBuf,
    max_turns: usize,
    timeout: Duration,
    budget_tokens: Option<usize>,
    read_only: bool,
    allow_oauth: bool,
    fail_on_unverified: bool,
}

struct SweBenchCli {
    help: bool,
    instances: PathBuf,
    workspaces_root: PathBuf,
    ridgecode: PathBuf,
    model_name: String,
    predictions: PathBuf,
    limit: Option<usize>,
    max_turns: usize,
    timeout: Duration,
    budget_tokens: Option<usize>,
    allow_oauth: bool,
}

struct SweBenchScoreCli {
    help: bool,
    reports_root: PathBuf,
}

impl SweBenchScoreCli {
    fn parse(args: &[String]) -> anyhow::Result<Self> {
        let mut help = false;
        let mut reports_root = None;
        let mut index = 0;
        while index < args.len() {
            match args[index].as_str() {
                "--help" | "-h" => help = true,
                "--reports-root" => {
                    let value = args
                        .get(index + 1)
                        .filter(|value| !value.starts_with('-'))
                        .ok_or_else(|| anyhow::anyhow!("--reports-root needs a path"))?;
                    reports_root = Some(PathBuf::from(value));
                    index += 1;
                }
                other => anyhow::bail!("unknown SWE-bench score argument: {other}"),
            }
            index += 1;
        }
        Ok(Self {
            help,
            reports_root: if help {
                PathBuf::new()
            } else {
                reports_root.ok_or_else(|| anyhow::anyhow!("--reports-root needs a path"))?
            },
        })
    }
}

struct SweBenchCompareCli {
    help: bool,
    baseline_reports: PathBuf,
    candidate_reports: PathBuf,
}

impl SweBenchCompareCli {
    fn parse(args: &[String]) -> anyhow::Result<Self> {
        let mut help = false;
        let mut baseline_reports = None;
        let mut candidate_reports = None;
        let mut index = 0;
        while index < args.len() {
            let value = |flag: &str| -> anyhow::Result<&String> {
                args.get(index + 1)
                    .filter(|value| !value.starts_with('-'))
                    .ok_or_else(|| anyhow::anyhow!("{flag} needs a path"))
            };
            match args[index].as_str() {
                "--help" | "-h" => help = true,
                "--baseline-reports" => {
                    baseline_reports = Some(PathBuf::from(value("--baseline-reports")?));
                    index += 1;
                }
                "--candidate-reports" => {
                    candidate_reports = Some(PathBuf::from(value("--candidate-reports")?));
                    index += 1;
                }
                other => anyhow::bail!("unknown SWE-bench compare argument: {other}"),
            }
            index += 1;
        }
        Ok(Self {
            help,
            baseline_reports: if help {
                PathBuf::new()
            } else {
                baseline_reports
                    .ok_or_else(|| anyhow::anyhow!("--baseline-reports needs a path"))?
            },
            candidate_reports: if help {
                PathBuf::new()
            } else {
                candidate_reports
                    .ok_or_else(|| anyhow::anyhow!("--candidate-reports needs a path"))?
            },
        })
    }
}

impl SweBenchCli {
    fn parse(args: &[String]) -> anyhow::Result<Self> {
        let mut help = false;
        let mut instances = None;
        let mut workspaces_root = None;
        let mut ridgecode = None;
        let mut model_name = None;
        let mut predictions = None;
        let mut limit = None;
        let mut max_turns = 80usize;
        let mut timeout = Duration::from_secs(20 * 60);
        let mut budget_tokens = None;
        let mut allow_oauth = false;
        let mut index = 0;
        while index < args.len() {
            let arg = args[index].as_str();
            let value = |flag: &str| -> anyhow::Result<&str> {
                args.get(index + 1)
                    .map(String::as_str)
                    .filter(|value| !value.starts_with('-'))
                    .ok_or_else(|| anyhow::anyhow!("{flag} needs a value"))
            };
            match arg {
                "--help" | "-h" => help = true,
                "--instances" => {
                    instances = Some(PathBuf::from(value("--instances")?));
                    index += 1;
                }
                "--workspaces-root" => {
                    workspaces_root = Some(PathBuf::from(value("--workspaces-root")?));
                    index += 1;
                }
                "--ridgecode" => {
                    ridgecode = Some(PathBuf::from(value("--ridgecode")?));
                    index += 1;
                }
                "--model-name" => {
                    model_name = Some(value("--model-name")?.to_string());
                    index += 1;
                }
                "--predictions" => {
                    predictions = Some(PathBuf::from(value("--predictions")?));
                    index += 1;
                }
                "--limit" => {
                    let parsed = next_usize(&mut args[index + 1..].iter().cloned(), "--limit")?;
                    if parsed == 0 {
                        anyhow::bail!("--limit must be a positive integer");
                    }
                    limit = Some(parsed);
                    index += 1;
                }
                "--max-turns" => {
                    max_turns = next_usize(&mut args[index + 1..].iter().cloned(), "--max-turns")?;
                    if max_turns == 0 {
                        anyhow::bail!("--max-turns must be a positive integer");
                    }
                    index += 1;
                }
                "--timeout-ms" => {
                    timeout = Duration::from_millis(next_u64(
                        &mut args[index + 1..].iter().cloned(),
                        "--timeout-ms",
                    )?);
                    if timeout.is_zero() {
                        anyhow::bail!("--timeout-ms must be a positive integer");
                    }
                    index += 1;
                }
                "--budget-tokens" => {
                    budget_tokens = Some(next_usize(
                        &mut args[index + 1..].iter().cloned(),
                        "--budget-tokens",
                    )?);
                    index += 1;
                }
                "--allow-oauth" => allow_oauth = true,
                other => anyhow::bail!("unknown SWE-bench export argument: {other}"),
            }
            index += 1;
        }
        if help {
            return Ok(Self {
                help,
                instances: PathBuf::new(),
                workspaces_root: PathBuf::new(),
                ridgecode: PathBuf::new(),
                model_name: String::new(),
                predictions: PathBuf::new(),
                limit,
                max_turns,
                timeout,
                budget_tokens,
                allow_oauth,
            });
        }
        let model_name = model_name.ok_or_else(|| anyhow::anyhow!("--model-name needs a value"))?;
        if model_name.trim().is_empty() {
            anyhow::bail!("--model-name needs a non-empty value");
        }
        Ok(Self {
            help,
            instances: instances.ok_or_else(|| anyhow::anyhow!("--instances needs a path"))?,
            workspaces_root: workspaces_root
                .ok_or_else(|| anyhow::anyhow!("--workspaces-root needs a path"))?,
            ridgecode: ridgecode.ok_or_else(|| anyhow::anyhow!("--ridgecode needs a path"))?,
            model_name,
            predictions: predictions
                .ok_or_else(|| anyhow::anyhow!("--predictions needs a path"))?,
            limit,
            max_turns,
            timeout,
            budget_tokens,
            allow_oauth,
        })
    }
}

impl ExternalCli {
    fn parse(args: &[String]) -> anyhow::Result<Self> {
        let mut help = false;
        let mut cases = None;
        let mut corpus_root = None;
        let mut ridgecode = None;
        let mut max_turns = 80usize;
        let mut timeout = Duration::from_secs(20 * 60);
        let mut budget_tokens = None;
        let mut read_only = false;
        let mut allow_oauth = false;
        let mut fail_on_unverified = false;
        let mut index = 0;
        while index < args.len() {
            let arg = args[index].as_str();
            let value = |flag: &str| -> anyhow::Result<&str> {
                args.get(index + 1)
                    .map(String::as_str)
                    .filter(|value| !value.starts_with('-'))
                    .ok_or_else(|| anyhow::anyhow!("{flag} needs a value"))
            };
            match arg {
                "--help" | "-h" => help = true,
                "--cases" => {
                    cases = Some(PathBuf::from(value("--cases")?));
                    index += 1;
                }
                "--corpus-root" => {
                    corpus_root = Some(PathBuf::from(value("--corpus-root")?));
                    index += 1;
                }
                "--ridgecode" => {
                    ridgecode = Some(PathBuf::from(value("--ridgecode")?));
                    index += 1;
                }
                "--max-turns" => {
                    max_turns = next_usize(&mut args[index + 1..].iter().cloned(), "--max-turns")?;
                    if max_turns == 0 {
                        anyhow::bail!("--max-turns must be a positive integer");
                    }
                    index += 1;
                }
                "--timeout-ms" => {
                    timeout = Duration::from_millis(next_u64(
                        &mut args[index + 1..].iter().cloned(),
                        "--timeout-ms",
                    )?);
                    if timeout.is_zero() {
                        anyhow::bail!("--timeout-ms must be a positive integer");
                    }
                    index += 1;
                }
                "--budget-tokens" => {
                    budget_tokens = Some(next_usize(
                        &mut args[index + 1..].iter().cloned(),
                        "--budget-tokens",
                    )?);
                    index += 1;
                }
                "--read-only" => read_only = true,
                "--allow-oauth" => allow_oauth = true,
                "--fail-on-unverified" => fail_on_unverified = true,
                other => anyhow::bail!("unknown external eval argument: {other}"),
            }
            index += 1;
        }
        if help {
            return Ok(Self {
                help,
                cases: PathBuf::new(),
                corpus_root: PathBuf::new(),
                ridgecode: PathBuf::new(),
                max_turns,
                timeout,
                budget_tokens,
                read_only,
                allow_oauth,
                fail_on_unverified,
            });
        }
        Ok(Self {
            help,
            cases: cases.ok_or_else(|| anyhow::anyhow!("--cases needs a path"))?,
            corpus_root: corpus_root
                .ok_or_else(|| anyhow::anyhow!("--corpus-root needs a path"))?,
            ridgecode: ridgecode.ok_or_else(|| anyhow::anyhow!("--ridgecode needs a path"))?,
            max_turns,
            timeout,
            budget_tokens,
            read_only,
            allow_oauth,
            fail_on_unverified,
        })
    }
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
    use super::{Cli, ExternalCli, SweBenchCli, SweBenchCompareCli, SweBenchScoreCli};
    use std::path::PathBuf;

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

    #[test]
    fn external_cli_requires_explicit_paths_and_is_api_key_first() {
        let cli = ExternalCli::parse(&[
            "--cases".to_string(),
            "suite.json".to_string(),
            "--corpus-root".to_string(),
            "corpus".to_string(),
            "--ridgecode".to_string(),
            "target/ridgecode".to_string(),
            "--timeout-ms".to_string(),
            "9000".to_string(),
            "--read-only".to_string(),
            "--fail-on-unverified".to_string(),
        ])
        .expect("valid external CLI");
        assert_eq!(cli.timeout, std::time::Duration::from_secs(9));
        assert!(cli.read_only && cli.fail_on_unverified);
        assert!(!cli.allow_oauth, "API-key-only is the safe default");
        assert!(ExternalCli::parse(&["--cases".to_string(), "suite.json".to_string()]).is_err());
    }

    #[test]
    fn swebench_cli_requires_standard_export_inputs() {
        let cli = SweBenchCli::parse(&[
            "--instances".to_string(),
            "dataset.jsonl".to_string(),
            "--workspaces-root".to_string(),
            "workspaces".to_string(),
            "--ridgecode".to_string(),
            "target/ridgecode".to_string(),
            "--model-name".to_string(),
            "ridgecode/glm-5.3".to_string(),
            "--predictions".to_string(),
            "workspaces/predictions.jsonl".to_string(),
            "--limit".to_string(),
            "2".to_string(),
        ])
        .expect("valid SWE-bench export CLI");
        assert_eq!(cli.limit, Some(2));
        assert!(!cli.allow_oauth, "API-key-only is the safe default");
        assert!(
            SweBenchCli::parse(&["--instances".to_string(), "dataset.jsonl".to_string()]).is_err()
        );
    }

    #[test]
    fn swebench_score_and_compare_clis_require_official_report_roots() {
        let score = SweBenchScoreCli::parse(&[
            "--reports-root".to_string(),
            "logs/evaluation/run/model".to_string(),
        ])
        .expect("score root is valid");
        assert_eq!(
            score.reports_root,
            PathBuf::from("logs/evaluation/run/model")
        );
        let compare = SweBenchCompareCli::parse(&[
            "--baseline-reports".to_string(),
            "baseline".to_string(),
            "--candidate-reports".to_string(),
            "candidate".to_string(),
        ])
        .expect("compare roots are valid");
        assert_eq!(compare.baseline_reports, PathBuf::from("baseline"));
        assert_eq!(compare.candidate_reports, PathBuf::from("candidate"));
        assert!(SweBenchScoreCli::parse(&[]).is_err());
        assert!(
            SweBenchCompareCli::parse(&["--baseline-reports".to_string(), "x".to_string()])
                .is_err()
        );
    }
}
