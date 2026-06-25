//! 内置 eval 任务集:每个任务自带 prompt、起始 seed、隐藏验收,
//! 以及离线模式所需的金标准解答(solution_files)与编排计划(plan_json)。

use std::path::PathBuf;

pub struct EvalTask {
    pub name: String,
    pub prompt: String,
    pub seed_dir: PathBuf,
    pub accept_dir: PathBuf,
    /// 离线 stub 写入的正确解答:(相对路径, 文件内容)。
    pub solution_files: Vec<(String, String)>,
    /// 离线编排模式 planner 预制回复(单个 trivial 子任务 → 路由到弱模型执行)。
    pub plan_json: String,
}

fn tasks_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tasks")
}

pub fn builtin_tasks() -> Vec<EvalTask> {
    let root = tasks_root();
    vec![
        EvalTask {
            name: "add-mul".into(),
            prompt: include_str!("../tasks/add-mul/prompt.txt").into(),
            seed_dir: root.join("add-mul/seed"),
            accept_dir: root.join("add-mul/accept"),
            solution_files: vec![(
                "src/lib.rs".into(),
                "pub fn add(a: i64, b: i64) -> i64 { a + b }\npub fn mul(a: i64, b: i64) -> i64 { a * b }\n".into(),
            )],
            plan_json: r#"[{"id":"s1","description":"实现 add 和 mul 函数","deps":[],"difficulty":"trivial"}]"#.into(),
        },
        EvalTask {
            name: "fix-compile".into(),
            prompt: include_str!("../tasks/fix-compile/prompt.txt").into(),
            seed_dir: root.join("fix-compile/seed"),
            accept_dir: root.join("fix-compile/accept"),
            solution_files: vec![(
                "src/lib.rs".into(),
                "pub fn double(x: i64) -> i64 { x * 2 }\n".into(),
            )],
            plan_json: r#"[{"id":"s1","description":"修复 double 使其编译且正确","deps":[],"difficulty":"trivial"}]"#.into(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_tasks_present_and_well_formed() {
        let tasks = builtin_tasks();
        assert_eq!(tasks.len(), 2);
        for t in &tasks {
            assert!(!t.prompt.trim().is_empty(), "{} prompt 为空", t.name);
            assert!(t.seed_dir.join("Cargo.toml").exists(), "{} 缺 seed Cargo.toml", t.name);
            assert!(t.accept_dir.join("tests/acceptance.rs").exists(), "{} 缺验收", t.name);
            assert!(!t.solution_files.is_empty(), "{} 缺 solution", t.name);
            // plan_json 必须能解析成非空数组
            let v: serde_json::Value = serde_json::from_str(&t.plan_json).unwrap();
            assert!(v.as_array().map(|a| !a.is_empty()).unwrap_or(false), "{} plan_json 非法", t.name);
        }
    }
}
