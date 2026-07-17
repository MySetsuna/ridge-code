use super::*;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Counter {
    n: i64,
    trail: Vec<String>,
}

enum Up {
    Add(i64),
    Visit(String),
    Both(i64, String),
}

impl GraphState for Counter {
    type Update = Up;
    fn apply(&mut self, u: Up) {
        match u {
            Up::Add(x) => self.n += x,
            Up::Visit(s) => self.trail.push(s), // append reducer
            Up::Both(x, s) => {
                self.n += x;
                self.trail.push(s);
            }
        }
    }
}

#[tokio::test]
async fn linear_runs_in_order() {
    let mut g = StateGraph::<Counter>::new();
    g.add_node("a", |_s| async {
        Ok::<_, Infallible>(Up::Both(1, "a".into()))
    });
    g.add_node("b", |_s| async {
        Ok::<_, Infallible>(Up::Both(10, "b".into()))
    });
    g.set_entry("a");
    g.add_edge("a", "b");
    g.add_edge("b", END);

    let out = g
        .compile()
        .unwrap()
        .invoke(Counter::default())
        .await
        .unwrap();
    assert_eq!(out.n, 11);
    assert_eq!(out.trail, vec!["a", "b"]);
}

#[tokio::test]
async fn conditional_edge_routes() {
    let mut g = StateGraph::<Counter>::new();
    g.add_node("start", |_s| async { Ok::<_, Infallible>(Up::Add(5)) });
    g.add_node("big", |_s| async {
        Ok::<_, Infallible>(Up::Visit("big".into()))
    });
    g.add_node("small", |_s| async {
        Ok::<_, Infallible>(Up::Visit("small".into()))
    });
    g.set_entry("start");
    g.add_conditional_edge("start", |s: &Counter| {
        if s.n >= 3 {
            vec!["big".to_string()]
        } else {
            vec!["small".to_string()]
        }
    });
    g.add_edge("big", END);
    g.add_edge("small", END);

    let out = g
        .compile()
        .unwrap()
        .invoke(Counter::default())
        .await
        .unwrap();
    assert_eq!(out.trail, vec!["big"]);
}

#[tokio::test]
async fn parallel_superstep_obeys_bsp() {
    let mut g = StateGraph::<Counter>::new();
    g.add_node("fork", |_s| async { Ok::<_, Infallible>(Up::Add(0)) });
    // BSP:同一超步里 left / right 看到的是**同一份**上一超步末的快照(n=100)。
    g.add_node("left", |s: Counter| async move {
        Ok::<_, Infallible>(Up::Both(1, format!("left saw {}", s.n)))
    });
    g.add_node("right", |s: Counter| async move {
        Ok::<_, Infallible>(Up::Both(2, format!("right saw {}", s.n)))
    });
    g.set_entry("fork");
    g.add_edge("fork", "left");
    g.add_edge("fork", "right"); // fan-out:两个节点同一超步并发
    g.add_edge("left", END);
    g.add_edge("right", END);

    let out = g
        .compile()
        .unwrap()
        .invoke(Counter {
            n: 100,
            trail: vec![],
        })
        .await
        .unwrap();

    // 两个更新在同步点合并:100 + 1 + 2
    assert_eq!(out.n, 103);
    assert!(out.trail.iter().any(|t| t == "left saw 100"));
    assert!(out.trail.iter().any(|t| t == "right saw 100"));
}

#[tokio::test]
async fn checkpoint_records_every_superstep() {
    let mut g = StateGraph::<Counter>::new();
    for name in ["a", "b", "c"] {
        g.add_node(name, |_s| async { Ok::<_, Infallible>(Up::Add(1)) });
    }
    g.set_entry("a");
    g.add_edge("a", "b");
    g.add_edge("b", "c");
    g.add_edge("c", END);

    let app = g.compile().unwrap();
    let cp = MemoryCheckpointer::new();
    let out = app
        .invoke_with(Counter::default(), &RunConfig::default(), Some(&cp), None)
        .await
        .unwrap();

    assert_eq!(out.n, 3);
    // step0(初始) + step1..3
    assert_eq!(cp.history().len(), 4);

    // 时间旅行:能读回任意历史超步的状态与它当时的 frontier。
    let snap = cp.get(1).unwrap();
    assert_eq!(snap.state.n, 1);
    assert_eq!(snap.frontier, vec!["b"]);
    assert_eq!(cp.latest().unwrap().state.n, 3);
}

#[tokio::test]
async fn step_limit_stops_runaway_loop() {
    let mut g = StateGraph::<Counter>::new();
    g.add_node("spin", |_s| async { Ok::<_, Infallible>(Up::Add(1)) });
    g.set_entry("spin");
    g.add_edge("spin", "spin"); // 无限自环

    let app = g.compile().unwrap();
    let cfg = RunConfig { max_supersteps: 5 };
    let err = app
        .invoke_with(Counter::default(), &cfg, None, None)
        .await
        .unwrap_err();
    assert!(matches!(err, GraphError::StepLimit(5)));
}

