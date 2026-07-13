# 手搓 Rust 版 LangGraph + 编码 Agent —— 开发报告

> 来源:NotebookLM 笔记本「手搓agent」(44 个来源)。本报告只提炼与 **LangGraph Rust 版开发**
> 直接相关的部分,略去笔记本里混入的一批蛋白质/酶「loop engineering」文献(同名不同题,无关)。
> 主要参考来源:`智能体开发:理论、技术栈与路线图`(含手搓 Rust MVP 蓝图)、`langchain-ai/langgraph`、
> `modelcontextprotocol/rust-sdk`、以及若干 loop engineering 文章(Addy Osmani / Lushbinary / Shift Asia / AI Builder Club)。
>
> 本报告的结论已经**落成代码**:`crates/langgraph`(引擎)+ `crates/agent`(agent)。文末有报告→代码的对照。

---

## 0. 结论先行(TL;DR)

1. **LangGraph 没有官方 Rust 版**,社区只有零星实验品,没有生产级对齐。但 **Rust 极其适合手搓 LangGraph**:它的核心哲学就是 **Pregel 拓扑图 + 状态机**,而 Rust 的强类型、`enum`(天然表达条件路由)、无畏并发、Tokio 异步运行时,正好是「智能体状态机」的孵化器。
2. 手搓只需 4 个要素:**State(状态+reducer)、Node(异步任务)、Edge(路由)、Runtime(超步执行环)**。
3. Rust 版可以在几处**直接超越 Python 版**:`serde`+`ArcSwap` 做零开销快照/时间旅行;`tokio::spawn`+`join_all` 做子智能体真并行(Python `asyncio` 实际单线程排队);内嵌官方 `rust-sdk` 让 MCP RPC 损耗降到微秒级。
4. **开发顺序**:先做稳的 **LangGraph 引擎**(这是地基),再在它上面做 **agent**(ReAct 循环 + maker-checker 验证 + 停机护栏)。本仓库已按此顺序落地两阶段的最小可用版本。

---

## 1. 为什么先做 LangGraph,再做 Agent

笔记本给出的「研发第一步战略建议」很明确:**先别造轮子,先深度解构 LangGraph 与 MCP SDK 两个项目**,把「持久化检查点实现状态回滚/时间旅行」这套地基理解透。原因:

- Agent 的「大脑」(何时思考、何时行动、如何分支、如何回滚、如何并发子任务)本质上是一台**有状态的图状态机**。没有一个可靠的图引擎,agent 的编排逻辑会散落在一堆 `if/loop` 里,不可观测、不可回滚、不可并行。
- LangGraph 定位就是这台「底层编排基础设施」:持久化执行、human-in-the-loop、短期/长期记忆、可视化调试、生产级部署。先把这层做对,agent 只是它上面的一组节点与边。

所以本仓库的分层是:`langgraph`(引擎,不含任何 LLM 概念)→ `agent`(把 reason/act/verify 装配成一张图)。

---

## 2. LangGraph 核心架构(要手搓,先吃透)

LangGraph 灵感来自 **Pregel** 与 Apache Beam,公共接口借鉴 NetworkX。它把「复杂、非线性的智能体逻辑」转成一个**确定、可观测、可审计**的控制系统。五组概念:

### 2.1 图结构
| 概念 | 作用 |
|---|---|
| **StateGraph** | 核心入口类;建图时先定义 **State Schema**(全图共享的数据结构) |
| **Node(节点)** | 处理单元(异步函数):接收当前 `State`,执行任务(调 LLM / 工具),返回**对状态的更新** |
| **Edge(边)** | 节点间的确定性路径:A 完成后无条件转 B |
| **Conditional Edge(条件边)** | 用逻辑判断(`match`/`if-else`)**动态**决定下一个节点(如「继续调工具」vs「直接回复」) |

### 2.2 状态管理:Channels 与 Reducers
- **State** 是智能体的「记忆」,在节点间流动并持久化。
- 状态由多个 **Channels** 组成;每个 channel 配一个 **Reducer** 定义「新数据如何合并进旧状态」。
- 关键点:聊天记录字段的 reducer 应是**追加(append)**而非覆盖 —— 否则多节点并发写同一字段会**丢更新**。**不显式定义 reducer 时,默认覆盖是危险的。**

### 2.3 执行模型:超步(Superstep)+ BSP
- **超步(Superstep)** 源自 Pregel:每一超步里所有就绪节点**并行运行**;本步全部完成后,把更新统一合并进状态,再决定下一超步跑哪些节点。
- **BSP(Bulk Synchronous Parallel)**:节点**只能看到上一超步结束时的状态快照** —— 这从根上消除了同一超步内节点间的竞态。
- **同步点(Sync Point)**:所有并行节点跑完 → 统一 reduce → 才进入下一超步。

