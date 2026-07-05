//! 汇总 TaskOutcome → 指标与对照表 + JSON 存档。

use std::path::Path;

use anyhow::{Context, Result};
use rc_types::Cost;

use crate::{RunMode, TaskOutcome};

#[derive(Debug, Clone)]
pub struct ModeSummary {
    pub mode: RunMode,
    pub total: usize,
    pub passed: usize,
    pub total_usd: f64,
    pub strong_share: f64,
    pub total_ms: u128,
}

pub fn summarize(outcomes: &[TaskOutcome]) -> Vec<ModeSummary> {
    let mut summaries = Vec::new();
    for mode in [RunMode::Baseline, RunMode::Orchestrated] {
        let items: Vec<&TaskOutcome> = outcomes.iter().filter(|o| o.mode == mode).collect();
        if items.is_empty() {
            continue;
        }
        let mut agg = Cost::default();
        let mut total_usd = 0.0;
        let mut passed = 0usize;
        let mut total_ms = 0u128;
        for o in &items {
            agg.strong_in += o.cost.strong_in;
            agg.strong_out += o.cost.strong_out;
            agg.weak_in += o.cost.weak_in;
            agg.weak_out += o.cost.weak_out;
            total_usd += o.usd;
            total_ms += o.elapsed_ms;
            if o.success {
                passed += 1;
            }
        }
        summaries.push(ModeSummary {
            mode,
            total: items.len(),
            passed,
            total_usd,
            strong_share: agg.strong_share(),
            total_ms,
        });
    }
    summaries
}

pub fn render(summaries: &[ModeSummary]) -> String {
    let mut out = String::from("\n──────── ridge-code eval 报告 ────────\n");
    out.push_str("模式          通过/总   总成本(USD)   强模型占比   总耗时(ms)\n");
    for s in summaries {
        out.push_str(&format!(
            "{:<12}  {}/{}      ${:<10.4}  {:>6.0}%      {}\n",
            format!("{:?}", s.mode),
            s.passed,
            s.total,
            s.total_usd,
            s.strong_share * 100.0,
            s.total_ms,
        ));
    }
    let base = summaries.iter().find(|m| m.mode == RunMode::Baseline);
    let orch = summaries.iter().find(|m| m.mode == RunMode::Orchestrated);
    if let (Some(b), Some(o)) = (base, orch) {
        let saving = if b.total_usd > 0.0 {
            (b.total_usd - o.total_usd) / b.total_usd * 100.0
        } else {
            0.0
        };
        out.push_str(&format!(
            "\n对比:编排相对基线节省成本 {:.0}%;质量 通过 {}/{} vs {}/{}(越接近越好)\n",
            saving, o.passed, o.total, b.passed, b.total
        ));
    }
    out
}

pub fn write_json(outcomes: &[TaskOutcome], path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建目录 {} 失败", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(outcomes).context("序列化结果失败")?;
    std::fs::write(path, json).with_context(|| format!("写 {} 失败", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RunMode, TaskOutcome};
    use rc_types::Cost;

    fn outcome(mode: RunMode, success: bool, c: Cost, usd: f64) -> TaskOutcome {
        TaskOutcome {
            task: "t".into(),
            mode,
            success,
            cost: c,
            usd,
            elapsed_ms: 10,
            error: None,
        }
    }

    #[test]
    fn summarize_computes_rates_and_share() {
        let outcomes = vec![
            outcome(
                RunMode::Baseline,
                true,
                Cost {
                    strong_in: 100,
                    strong_out: 50,
                    weak_in: 0,
                    weak_out: 0,
                },
                0.5,
            ),
            outcome(
                RunMode::Orchestrated,
                true,
                Cost {
                    strong_in: 20,
                    strong_out: 10,
                    weak_in: 80,
                    weak_out: 40,
                },
                0.1,
            ),
        ];
        let s = summarize(&outcomes);
        assert_eq!(s.len(), 2);
        let base = s.iter().find(|m| m.mode == RunMode::Baseline).unwrap();
        assert_eq!(base.passed, 1);
        assert!((base.total_usd - 0.5).abs() < 1e-9);
        assert!((base.strong_share - 1.0).abs() < 1e-9);
        let orch = s.iter().find(|m| m.mode == RunMode::Orchestrated).unwrap();
        assert!((orch.strong_share - 0.2).abs() < 1e-9); // 30 / 150
    }
}
