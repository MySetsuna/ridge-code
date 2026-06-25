# M3 · eval 闭环 — 设计文档

> 状态:已通过头脑风暴评审(2026-06-25),待写实施计划。
> 配套:方向与指标依据见 `PLAN.md §9`、`§10`;项目现状见 `CLAUDE.md`。

---

## 1. 背景与目标

ridge-code 的核心赌注是「强/弱混合编排能在**同等质量下显著省钱**」。这个论点至今**没有被量化验证过**——我们不知道现在比"全程强模型"到底省了多少,甚至不知道是不是真省了。

M3 eval 就是验证它的 **go/no-go 关口**(`PLAN §9`/`§10`):跑一组带客观验收的小编码任务,对比「全程强模型单 agent」基线与「混合编排」的成本与质量。

本设计交付一个**最小可跑闭环**:内置小任务集、支持真实/离线两种运行、含基线对比、产出成本-质量对照。

## 2. 范围

**做:**
- 独立二进制 `rc-eval`(`cargo run -p rc-eval`)。
- 内置 2-3 个超小自包含 Rust 任务(随仓库走、开箱即跑、可复现)。
- 两种运行模式对比:**基线**(全程强模型单 agent)vs **混合编排**。
- **真实模式**(调真实 provider、量真实成本)+ **离线模式**(StubProvider、零成本零联网验证 harness)。
- 客观验收:eval 注入**隐藏验收测试**判定"做对没"。
- **USD 成本折算**(定价与用量解耦)。
- 指标 + 控制台对照表 + JSON 结果存档。

**不做(YAGNI,留待后续):**
- 大规模 / 真实开源项目任务集。
- 多次重复取平均、统计显著性(最小闭环先单次)。
- 并行跑任务(先串行,简单可控)。
- 把 eval 塞进 `ridge-code` 主命令(保持独立 bin)。

## 3. 关键决策(已与用户对齐)

| # | 决策 | 取舍理由 |
|---|---|---|
| 1 | 任务集 = 内置 2-3 个超小自包含任务 | 开箱即跑、可复现,最小闭环够用 |
| 2 | 真实 + 离线(Stub)两种模式 | 真实量成本;离线零成本验证 harness,落地 `HANDOFF §5` |
| 3 | 含「全程强模型」基线对比 | 验证核心论点(到底省了多少),go/no-go |
| 4 | 基线路径**复用 `rc-core`**(新增 `run_single`) | 与编排共用工具/验证,公平对比;避免逻辑漂移 |
| 5 | eval = **独立 bin `rc-eval`** | 面向开发者的度量工具,不污染主 CLI 体验 |
| 6 | `StubProvider` 放 `rc-providers` | 切 provider 即可,`Orchestrator` 无感知;可复用于单测 |
| 7 | 定价与用量解耦(`Pricing` + 报表层折算) | `rc-types` 保持纯数据;定价可换、可被主 CLI 复用 |
| 8 | 验收 = eval 注入**隐藏验收测试** | "做对"客观,防 agent 自写水测试刷高成功率 |

## 4. 架构与数据流

`rc-eval` 是独立二进制。一次运行:

```
加载内置任务集 tasks/*  +  定价 Pricing  +  provider(真实 / --offline 用 Stub)
        │
        ▼   对【每个任务】×【两种模式:基线 / 编排】:
   ┌─────────────────────────────────────────────┐
   │ 1. 把任务的 seed/ 复制到干净的临时工作目录       │  ← 隔离:每次跑都是全新副本
   │ 2. 在副本里跑该模式,拿到 Cost(token 用量)      │
   │      · 基线 = rc-core run_single(全程强)        │
   │      · 编排 = rc-core Orchestrator::run(混合)    │
   │ 3. 注入隐藏验收测试 → 跑 cargo test → pass/fail  │  ← 客观判定"做对没"
   │ 4. 记一条 TaskOutcome:成功? / token / USD / 耗时 │
   │ 5. 丢弃副本(--keep 可保留调试)                  │
   └─────────────────────────────────────────────┘
        │
        ▼
   汇总两种模式:成功率 · 总/均 USD · 强模型 token 占比 · 总耗时
        │
        ▼
   打印对照表(基线 vs 编排:省了百分之多少、质量是否持平) + 写 JSON 结果存档
```

**组件划分(各块职责单一、可独立测试):**

| 组件 | 职责 | 依赖 |
|---|---|---|
| `rc-eval` bin `main` | 解析参数(`--offline`、`--keep`、可选任务过滤)、装配 provider 与定价、驱动全流程 | clap |
| `runner` | 跑「一个任务 × 一种模式」:复制 seed → 跑模式 → 注入验收 → 判定 → 返回一条 `TaskOutcome` | rc-core, rc-verify |
| `reporter` | 汇总多条 `TaskOutcome` → 算指标与对比 → 打印表格 + 写 JSON | rc-types(Pricing) |
| 任务集 `tasks/<name>/` | 随 crate 走的 fixture:任务描述 + 起始代码 + 隐藏验收 | — |

**关键不变量:** 真实 vs 离线只在装配阶段换 provider,`runner`/`rc-core` 完全不感知;基线与编排都由 `rc-core` 提供入口、共用同一套工具/验证,保证对比公平。

## 5. 数据结构与各 crate 改动

### `rc-types`(保持纯数据、零业务)
```rust
/// 每百万 token 的美元单价。
pub struct Rate { pub in_per_mtok: f64, pub out_per_mtok: f64 }
/// 强/弱两档定价。
pub struct Pricing { pub strong: Rate, pub weak: Rate }
```
`Cost` **不改**(仍只存 token)。USD 折算由报表层用 `Pricing` 完成(例如 `reporter` 内 `cost.strong_in/1e6 * rate.in_per_mtok + ...`),不让 `rc-types` 承担业务。

