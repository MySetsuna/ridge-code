# M4 · ratatui 实时视图 — 设计文档

> 状态:待实现(2026-07-05)。
> 配套:`PLAN.md §3`(Event 类型)、`§4/§10`(ratatui,M4 收尾);现状见 `CLAUDE.md`。

---

## 1. 背景与目标

编排现在只用 `tracing` 打日志(线性滚动),看不清 DAG 全貌:哪些子任务在跑/完成、当前阶段、
成本实时累积。`PLAN §3` 早就规划了 `Event` 类型「给 tracing / TUI / 报告」,但一直没落地。

M4 收尾这一项:**编排器发出结构化 `Event`,一个 ratatui TUI 实时渲染 DAG/进度**——
阶段、子任务状态(待办/运行/完成)、最近工具调用、实时成本(强/弱 token + 强占比)。
`--tui` 开启;不开则行为与现在完全一致(tracing 日志 + 末尾报告)。

## 2. 范围

**做:**
- `rc-types` 加 `Event` 枚举 + 小支撑类型(`Phase`/`PlannedSubtask`),纯数据。
- `rc-core`:`Orchestrator` 持可选事件 sink(`UnboundedSender<Event>`);`with_events(tx)` builder;
  在各编排步骤 emit 事件(阶段/规划/子任务起止/工具/修复/评审/成本/结束)。**不开 sink 时零行为变化。**
- `rc-cli`:`tui.rs`(ratatui + crossterm)——起终端 → 后台 task 跑编排发事件 → 主循环收事件更新
  `TuiState` 并重绘;`q`/Ctrl-C 退出;结束后恢复终端 + 打印最终报告。`--tui` flag。
- 纯逻辑(`TuiState::apply(Event)`)单测;rc-core「编排发出事件序列」离线单测(StubProvider)。

**不做(YAGNI):**
- 交互式操作(暂停/重试/取消子任务)——先只读实时视图。
- 鼠标 / 滚动历史 / 多面板切换。
- 把并行 DAG 画成图形(当前子任务仍串行,列表即可;并行留待并行调度落地)。
- 替换 tracing(TUI 与 tracing 并存;`--tui` 时抑制 tracing 输出以免污染屏幕)。

## 3. 关键决策

| # | 决策 | 取舍理由 |
|---|---|---|
| 1 | 编排发 `Event`,TUI 消费 | 落地 `PLAN §3` 的 Event;表现层与编排层解耦,报告/未来 web 视图也能复用 |
| 2 | sink = `tokio::mpsc::UnboundedSender<Event>` | send 非 async、不阻塞编排;Clone 便于穿进 `run_agent` |
| 3 | `Orchestrator` 加**可选** sink + `with_events` builder,`new` 签名不变 | 加法式;rc-eval/现有测试零改动,不开则零行为变化 |
| 4 | TUI 在 `rc-cli` 的模块(非独立 crate) | 只被 bin 用;避免为presentation 单开 crate |
| 5 | ratatui 0.30 + 其 re-export 的 crossterm | 跨平台(Windows 可用);只加一个 dep |
| 6 | 编排跑在 spawned task,渲染循环在主线程 | 多线程 runtime 下编排在 worker 线程推进,主线程 100ms 轮询终端;`try_recv` 收事件 |
| 7 | `--tui` 时不 init tracing fmt | 日志会画在 TUI 上;事件已替代日志 |
| 8 | 成本/工具事件穿进 `run_agent`(加 `Option<&Sender>` 参) | 工具调用与 token 累积是最「活」的部分,值得实时 |

## 4. 数据流

```
rc-cli(--tui):
  let (tx, rx) = mpsc::unbounded_channel::<Event>();
  let orch = Arc::new(orch.with_events(tx));      // sink 装进编排器
  let handle = tokio::spawn(run_arc(orch, task)); // 后台跑编排,一路 send(Event)
  ── 主线程渲染循环 ──
  loop {
    while let Ok(ev) = rx.try_recv() { state.apply(ev) }   // 收事件更新状态
    terminal.draw(|f| ui(f, &state))                       // 重绘
    if poll(100ms) { 读键;q/Ctrl-C → quit }
    if state.finished && quit { break }
  }
  恢复终端(RAII 守卫,panic 也恢复) → handle.await 拿 Outcome → 打印报告
```

`ui`:顶部 = 当前阶段 + 实时成本(强/弱 token、强占比%);左 = 子任务列表(○待办/▶运行/✓完成 +
难度 + tier);右/下 = 最近工具调用 + 事件日志(环形缓冲,capped)。

## 5. 数据结构与改动

### `rc-types`(纯数据)
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase { Planning, Executing, Verifying, Reviewing, Done }

#[derive(Debug, Clone)]
pub struct PlannedSubtask { pub id: String, pub description: String, pub difficulty: Difficulty }

#[derive(Debug, Clone)]
pub enum Event {
    Phase(Phase),
    Planned(Vec<PlannedSubtask>),
    SubtaskStarted { id: String, tier: ModelTier },
    SubtaskDone { id: String },
    Tool { step: usize, name: String },
    Repair { round: usize },
    Review { approved: bool },
    Cost(Cost),
    Note(String),      // 自由文本:验证过/不过、评审跳过 等
    Finished,
}
```

### `rc-core`
- 字段 `events: Option<tokio::sync::mpsc::UnboundedSender<Event>>`(`new` 默认 None)。
- `pub fn with_events(mut self, tx) -> Self`。
- 私有 `fn emit(&self, ev: Event)`:`if let Some(tx) = &self.events { let _ = tx.send(ev); }`。
- `run`:开头 emit `Phase(Planning)`;plan 后 emit `Planned(..)`;进执行 emit `Phase(Executing)`,
  每子任务 emit `SubtaskStarted`/`SubtaskDone`;验证阶段 `Phase(Verifying)` + `Note`/`Repair`;
  评审 `Phase(Reviewing)` + `Review`/`Note`;末尾 `Phase(Done)` + `Finished`。
- `run_agent` 加 `events: Option<&UnboundedSender<Event>>` 参:每工具调用 emit `Tool`,每次模型回复后 emit `Cost(*cost)`。
  各调用点传 `self.events.as_ref()`(评审也传,工具/成本照样计)。

### `rc-cli`
- `Cargo.toml` 加 `ratatui`(workspace dep)。
- `tui.rs`:`TuiState` + `apply` + `ui` + `run_with_tui(orch, task) -> Result<Outcome>`;终端 RAII 守卫。
- `main`:`--tui` flag;为真则不 init tracing、走 `run_with_tui`;否则原路径。MCP shutdown 照旧。

## 6. 测试
- `TuiState::apply`:喂一串 Event,断言 phase/子任务状态/cost/工具缓冲更新正确(纯逻辑,无终端)。
- `rc-core`:用 StubProvider + 一个 channel 跑 `run`,断言事件序列含 `Phase(Planning)`→`Planned`→…→`Finished`,
  且 `Cost` 事件的 token 单调不减。离线、零终端、零联网。
- 现有 rc-core/rc-eval 测试保持绿(不开 events → 零变化)。

## 7. DoD
- `ridge-code --tui --cwd <proj> "任务"` 显示实时 DAG:阶段推进、子任务状态流转、工具调用、成本累积;
  `q` 退出后恢复终端并打印报告。
- 不加 `--tui` 时行为与现在完全一致(零回归)。
- `cargo build/test/clippy(-D warnings)/fmt` 全绿;Windows 终端可用(crossterm)。
