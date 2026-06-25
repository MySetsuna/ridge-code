//! 离线集成:用 StubProvider 跑通内置任务的两种模式,零联网零成本。
//! 慢测(会在临时副本里跑 cargo build/test 子进程),但不烧 key。
//! 注意:本文件内的测试通过 set_current_dir 改进程全局 cwd,必须串行;请勿在此文件再加会改 cwd 的并行 #[tokio::test]。

use rc_eval::{runner, tasks, RunMode};
use rc_types::{Pricing, Rate};

#[tokio::test]
async fn offline_add_mul_both_modes_pass() {
    let pricing = Pricing {
        strong: Rate { in_per_mtok: 3.0, out_per_mtok: 15.0 },
        weak: Rate { in_per_mtok: 0.5, out_per_mtok: 1.5 },
    };
    let task = tasks::builtin_tasks().into_iter().find(|t| t.name == "add-mul").unwrap();

    for mode in [RunMode::Baseline, RunMode::Orchestrated] {
        let (strong, weak) = runner::offline_providers(&task, mode);
        let out = runner::run_one(&task, mode, strong, weak, &pricing, false).await;
        assert!(out.success, "{:?} 未通过验收: {:?}", mode, out.error);
        assert!(out.usd > 0.0, "{:?} 成本应为正", mode);
    }
}