### `rc-providers`:新增 `StubProvider`
- 实现 `LlmProvider`;持有一个"脚本"——按调用顺序返回预设的 `Completion`(含 `content` / `tool_calls` 与 `Usage`)。
- 调用次数超出脚本长度 → 返回明确错误。
- 离线模式由它驱动;`Orchestrator` 与 `run_single` 对它无感知。

### `rc-core`:新增「单 agent 直跑」入口
```rust
/// 基线:强模型,不分解/不路由,直接工具循环 + 验证修复。返回与编排相同的 Outcome。
pub async fn run_single(&self, task: &str) -> Result<Outcome>;
```
- 复用现有 `run_agent`(全部工具)+ `verify_and_repair`(强);**不含 Planner / Router / Reviewer**——这正是"全程强模型单 agent"基线的定义,与混合编排形成对比。
- 返回现有 `Outcome`(基线 `subtasks = 1`、`reviewed = false`)。与 `Orchestrator::run` 并列,两者都吐带 `Cost` 的结果。

### `rc-eval`:新建 bin + 模块
```rust
pub struct EvalTask { pub name: String, pub prompt: String, pub seed_dir: PathBuf, pub accept_dir: PathBuf }
pub enum RunMode { Baseline, Orchestrated }
pub struct TaskOutcome { pub task: String, pub mode: RunMode, pub success: bool,
                         pub cost: Cost, pub usd: f64, pub elapsed_ms: u128, pub error: Option<String> }
```
模块:`runner`(跑一条)、`reporter`(汇总+输出)、`tasks`(发现/加载内置任务集)。

## 6. 任务集与验收(防作弊的关键)

任务集 `crates/rc-eval/tasks/<name>/`,每个含三样:
- `prompt.txt` — 给 agent 的任务描述,**必须写明公共 API 签名**(函数名/参数/返回),否则隐藏验收对不上(eval 任务设计的硬约束)。
- `seed/` — 起始代码(空骨架,或放一个编译不过的函数让它修)。
- `accept/` — **隐藏验收测试**,agent 跑时不在工作目录里、看不到。

**跑一个任务的步骤(runner):**
1. 复制 `seed/` 到临时工作副本。
2. 在副本里跑模式(agent 改代码)。
3. 把 `accept/` 内容**覆盖进副本**(典型为 `tests/acceptance.rs`)。
4. 跑 `cargo test`(复用 `rc-verify`)→ 通过 = 成功。
5. 删副本(`--keep` 保留)。

验收测试独立调用被实现的公共 API,不依赖 agent 自写的测试。

**初拟任务:**
1. `add-mul` — 空骨架,要求实现 `pub fn add(a:i64,b:i64)->i64`、`pub fn mul(a:i64,b:i64)->i64`;隐藏验收断言若干样例。
2. `fix-compile` — seed 含一个编译不过的函数,要求修好;验收 = build + test 过。
3.(可选)`format-something` — 实现一个小格式化/解析函数 + 隐藏验收。

## 7. 指标、报表、错误处理、测试

**指标(每模式汇总):** 成功率(通过验收数/总数)、每任务/总成本 USD、强模型 token 占比、总/均 wall-clock。

**对比与判定:** 编排 vs 基线的 USD 节省百分比 + 成功率是否持平。若「质量持平 & 成本显著更低」→ 核心假设成立。

**产出:** 控制台对照表 + 一份 JSON 结果写到 `target/eval/`(便于后续回归追踪;时间戳用 `std::time::SystemTime`)。

**错误处理:** 单个任务跑挂(provider 报错/超时/panic)→ 记为该项 `success=false` + `error` 原因,**继续跑其余任务**,不中断整轮。Stub 脚本与调用不匹配 → 明确报错。

**测试策略:**
- `StubProvider`:单测验证按脚本返回。
- `runner`:用 `StubProvider` + 一个内置任务,**离线跑通一条 baseline**,断言产出一条 `TaskOutcome`(成功、cost 非零)。这是离线模式的核心价值——CI 里零成本验证 harness。
- `reporter`:给定几条假 `TaskOutcome`,断言汇总指标算对(成功率、USD、强占比)。

## 8. 实施任务分组(并发策略预告)

按已约定的执行策略:**独立任务派并发 subagent,关联任务主会话串行做**。

- **独立(可并发 subagent):**
  - A. `rc-types` 加 `Rate`/`Pricing`。
  - B. `rc-providers` 加 `StubProvider`(+ 单测)。
  - C. `crates/rc-eval/tasks/*` 三个 fixture(seed + prompt + accept)。
- **关联(主会话串行,负责对接缝):**
  - D. `rc-core` 加 `run_single`(依赖现有 rc-core)。
  - E. `rc-eval` 的 `tasks`/`runner`/`reporter`(依赖 A/B/C/D)。
  - F. config 加定价字段 + bin 装配 + USD 报表 + 离线集成测试(依赖 A/E)。

依赖链:A、B、C 可并行起步;D 可与 A/B/C 并行;E 依赖 A/B/C/D;F 依赖 A/E。

## 9. 本设计的验收标准(DoD)

- `cargo run -p rc-eval -- --offline` 能**零成本、零联网**跑通全部内置任务的两种模式,产出对照表 + JSON。
- 真实模式能跑通,输出含 USD 的对照表与"省了百分之多少 / 质量是否持平"。
- `StubProvider`/`runner`/`reporter` 有单测;`cargo test` 全绿。
