//! 跑「一个任务 × 一种模式」:复制 seed → 跑模式 → 注入隐藏验收 → 判定 → TaskOutcome。

use std::path::Path;
use std::time::Instant;

use anyhow::Result;
use rc_core::{Orchestrator, OrchestratorConfig};
use rc_providers::{LlmProvider, StubProvider};
use rc_types::{Completion, Cost, Message, Pricing, Role, ToolCall, Usage};
use rc_verify::{verify, Check, VerifyPlan};

use crate::tasks::EvalTask;
use crate::{RunMode, TaskOutcome};

/// 离线模式:按任务/模式构造脚本化假模型。
/// - 基线:强 = 一次性写入解答;弱不参与。
/// - 编排:强 = 返回计划(单 trivial 子任务);弱 = 写入解答。
pub fn offline_providers(
    task: &EvalTask,
    mode: RunMode,
) -> (Box<dyn LlmProvider>, Box<dyn LlmProvider>) {
    match mode {
        RunMode::Baseline => (
            Box::new(StubProvider::new(
                "stub-strong",
                vec![write_file_completion(&task.solution_files)],
            )),
            Box::new(StubProvider::new("stub-weak", vec![])),
        ),
        RunMode::Orchestrated => (
            Box::new(StubProvider::new(
                "stub-strong",
                vec![plan_completion(&task.plan_json)],
            )),
            Box::new(StubProvider::new(
                "stub-weak",
                vec![write_file_completion(&task.solution_files)],
            )),
        ),
    }
}

fn write_file_completion(files: &[(String, String)]) -> Completion {
    let tool_calls = files
        .iter()
        .enumerate()
        .map(|(i, (path, content))| ToolCall {
            id: format!("call_{i}"),
            name: "write_file".into(),
            arguments: serde_json::json!({ "path": path, "content": content }).to_string(),
        })
        .collect();
    Completion {
        message: Message {
            role: Role::Assistant,
            content: String::new(),
            tool_calls,
            tool_call_id: None,
        },
        usage: Usage {
            input_tokens: 200,
            output_tokens: 80,
        },
    }
}

fn plan_completion(plan_json: &str) -> Completion {
    Completion {
        message: Message {
            role: Role::Assistant,
            content: plan_json.to_string(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        },
        usage: Usage {
            input_tokens: 150,
            output_tokens: 40,
        },
    }
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

pub async fn run_one(
    task: &EvalTask,
    mode: RunMode,
    strong: Box<dyn LlmProvider>,
    weak: Box<dyn LlmProvider>,
    pricing: &Pricing,
    keep: bool,
) -> TaskOutcome {
    let started = Instant::now();
    let orig = std::env::current_dir().expect("读取当前目录失败");
    let safe_name = task.name.replace(['/', '\\', ' '], "_");
    let work = std::env::temp_dir().join(format!("rc-eval-{}-{:?}", safe_name, mode));

    let res = run_inner(task, mode, strong, weak, &work).await;

    // 务必恢复全局 cwd(rc-tools 用进程 cwd;后续报表写相对路径)。
    let _ = std::env::set_current_dir(&orig);
    if keep {
        tracing::info!(path = %work.display(), "保留工作副本");
    } else {
        let _ = std::fs::remove_dir_all(&work);
    }

    let elapsed_ms = started.elapsed().as_millis();
    match res {
        Ok((success, cost)) => {
            let usd = pricing.strong.cost_usd(cost.strong_in, cost.strong_out)
                + pricing.weak.cost_usd(cost.weak_in, cost.weak_out);
            TaskOutcome {
                task: task.name.clone(),
                mode,
                success,
                cost,
                usd,
                elapsed_ms,
                error: None,
            }
        }
        Err(e) => TaskOutcome {
            task: task.name.clone(),
            mode,
            success: false,
            cost: Cost::default(),
            usd: 0.0,
            elapsed_ms,
            error: Some(format!("{e:#}")),
        },
    }
}

async fn run_inner(
    task: &EvalTask,
    mode: RunMode,
    strong: Box<dyn LlmProvider>,
    weak: Box<dyn LlmProvider>,
    work: &Path,
) -> Result<(bool, Cost)> {
    if let Err(e) = std::fs::remove_dir_all(work) {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(e.into());
        }
    }
    copy_dir_all(&task.seed_dir, work)?;
    std::env::set_current_dir(work)?; // 让 rc-tools 文件工具落在副本里

    let orch = Orchestrator::new(
        strong,
        weak,
        work.to_path_buf(),
        OrchestratorConfig::default(),
    );
    let outcome = match mode {
        RunMode::Baseline => orch.run_single(&task.prompt).await?,
        RunMode::Orchestrated => orch.run(&task.prompt).await?,
    };

    // 注入隐藏验收测试,跑 cargo test 客观判定。
    copy_dir_all(&task.accept_dir, work)?;
    let plan = VerifyPlan {
        checks: vec![Check {
            label: "accept".into(),
            command: "cargo test".into(),
        }],
    };
    let verdict = verify(&plan, work).await?;
    Ok((verdict.is_pass(), outcome.cost))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_dir_all_copies_nested() {
        let base = std::env::temp_dir().join("rc-eval-copy-test");
        let _ = std::fs::remove_dir_all(&base);
        let src = base.join("src");
        std::fs::create_dir_all(src.join("a")).unwrap();
        std::fs::write(src.join("a/f.txt"), "hi").unwrap();
        let dst = base.join("dst");
        copy_dir_all(&src, &dst).unwrap();
        assert_eq!(std::fs::read_to_string(dst.join("a/f.txt")).unwrap(), "hi");
        let _ = std::fs::remove_dir_all(&base);
    }
}
