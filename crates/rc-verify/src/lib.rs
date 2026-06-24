//! rc-verify — 客观验证器(M1)。详见 PLAN.md §7。
//!
//! 决定一个项目要跑哪些检查(优先 `ridge.toml`,否则按项目类型探测),
//! 逐条执行命令,产出 `Verdict`。失败时把命令输出装进 `Diagnostic` 回喂修复循环。

use anyhow::{Context, Result};
use rc_types::{Diagnostic, Verdict};
use serde::Deserialize;
use std::path::Path;

/// 每条检查输出回喂模型时的截断上限(字符)。编译错误的结论通常在末尾。
const MAX_DETAIL: usize = 4000;

/// 一条验证检查。
#[derive(Debug, Clone)]
pub struct Check {
    pub label: String,
    pub command: String,
}

/// 一组有序检查(全部通过才算 Pass)。
#[derive(Debug, Clone, Default)]
pub struct VerifyPlan {
    pub checks: Vec<Check>,
}

#[derive(Deserialize)]
struct RidgeToml {
    #[serde(default)]
    verify: VerifySection,
}

#[derive(Deserialize, Default)]
struct VerifySection {
    build: Option<String>,
    test: Option<String>,
    lint: Option<String>,
}

/// 决定项目要跑哪些检查:优先 `ridge.toml [verify]`,否则按项目类型探测。
pub fn resolve_plan(project_dir: &Path) -> VerifyPlan {
    plan_from_ridge_toml(project_dir).unwrap_or_else(|| detect_plan(project_dir))
}

fn plan_from_ridge_toml(project_dir: &Path) -> Option<VerifyPlan> {
    let text = std::fs::read_to_string(project_dir.join("ridge.toml")).ok()?;
    let parsed: RidgeToml = toml::from_str(&text).ok()?;
    let mut checks = Vec::new();
    if let Some(c) = parsed.verify.build {
        checks.push(Check { label: "build".into(), command: c });
    }
    if let Some(c) = parsed.verify.test {
        checks.push(Check { label: "test".into(), command: c });
    }
    if let Some(c) = parsed.verify.lint {
        checks.push(Check { label: "lint".into(), command: c });
    }
    if checks.is_empty() {
        None
    } else {
        Some(VerifyPlan { checks })
    }
}

fn detect_plan(project_dir: &Path) -> VerifyPlan {
    let mut checks = Vec::new();
    if project_dir.join("Cargo.toml").exists() {
        checks.push(Check { label: "build".into(), command: "cargo build".into() });
    } else if project_dir.join("package.json").exists() {
        // --if-present:缺少 build 脚本时跳过而非报错。
        checks.push(Check {
            label: "build".into(),
            command: "npm run build --if-present".into(),
        });
    }
    VerifyPlan { checks }
}

/// 跑完计划里的所有检查,产出 Verdict。
pub async fn verify(plan: &VerifyPlan, project_dir: &Path) -> Result<Verdict> {
    if plan.checks.is_empty() {
        return Ok(Verdict::Uncertain {
            note: "未发现可用的验证命令(无 ridge.toml,也未探测到 Cargo.toml / package.json)".into(),
        });
    }
    let mut reasons = Vec::new();
    for check in &plan.checks {
        let (ok, output) = run_check(&check.command, project_dir).await?;
        if !ok {
            reasons.push(Diagnostic {
                source: check.label.clone(),
                detail: truncate_tail(&output, MAX_DETAIL),
            });
        }
    }
    if reasons.is_empty() {
        Ok(Verdict::Pass)
    } else {
        Ok(Verdict::Fail { reasons })
    }
}

async fn run_check(command: &str, project_dir: &Path) -> Result<(bool, String)> {
    let mut cmd = if cfg!(windows) {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(command);
        c
    } else {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(command);
        c
    };
    cmd.current_dir(project_dir);
    let out = cmd
        .output()
        .await
        .with_context(|| format!("执行验证命令失败: {command}"))?;
    let mut combined = String::new();
    combined.push_str(&String::from_utf8_lossy(&out.stdout));
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok((out.status.success(), combined))
}

/// 保留末尾 max 个字符。
fn truncate_tail(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let tail: String = chars[chars.len() - max..].iter().collect();
    format!("[输出已截断,仅显示末尾 {max} 字符]\n{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_plan_is_uncertain() {
        let v = verify(&VerifyPlan::default(), &std::env::temp_dir())
            .await
            .unwrap();
        assert!(matches!(v, Verdict::Uncertain { .. }));
    }

    #[tokio::test]
    async fn passing_check() {
        let plan = VerifyPlan {
            checks: vec![Check { label: "ok".into(), command: "exit 0".into() }],
        };
        let v = verify(&plan, &std::env::temp_dir()).await.unwrap();
        assert!(v.is_pass());
    }

    #[tokio::test]
    async fn failing_check_reports_diagnostic() {
        let plan = VerifyPlan {
            checks: vec![Check { label: "boom".into(), command: "exit 1".into() }],
        };
        let v = verify(&plan, &std::env::temp_dir()).await.unwrap();
        match v {
            Verdict::Fail { reasons } => {
                assert_eq!(reasons.len(), 1);
                assert_eq!(reasons[0].source, "boom");
            }
            other => panic!("expected Fail, got {other:?}"),
        }
    }
}