### 2.4 容错与交互
| 特性 | 作用 |
|---|---|
| **Checkpointer** | 每个超步后自动存状态快照 → 故障恢复 + **时间旅行**(回滚到任意历史步、多路线分叉) |
| **Human-in-the-loop** | 在节点执行前/条件边流转时设断点,人类可检查/改状态/批准敏感操作后恢复 |
| **Streaming** | 实时输出每个节点结果或 LLM token,交互式应用必备 |

### 2.5 组装成一张运行图
定义 State → 加 Nodes → 配 Edges(含条件边)→ 绑 Checkpointer → `compile()` → Runtime 循环执行超步(读状态、唤醒节点、合并更新、存 checkpoint,直到 END)。

---

## 3. 概念到 Rust 的映射(手搓蓝图)

笔记本给出的 MVP 用字符串状态、单活跃节点循环,够演示但不够工业级。本仓库在它基础上补齐了 **reducer、超步并行/BSP、checkpoint、streaming**。核心映射:

| LangGraph(Python) | 本仓库 Rust 实现 | 位置 |
|---|---|---|
| State Schema | `trait GraphState: Clone + Send + Sync` | `langgraph/src/state.rs` |
| Channels + Reducers | `type Update` + `fn apply(&mut self, u)` —— 状态自己声明合并语义 | 同上 |
| Node | `add_node(name, async fn(S) -> Result<S::Update, E>)` | `langgraph/src/graph.rs` |
| Edge / Conditional Edge | `add_edge` / `add_conditional_edge(router: Fn(&S)->Vec<String>)` | 同上 |
| START / END | `pub const START/END: &str` 虚拟节点 | 同上 |
| Superstep + BSP | `invoke_with`:每超步克隆快照 → `tokio::spawn` 并发跑 frontier → 同步点统一 `apply` → 据合并后状态路由 | 同上 |
| Checkpointer / 时间旅行 | `trait Checkpointer` + `MemoryCheckpointer`(append-only,可 `get(step)` 回读) | `langgraph/src/checkpoint.rs` |
| Streaming | `enum StreamEvent` + `mpsc::UnboundedSender` | `langgraph/src/graph.rs` |
| 防跑飞 | `RunConfig.max_supersteps` → `GraphError::StepLimit` | 同上 |

**为什么 Rust 版更强(笔记本的「超越」论点 + 已落地的取舍):**
- **强类型 State**:数据契约在编译期被强制,而非运行时 `dict` 猜键。
- **无畏并发**:超步内多节点用 `tokio::spawn` 扔进多线程池真并行(Python `asyncio` 单线程排队);`enum` 天然表达条件分支路由。
- **零开销快照**:`S: Clone` 直接克隆(Arc 内部数据),要跨进程再上 `serde`+`bincode` 落盘。

---

## 4. 手搓的四个避坑指南(来自笔记本 + 已在代码中处理)

1. **Reducer 语义要显式** —— 别让默认覆盖偷偷丢数据。本仓库用 `trait GraphState::apply` 强制每种状态声明合并语义(`messages` 追加、计数器累加)。合并大结构时用 `std::mem::take`/`Cow` 减少深拷贝。
2. **BSP 别脏读** —— 同一超步节点若中途读同级实时更新,会破坏确定性。实现上:超步开始先 `let snapshot = state.clone()`,所有节点吃同一份 `snapshot`,跑完才在同步点 `apply`。(见 `parallel_superstep_obeys_bsp` 测试:两个并行节点都只看到 `n=100`,合并后才是 `103`。)
3. **Checkpoint 要异步 + 跳过临时字段** —— 持久化写盘用 `tokio::fs` 别阻塞执行环;网络句柄/DB 连接用 `#[serde(skip)]`,否则反序列化失败。快照以 **append-only 版本日志**存,才能二分回溯定位「哪一步开始错」。
4. **并发子智能体要有熔断** —— `tokio::spawn` + `join_all` 跑满核心的同时,必须配**硬回合上限 / token 预算**,并给并行 agent 独立 **Git Worktree / 沙箱**隔离文件冲突;对反复失败的下游用**断路器(circuit breaker)**快速失败。

---

## 5. Agent 阶段:把 Loop Engineering 落成一张图

引擎做好后,agent 就是引擎上的一组节点。笔记本把 2026 年的主流范式讲清楚了:**Loop Engineering(环路工程)** —— 从「手动提示 agent」转向「设计提示 agent 的系统」。它是继 Prompt→Context→Harness 之后的第四层,每层包裹前一层:

| 层 | 优化什么 | 工作单元 |
|---|---|---|
| Prompt Engineering | 一句指令怎么说 | 你手敲的一个 turn |
| Context Engineering | 窗口里放什么(历史/检索/工具定义) | 一次回答的周边条件 |
| Harness Engineering | 单 agent 的执行环境(工具/权限/沙箱/状态) | 一个 agent 的工作台 |
| **Loop Engineering** | 自主循环:发现→规划→执行→验证→决定是否再来 + **何时停** | 跨多 turn 的自运行循环 |

