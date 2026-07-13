# CLAUDE.md

给 Claude Code 在本仓库干活的指引。与仓库既有文档一致,本文件用中文。

> 「为什么这么设计」与来源见 `docs/REPORT-langgraph-rust.md`;上手看 `README.md`。本文件只补「怎么干活」。

## 这是什么

手搓的 **Rust 版 LangGraph** 引擎 + 跑在它上面的最小**编码 agent**(单二进制 `ridge`)。
核心赌注:agent 的「大脑」本质是一台**有状态的图状态机**,先把这台引擎做对(Pregel 超步 + BSP + checkpoint),
agent 就只是引擎上的一组节点与边。开发顺序刻意分两层:`langgraph`(不含 LLM 概念)→ `agent`(装配 reason/act/verify)。

## 常用命令

```bash
cargo build --workspace
cargo test --workspace              # 6 引擎 + 2 agent + 1 doctest
cargo test -p langgraph             # 只测引擎
cargo test -p agent                 # 只测 agent
cargo run -p agent --bin ridge      # 跑 agent 闭环 demo
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings   # CI 会卡这两个
```

⚠️ 二进制名是 **`ridge`**,但它住在 `crates/agent`(package 名 `agent`)。跑 demo 用 `-p agent --bin ridge`。

## 架构:两层

```
crates/langgraph  (纯图引擎,零 LLM 依赖)
  state.rs       GraphState trait(State + reducer:节点返回 Update,apply 合并)
  graph.rs       StateGraph 构建器 + CompiledGraph + Pregel 超步执行环(invoke_with)
  checkpoint.rs  Checkpointer trait + MemoryCheckpointer(append-only,时间旅行)
        ▲
        │ depends on
        │
crates/agent      (装配 agent 图 + 二进制 ridge)
  lib.rs         AgentState/Patch(reducer)、Brain trait(接 LLM 的接缝)、build_agent
  main.rs        demo:跑通闭环 + 打印每个超步的 checkpoint
```

## 引擎的关键设计(改动时心里有数)

- **reducer 显式**:`GraphState::apply` 强制每种状态声明合并语义(默认覆盖会在并发下丢更新)。
- **BSP 超步**:`invoke_with` 每超步先 `state.clone()` 成快照,frontier 里所有节点吃**同一份快照**并 `tokio::spawn` 并发跑,跑完在**同步点**统一 `apply`,再据合并后状态路由。别让节点中途脏读同级更新。
- **条件边优先于静态边**;节点无出边则隐式 END。
- **防跑飞**:`RunConfig.max_supersteps` → `GraphError::StepLimit`。
- **checkpoint 时间旅行**:每超步落 `Checkpoint{step, frontier, state}`;`MemoryCheckpointer::get(step)` 可回读任意历史步。要跨进程持久化就在 `save` 里加 serde+bincode 落盘,trait 不用改。
- **错误**:库层用 `thiserror`(`GraphError`),节点错误归一化成 `BoxError`;应用层用 `anyhow`。

## agent 的关键设计

- **maker ≠ checker**:`reason`/`act` 生成,`verify` **独立**判定且只认确定性信号(工具输出的 `tests: passed`),不信模型自述。
- **双保险停机**:`MAX_STEPS` 硬上限 + `approved` 闸门。
- **`Brain` trait** 是接真实 LLM provider 的接缝 —— 换实现,图不动。当前是离线 `ScriptedBrain`(零联网可测)。

## 工程约定

- 依赖版本统一在根 `Cargo.toml` 的 `[workspace.dependencies]`,子 crate 用 `dep.workspace = true`。
- 可观测走 `tracing`(暂未接),别用 `println!` 调试;`println!` 只用于 demo 的最终报告。
- provider 边界(未来接 LLM/MCP 时):第三方 SDK 包在自己的 trait 后,别让引擎直接依赖具体实现。
- 新代码必须 `cargo fmt` + `clippy -D warnings` 干净(CI 卡)。非平凡逻辑留一个可跑的测试。
