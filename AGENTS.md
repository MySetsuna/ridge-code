# AGENTS.md

给 Codex 在本仓库干活的指引。与仓库既有文档一致,本文件用中文。

> 「为什么这么设计」与来源见 `docs/REPORT-langgraph-rust.md`;**方向/北极星见 `docs/DIRECTION.md`**;上手看 `README.md`。本文件只补「怎么干活」。

## 这是什么(北极星)

RidgeCode 是一个**模块化、跨领域可扩展的通用 agent 框架**(单二进制 `ridgecode`),既能像 Codex 写代码,又能做**编程以外**的事。
**加新能力 = 加一个 MCP server 配置 或 一个 `SKILL.md`,而不是改 Rust 源码。** 四层解耦:
内核(`langgraph-rs` 引擎)→ 协议(MCP 接万物)→ 知识(声明式 Skills)→ 协作(多智能体 maker-checker)+ 安全(权限门/危险命令拦截,沙箱待做)。详见 `docs/DIRECTION.md`。
底层赌注不变:agent 的「大脑」是一台**有状态图状态机**,引擎(`langgraph`,不含 LLM)与 agent(`reason/act/verify`)分层。

## 常用命令

```bash
cargo build --workspace
cargo test --workspace              # 6 引擎 + 2 agent + 1 doctest
cargo test -p langgraph             # 只测引擎
cargo test -p agent                 # 只测 agent
cargo run -p agent --bin ridgecode  # 跑 agent 闭环 demo
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings   # CI 会卡这两个
```

⚠️ 产品名 **RidgeCode**,二进制/命令是 **`ridgecode`**,但它住在 `crates/agent`(package 名 `agent`)。跑 demo 用 `-p agent --bin ridgecode`。环境变量前缀仍是 `RIDGE_*`(不改,避免破坏现有配置)。

## 跑起来(交互)

- `ridgecode`(TTY)-> **TUI**(ratatui:流式 + 状态行 + 权限弹窗);斜杠命令 `/model /provider /agent /config /compact /tools` 等**只在 TUI**。
- 非 TTY(管道/CI/重定向)-> **headless**:逐行 stdin 当任务串行跑,无斜杠命令,恒 `AutoApprove`(危险命令仍硬拦截)。
- **文本 REPL 已退役**:交互统一走 TUI,命令逻辑不再双份(见 `docs/` 决策记录)。改交互相关代码时,只有 `tui.rs`(TTY)与 `main.rs::headless`(非 TTY)两处,别再找 repl。

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

周边 crate(由 agent 依赖,按需改):
- `provider` - LLM provider 边界:归一化 Anthropic(`tool_use`)/ OpenAI(`tool_calls`)到统一 `Completion` + `ToolCall`。HTTP 客户端是薄一层,可纯函数单测、不烧 key。
- `mcp` - 最小 MCP 客户端(JSON-RPC 2.0):`initialize`/`tools/list`/`tools/call` + `<server>__<tool>` 命名空间。传输走 `McpTransport` trait 可插拔(内置 stdio;生产可换 `rmcp`)。
- `tools` - agent 的真实工具集(文件读写 + 跨平台 shell),纯 std。轻量内核护栏(非 OS 隔离):`jail_path` 限写 cwd 子树、`is_dangerous_command` 拦致命命令。
- `eval` - eval harness:批量跑 agent 量 pass-rate + token 成本,只收 `approved` 确定性闸,不看模型自述。

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
- **sub-agent(只读)**:agent 定义 = 带 frontmatter 的 `.md`(内置 fastcontext/explorer/reviewer 编进二进制 + 用户 `~/.ridge/agents/*.md`,同名覆盖内置)。主 agent 用 `dispatch_agent` 工具自动派 / `/agent` 手动派;子 agent 独立上下文、只回结论(省主上下文/token)、恒**只读**(仅 `read_file`/`search`,双重防御不下放写/shell)。`provider:` 字段引 `config.providers` 的廉价档 = **FastContext** 省钱。cwd 的 `CLAUDE.md`/`AGENTS.md` 经 `load_project_rules` 注入 system prompt。
- **内核 token 节约(愿景已收束,2026-07-16)**:四层已落地,发 LLM 的上下文对**长任务**保持有界。改这块前先看 `docs/iterations/VISION-token-runtime-state-COMPLETE.md`。四判据:①**历史有界** -- `to_messages` 按加权字符估算(`est_tokens`,CJK≈1tok/字,不引 tiktoken)超阈值自动 `compact_history`(压缩窗口首端裁悬空 `role=tool` 防端点 400);②**静态底噪极小** -- 工具 `description` 精简(`tool_descriptions_stay_terse` 守 <120 字/工具);③**Lean 输出** -- `BASE_SYSTEM` 含简洁 + 最小 diff 约束;④**事实驱动** -- `AgentState.modified_files`(`BTreeSet` 有序稳态)/`last_error` 由 `durable_updates` 在 act 后确定性回填,`durable_state_block` 编成事实块注入 messages **末尾**(role=system;首部 system prompt 冻结利缓存),体量 O(去重文件数)不随步数膨胀。**内核精简铁律**:squeez/向量库(RAG)/AST 骨架(syn)/tiktoken 等**外置可装能力一律走 MCP/SKILL,不进 Rust 内核**;动态工具加载/模型路由附条件推迟(见 VISION 文档)。

## 工程约定

- 依赖版本统一在根 `Cargo.toml` 的 `[workspace.dependencies]`,子 crate 用 `dep.workspace = true`。
- 可观测走 `tracing`(已接:`init_tracing` 装 subscriber + 核心闭环埋点 —— `execute_tool_call`/reason 节点 LLM 调用/`write_run` 收尾;`RUST_LOG=agent=debug,langgraph=debug ridgecode …` 可观每步),别用 `println!` 调试;`println!` 只用于 demo 的最终报告。
- provider 边界(未来接 LLM/MCP 时):第三方 SDK 包在自己的 trait 后,别让引擎直接依赖具体实现。
- 新代码必须 `cargo fmt` + `clippy -D warnings` 干净(CI 卡)。非平凡逻辑留一个可跑的测试。