### 生产级自主循环的五要素(agent 要实现的)
1. **发现任务**:cron / webhook / GitHub 事件触发 + triage 技能定优先级。
2. **执行**:Git Worktree 隔离并行;`SKILL.md` 沉淀项目知识;**MCP** 接实时工具(文件/终端/DB)。
3. **验证(瓶颈所在)**:**生成已商品化,验证才是核心**。**别让生成代码的 agent 自审**(它总给自己打 A)。**maker(生成)≠ checker(独立、甚至只读、带对抗性指令)**;成功标准绑到**确定性信号**(编译退出码、测试通过、lint、Playwright 录像),而非 LLM 语义自述。
4. **成本控制**:硬回合上限(5–10 轮)+ token/费用预算,触发即熔断转人工。
5. **防失控**:停机条件写成**合同**(`npm test` 退出码 0 且覆盖率 ≥ 90%),而非愿望(「改进代码」);**无进展检测**(状态哈希连续不变即判死锁强退)。

### 关键模式
- **Andrew Ng 三环**:内环(agent 自写自测,秒~分)/ 中环(人评审改规范,分~时)/ 外环(真实用户与生产数据,时~周)。越外层,验证越依赖人类 context。
- **Ralph 技术(上下文重置)**:别在一个超长会话里憋大招;每个 pass 用**全新干净上下文**,只通过磁盘状态文件传进度,防「上下文腐烂」。
- **共享大脑文件系统**:Artifacts(产物)/ Contracts(每个 loop 的目标与边界 README)/ Logs(全局工作日志)—— 让多次运行、多个 loop 能互相累积。
- **授权阶梯**:手动 → 自动 triage → 生成草案 → 经核实的 PR → 自动合并,一级一级放权,别一步到位。

### 本仓库 agent 的落地(`crates/agent`)
把上面的结构压成一张最小可运行的图(离线、零联网、可测):

```text
START ─▶ reason ──(finish 或到回合上限)──▶ verify ──(approved / 到顶)──▶ END
          ▲  │                                 │
          │  └──(其它 action)──▶ act ──────────┘(未过 → 回 reason)
          └──────────── reflection loop ────────
```
- `reason`/`act` 是 **maker**,`verify` 是**独立 checker**:`verify` 只认工具输出里的 `tests: passed`(确定性信号),不信 `reason` 的自述 —— 直接对应「别让生成者自审」。
- **双保险停机**:`MAX_STEPS` 硬上限 + `approved` 闸门。`broken_loop_terminates_at_cap` 测试证明:大脑永不收工 + 工具永远失败时,循环在回合上限停机,不会烧到天荒地老。
- `Brain` trait 是接**真实 LLM provider** 的接缝;当前给离线 `ScriptedBrain`。下一步把它换成 OpenAI/Anthropic 客户端即可,图不动。

---

## 6. 分阶段路线图

- **阶段 1 —— LangGraph 引擎(地基,本仓库已落地 MVP)**
  State+reducer / Node / Edge+条件边 / 超步 BSP 执行环 / checkpoint 时间旅行 / streaming / 防跑飞。✅ 已有 6 个单测覆盖。
- **阶段 2 —— Agent(本仓库已落地 MVP)**
  ReAct 循环 + maker-checker 验证 + 停机护栏,跑在阶段 1 引擎上。✅ 已有 2 个单测(happy path 收敛 + 坏循环停机)。
- **阶段 3 —— 接真实能力(下一步)**
  ① `Brain` 换真实 LLM provider(结构化输出约束 JSON、流式解析边生成边解工具调用);
  ② 内嵌官方 `modelcontextprotocol/rust-sdk` 做 MCP 客户端,把文件/终端/编译器接成工具;
  ③ checkpoint 上 `serde`+`bincode` 落盘,支持跨进程恢复与多路线分叉。
- **阶段 4 —— Harness / 多智能体 / 沙箱(远期)**
  子智能体真并行(`tokio::spawn`+`join_all`)+ Worktree/Docker(gVisor)/WASM 沙箱隔离 + 预热池降冷启动 + `tracing`/OpenTelemetry 全链路可观测 + SWE-bench 式 eval harness 度量成功率/成本。

---

## 7. 报告 → 代码 对照

| 报告章节 | 代码 |
|---|---|
| §2 LangGraph 核心 / §3 映射 | `crates/langgraph/src/{state,graph,checkpoint}.rs` |
| §4 避坑(reducer/BSP/checkpoint/并发) | `graph.rs::invoke_with` + `tests.rs::parallel_superstep_obeys_bsp` / `step_limit_stops_runaway_loop` |
| §5 Agent / Loop Engineering | `crates/agent/src/lib.rs`(reason/act/verify + 两条条件边) |
| §5 maker-checker / 停机 | `agent/src/lib.rs::tests::{happy_path_..., broken_loop_terminates_at_cap}` |
| §6 阶段 1/2 已落地,3/4 待办 | `crates/agent/src/main.rs`(demo:跑通闭环 + 打印超步 checkpoint) |

跑起来:`cargo test --workspace`(9 项全绿)、`cargo run -p agent --bin ridge`(看 agent 完整轨迹 + 每超步快照)。
