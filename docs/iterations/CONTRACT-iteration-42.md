# CONTRACT · iteration-42 —— 减法:砍掉未接线的 BoN/工作区隔离投机基建

> maker = 用户裁定(方向抉择「砍掉 agent 侧孤儿」);NLM 两版计划均围绕这团投机料自相矛盾(一版造隔离、二版删隔离+硬接 reviewer),经**代码核实**后由用户拍板。价值门禁:纯减法(删被证伪的过度设计,loop「与加法同权」)。

## 缺口(代码核实,非记忆)

- `crates/agent/src/workspace.rs`(`Workspace{GitWorktree|ShadowCopy}` / `create_isolated` / `merge_winner` / `cleanup`)+ `branch_score`(lib.rs)自 iter-24/25 造出,**17 轮零生产调用者**(grep `.rs` 全仓:仅模块自身 + 自测引用)。
- `langgraph::invoke_best_of` 引擎原语 agent 从不调用;为它造的 agent 侧隔离/评分胶水成孤儿。
- ARCHITECTURE §2.7 自述其「未接 CLI」——被证伪的提前设计。

## 目标
1. **删** `crates/agent/src/workspace.rs`(含其 5 个自测)、`lib.rs::branch_score`(含其 1 个自测)、`pub mod workspace;` 声明与悬空文档注释。
2. **保留** `langgraph::invoke_best_of`(通用引擎原语,langgraph 自带 `best_of_discards_failed_branches` 测)——库层通用能力,非 agent 特定死码,留作未来真接入的原语。

## 边界(不做)
- 不删 `langgraph::invoke_best_of` / `FileCheckpointer` / `MemoryCheckpointer`(库原语,有测有 API 价值)。
- 不「硬接 BoN 到 reviewer」(NLM 二版提案)—— reviewer 只读回文本结论,`branch_score` 按 approved+省token 择优对「评审质量」是错误指标,近乎无用,**驳回**。
- 不改引擎、登录、命令、Hook。

## 确定性验收信号
门禁 `cargo test --workspace && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` 全 **exit 0**。
- 被删符号(`branch_score` / `workspace::*` / `Workspace`)**全仓 `.rs` 零引用**(grep 证)。
- 编译通过 = 无残留调用者(等价于「零引用」的编译器证明)。
- 既有其余测试全绿(删的是自测 + 死码,不碰活路径);`langgraph::best_of` 测仍绿(原语保留)。
- 净删:workspace.rs 整文件 + branch_score + 各自测(约 -200 行)。

## 停机
单轮;收尾:回写 ARCHITECTURE(§2.7 工作区隔离节删除、invoke_best_of 描述改「库原语,agent 侧胶水已删」)、报告、提交带 `iter-42`、替换 NLM 架构来源。

## 附:方法论修正(用户指令)
本轮起**一切判断以代码为主、文档为辅,不信记忆**。上一轮我误把 `FileCheckpointer`/`workspace.rs` 当不存在(依赖陈旧记忆),经代码核实纠正;本机陈旧自动记忆(`~/.claude/projects/.../memory/`)已按用户要求清除(`autoMemoryEnabled:false`)。
