# CONTRACT — Iteration 24(内核主刀:Best-of-N 引擎原语;副刀:BPM 粘贴 + 动态输入高度)

## 目标

- **P0 `CompiledGraph::invoke_best_of`**(langgraph/graph.rs):`self: &Arc<Self>`,N 份初始状态经 JoinSet 并发 `invoke_with`,失败分支**丢弃**;调用方评分器 `Fn(&S) -> i64` 择优,**平分低索引胜**(确定性稳态);空输入或全败 → `GraphError::NoWinner`。`RunConfig` 补 `Clone`。
- **P0 agent 评分器 `branch_score`**:approved 压倒一切,同侪 token 省者胜。**不接 CLI 主流程**(工作区隔离未落,见 guidance-24 边界)。
- **P1 Bracketed Paste**:TerminalGuard best-effort 启/闭;select 加 `Event::Paste` 臂;纯函数 `sanitize_paste`。
- **P1 动态输入高度**:纯函数 `input_height(content, width, min=3, max=8)` 接入 draw 布局。

## 边界(不做)

- 不做 CowState / Pareto 多目标 / GEPA;不做分支工作区隔离(worktree,列下轮候选);不做 CSI u;不改 AgentState 结构;不引新依赖(JoinSet 是 tokio 自带)。

## 确定性验收信号(禁计时/环境断言)

1. `cargo test --workspace` exit 0,新增覆盖:
   - 引擎:初始 [1,5,3] 加 10,评分=值 → 胜者 15;含必败分支 [0(err),2] → 胜者 12(失败被弃);空输入 → `NoWinner`。
   - agent:`branch_score` —— approved 贵 > 未 approved 便宜;双 approved 省 token 者胜。
   - tui:`sanitize_paste`(CRLF/CR→LF、ESC 滤除、\t 保留)、`input_height`(空=min、折行、多行、封顶 max、width=0 不 panic)。
2. fmt + clippy `-D warnings` 净。

## 停机条件

- 触碰范围:langgraph/graph.rs + state.rs(错误枚举)+ tests.rs、agent/lib.rs(评分器)、tui.rs。越此即回退。
- 验收连续 2 次不过 → 熔断报告。
