# CONTRACT · iteration-44 —— 可观测 tracing 埋点(核心闭环)+ 修文档漂移

> maker = 用户(AskUserQuestion 选「都做」,本轮取三条中的 tracing);checker = 我。价值:CLAUDE.md 列的「可观测走 tracing」是真债——现仅 langgraph/graph.rs 3 处埋点,**agent 核心闭环零埋点**,`RUST_LOG=agent=debug` 几乎不显 agent 行为。NLM 认证过期,本轮 maker 由我据代码充任。

## 背景(代码核实)

- `init_tracing()`(main.rs:82 启动即调)已装 subscriber,但 agent 层无 `tracing::*` 事件 → 空心。
- CLAUDE.md/AGENTS.md 仍写「可观测走 `tracing`(暂未接)」——**文档漂移**(init 已接,只是埋点空)。

## 目标(P0)

1. **`execute_tool_call`(lib.rs:1750,同步)埋点**:入口 `debug!(tool)`;危险命令拦截处 `warn!(tool, reason, "blocked dangerous command")`;正常出口 `debug!(tool, ok, "tool done")`。
2. **reason 节点(build_core, lib.rs:2503)埋点**:provider 调用前后 `debug!(step, …, "llm request")` / `debug!(step, tokens, "llm response")`。
3. **`write_run`(lib.rs:2807)run 收尾埋点**:`info!(reason, steps, tokens, approved, "run complete")`(复用已算的 `halt_reason`,顺带去掉重复计算)。
4. **修文档漂移**:CLAUDE.md/AGENTS.md 的「可观测走 tracing(暂未接)」→ 改为「已接:init_tracing + 核心闭环埋点,`RUST_LOG=agent=debug` 可观 agent 每步」。

## 边界(不做)

- 不引新依赖(`tracing`/`tracing-subscriber` 已在 agent deps)。
- 不改行为/控制流:仅加 `tracing::*` 事件(纯副作用 side-channel),路由/停机/reducer 一律不动。
- 不做 span/instrument 全覆盖 / OpenTelemetry 导出 / metrics —— YAGNI,超出「让 RUST_LOG 有用」的最小目标。
- 不在异步 spawned 节点上做脆弱的事件捕获断言(async+spawn 下线程本地 subscriber 捕获不到)。

## 确定性验收信号(纯数据断言,无计时/PTY/全局单例)

门禁 `cargo test --workspace && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` 全 **exit 0**。新增测试(1 条,清晰可判):
- `execute_tool_call_traces_blocked_dangerous_command`:线程本地 `tracing::subscriber::with_default` 装一个写入共享 `Vec<u8>` 缓冲的 `fmt` subscriber(`with_max_level(DEBUG)`,`with_ansi(false)`),直接调 `execute_tool_call`(**同步、在测试线程**)传一条危险 `run_shell`(如 `rm -rf /`)→ 返回值 `starts_with("BLOCKED")`(**无文件副作用**)且捕获缓冲**含** `run_shell` 与 `blocked`/`dangerous` 字样。这是对「危险拦截被观测到」的确定性断言。
- 既有 84+ agent 测试全绿(埋点是纯 side-channel,行为不变)。

## 停机

单轮;收尾:回写 ARCHITECTURE(§可观测:tracing 埋点点)、修 CLAUDE.md/AGENTS.md 漂移、报告、提交带 `iter-44`。NLM 源替换待其重认证后补(本轮认证过期,不阻断代码进度)。