#[tokio::test]
async fn compile_rejects_missing_entry_and_dangling_edge() {
    let mut g = StateGraph::<Counter>::new();
    g.add_node("a", |_s| async { Ok::<_, Infallible>(Up::Add(1)) });
    // CompiledGraph 没实现 Debug,所以用 match 取错误,别用 unwrap_err。
    assert!(matches!(g.compile(), Err(GraphError::NoEntry)));

    let mut g2 = StateGraph::<Counter>::new();
    g2.add_node("a", |_s| async { Ok::<_, Infallible>(Up::Add(1)) });
    g2.set_entry("a");
    g2.add_edge("a", "ghost"); // 指向不存在的节点
    assert!(matches!(g2.compile(), Err(GraphError::DanglingEdge { .. })));
}

#[tokio::test]
async fn file_checkpointer_persists_and_resumes_across_processes() {
    // a -> b -> c,每个 +1;初始 0 → 终态 3。
    let mut g = StateGraph::<Counter>::new();
    for name in ["a", "b", "c"] {
        g.add_node(name, |_s| async { Ok::<_, Infallible>(Up::Add(1)) });
    }
    g.set_entry("a");
    g.add_edge("a", "b");
    g.add_edge("b", "c");
    g.add_edge("c", END);
    let app = g.compile().unwrap();

    let mut path = std::env::temp_dir();
    path.push(format!("ridge_ckpt_{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&path);

    // 「进程 1」:跑完整,每个超步落盘。
    let cp = FileCheckpointer::new(&path);
    let out = app
        .invoke_with(Counter::default(), &RunConfig::default(), Some(&cp), None)
        .await
        .unwrap();
    assert_eq!(out.n, 3);

    // 「进程 2」:全新 checkpointer 从磁盘读回,取超步 1 的快照(n=1,下一步 [b])。
    let cp2 = FileCheckpointer::new(&path);
    let snap: Checkpoint<Counter> = cp2.get(1).unwrap().expect("step 1 checkpoint on disk");
    assert_eq!(snap.state.n, 1);
    assert_eq!(snap.frontier, vec!["b"]);

    // 从该快照续跑 → 到达同样的终态 3(证明跨进程恢复 + 超步连续)。
    let resumed = app
        .resume(snap, &RunConfig::default(), None, None)
        .await
        .unwrap();
    assert_eq!(resumed.n, 3);

    let _ = std::fs::remove_file(&path);
}

/// iter-24:Best-of-N 择优 —— 评分最高的分支终态胜出。
#[tokio::test]
async fn best_of_picks_highest_score() {
    let mut g = StateGraph::<Counter>::new();
    g.add_node("grow", |_s| async { Ok::<_, Infallible>(Up::Add(10)) });
    g.set_entry("grow");
    g.add_edge("grow", END);
    let app = std::sync::Arc::new(g.compile().unwrap());

    let initials = vec![
        Counter {
            n: 1,
            trail: vec![],
        },
        Counter {
            n: 5,
            trail: vec![],
        },
        Counter {
            n: 3,
            trail: vec![],
        },
    ];
    let best = app
        .invoke_best_of(initials, &RunConfig::default(), |s| s.n)
        .await
        .unwrap();
    assert_eq!(best.n, 15); // 5 + 10 胜出;败者状态被丢弃
}

/// iter-24:失败分支被弃(投机语义),不掀翻整体;全败/空输入 → NoWinner。
#[tokio::test]
async fn best_of_discards_failed_branches() {
    let mut g = StateGraph::<Counter>::new();
    g.add_node("risky", |s: Counter| async move {
        if s.n == 0 {
            Err("zero not allowed".to_string().into())
        } else {
            Ok::<Up, crate::state::BoxError>(Up::Add(10))
        }
    });
    g.set_entry("risky");
    g.add_edge("risky", END);
    let app = std::sync::Arc::new(g.compile().unwrap());

    // 分支 0 必败,分支 1 胜出。
    let best = app
        .invoke_best_of(
            vec![
                Counter {
                    n: 0,
                    trail: vec![],
                },
                Counter {
                    n: 2,
                    trail: vec![],
                },
            ],
            &RunConfig::default(),
            |s| s.n,
        )
        .await
        .unwrap();
    assert_eq!(best.n, 12);

    // 空输入 → NoWinner。
    let err = app
        .invoke_best_of(vec![], &RunConfig::default(), |s: &Counter| s.n)
        .await
        .unwrap_err();
    assert!(matches!(err, GraphError::NoWinner));

    // 全败 → NoWinner。
    let err = app
        .invoke_best_of(
            vec![Counter {
                n: 0,
                trail: vec![],
            }],
            &RunConfig::default(),
            |s: &Counter| s.n,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, GraphError::NoWinner));
}
