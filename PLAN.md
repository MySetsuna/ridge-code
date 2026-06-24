# ridge-code · 基础规划与方向

> 本文件定方向与架构。落地时的具体起步步骤见 `HANDOFF.md`。
> 状态:方向已定(2026-06-24),尚未开工。

---

## 0. 一句话定位

**成本优化的编码 agent CLI。** 用「强模型主控规划与评审 + 弱模型扛执行量 + 客观验证器(编译/测试/类型/lint)做免费质量闸」的编排,跨 provider 混合压低成本,目标:在编码这个"验证便宜"的领域,用**显著更低的成本做到"优秀"(不追求顶尖)**。Rust 单二进制交付。

---

## 1. 方向依据(为什么这么定)

1. **能力上限由模型决定,不由框架/语言决定。** 赢点在编排 harness + 领域工具 + eval 闭环,不在用什么语言。
2. **编码领域有免费的强验证器**(编译器/测试/类型检查/lint),是 ground truth、不看模型强弱。所以"弱模型生成 + 强验证筛"这套方案**在编码领域最成立**——这是整个项目的前提。
3. **为什么 Rust:** 性能在 I/O 密集场景**不加分**(瓶颈是模型 API 与限流,不是本地算力);Rust 的真实价值是**单二进制分发、无运行时依赖、强类型建模复杂状态机(DAG/状态机)**。对一个要分发的 CLI,这笔交易划算。
   **代价(要接受):** provider/MCP 的 SDK 生态比 TS 薄、迭代略慢——用成熟的多 provider crate(见 §4)把这块胶水成本压下去。
4. **不追求克隆通用 Claude Code**,而是在垂直("编码 + 省钱")上做到优秀。胜负手是「级联路由 + 强验证 + 任务分解 + eval 闭环」,不是"多开 agent"。

---

## 2. 核心架构:编排大脑

```
用户任务
  ↓
[Planner · 强模型] 规划 → 产出 Task DAG
   · 分解成子任务,带依赖边(DAG,不是 flat list)
   · 每个子任务:自包含 spec + 难度/风险标签 + 验收标准(怎么算 done)
  ↓
[Scheduler] 按 DAG 拓扑序调度,无依赖的子任务并发
  ↓
[Router] 每个子任务按难度/风险分流
   ├─ 简单     → Worker(弱模型)
   └─ 难/高风险 → Worker(强模型,直接做,省掉"弱→打回"的往返)
  ↓
[Worker] 执行子任务(读写代码、调工具)→ 产出 patch/diff
   · 并行 worker 各自在隔离工作副本里改(git worktree / overlay),避免改同文件冲突
  ↓
[Verifier] 客观验证(第一道闸 · 免费 · 不看模型强弱)
   · 编译 / 测试 / 类型检查 / lint / 自定义验收脚本
   ├─ 过   → 标记子任务完成
   └─ 不过 → [Repair Loop]
              弱模型带错误反馈重试 → N 次仍失败 → 升级强模型 → 仍失败 → 退回 Planner 重规划该子树
  ↓
[Integrator] 子任务全完成后:集成 patch + 跨子任务一致性检查(接缝!)+ 全量验证(全库编译/测试)
  ↓
[Reviewer · 强模型] 选择性评审(只看客观工具盖不住的:架构/语义/安全/高风险)
   · 不逐字读全部 —— 否则 review 成本会吃掉省下的钱
   ├─ 通过   → 交付
   └─ 有问题 → 回 Repair Loop / Planner
  ↓
交付(apply diff + 输出报告 + 成本账单)
```

### 六条成本/质量杠杆(贯穿全局的设计原则)

1. **级联,不是平铺并发。** 弱先做,验证不过才升强;review 也级联(客观闸在前,强模型在后)。**省钱主要来自这条。**
2. **投资验证器,不是堆生成器。** 质量杠杆在选择器/验证器,不在 agent 数量。白嫖编译/测试/类型/lint。
3. **执行者 ≠ 验证者。** 弱模型不自评自己的产出(它做不好的事也判不准)。
4. **难任务直接路由给强模型**,跳过"弱做一遍再被打回"的浪费。
5. **review 选择性**(成本暗坑:全量深读 ≈ 重新生成一遍,会把省的钱吃回去)。
6. **eval 量化成本-质量**(§9)——"更便宜且同质量"是经验断言,必须量出来,否则盲飞。

---

## 3. 关键数据模型(`rc-types`)

纯数据 + serde,零业务逻辑,所有 crate 依赖它(对应 pi-web 的 `@pi-web/protocol` 角色)。

- `Task` — 根任务(用户请求)。
- `Subtask { id, spec, deps: Vec<Id>, difficulty, risk, acceptance, status }` — DAG 节点。
- `Difficulty { Trivial, Moderate, Hard }` / `Risk { Low, High }` — 驱动 Router 分流。
- `ModelTier { Weak, Strong }`。
- `Verdict { Pass, Fail { reasons: Vec<Diagnostic> }, Uncertain }` — 验证器/裁判产出;`reasons` 回喂 Repair Loop。
- `Patch` — 一组文件编辑(diff)。
- `CostRecord { provider, model, in_tok, out_tok, usd }` — 每次调用记账,汇总成本账单。
- `Event` — 编排过程事件(给 tracing / TUI / 报告)。

---

## 4. 技术栈(已核实,2026-06)

| 用途 | crate | 备注 |
|---|---|---|
| 异步运行时 | **tokio** | 事实标准 |
| HTTP | **reqwest** | |
| SSE 流式 | **reqwest-eventsource** / eventsource-stream | provider 流式响应 |
| 多 provider LLM 客户端 | **`llm-connector`** 或 **`llm`**(评估后二选一) | 已核实覆盖 Anthropic + OpenAI 兼容 + DeepSeek/GLM(智谱)/Qwen(阿里)/Moonshot 等国产便宜模型。**务必包在自己的 `LlmProvider` trait 后面**以便替换/降级到裸 reqwest(见 §6) |
| JSON / 类型 | **serde** + serde_json | |
| 工具参数 Schema | **schemars** | 生成工具 JSON Schema(对应 TS 的 zod) |
| CLI 参数 | **clap** | derive 风格 |
| 错误 | **anyhow**(应用层) + **thiserror**(库层) | |
| 可观测性 | **tracing** + tracing-subscriber | 编排系统**必须能看见 DAG 执行**,这是调试命脉 |
| 代码感知编辑 | **tree-sitter** + 各语言 grammar | 结构化编辑/定位 |
| MCP 客户端 | **rmcp**(官方 Rust SDK) | 已核实:官方、成熟(~470 万下载),client + 子进程 stdio |
| DAG 建模 | **petgraph** | 任务依赖图 + 拓扑排序 |
| diff 生成 | **similar** | 生成/应用 patch |
| 跑验证器 | **tokio::process** | 起 cargo/npm 等子进程 |
| 限流 | **governor** | 应对 provider 速率限制 |
| 重试退避 | **backon** | 处理 429 / 5xx |
| 配置 | **toml** + figment(或 config) | `~/.ridge/config.toml` + 项目级 `ridge.toml` |
| 终端实时视图(可选) | **ratatui** | 实时 DAG/进度视图,可后置到 M4 |
| 单二进制分发 | **cargo-dist** | 跨平台 release |

> ⚠️ **不要**用 `ai-agents` 这类"重框架"crate 当核心——它自带状态机/编排,会抢走你要自建的"大脑"。可借鉴思路,但核心编排必须是你自己的。`llm`/`llm-connector`/`mini-agent` 这类**薄客户端**才是合适的积木。

---

## 5. Crate / workspace 布局

```
ridge-code/                  (Cargo workspace)
├── Cargo.toml               workspace 根
├── crates/
│   ├── rc-types/            纯数据类型 + serde,零业务逻辑(所有人依赖它)
│   ├── rc-providers/        LlmProvider trait + 实现(Claude / OpenAI 兼容 / 国产),流式、重试、限流、记账
│   ├── rc-tools/            工具:fs 读写、shell、代码编辑(tree-sitter)、搜索;schemars 出 schema
│   ├── rc-verify/           验证器 runner:跑 build/test/typecheck/lint,解析错误成 Verdict
│   ├── rc-mcp/              MCP 客户端(rmcp),接外部工具/skills
│   ├── rc-core/             编排大脑:Planner/Scheduler/Router/Worker/RepairLoop/Integrator/Reviewer + 状态机
│   ├── rc-eval/             eval harness:跑任务集,量成本/质量/延迟
│   └── rc-cli/              二进制入口:clap、config、终端渲染,把上面接起来
└── docs/
```

依赖方向(底→上):`rc-types` → {`rc-providers`,`rc-tools`,`rc-verify`} → {`rc-mcp`} → `rc-core` → {`rc-eval`,`rc-cli`}。

---

## 6. provider 抽象(跨 provider 混合的关键)

```rust
#[async_trait]
trait LlmProvider {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionStream>;
    fn model_id(&self) -> &str;
    fn cost_rates(&self) -> CostRates;   // 用于记账
}
```

- **混合阵容:** 强 = Claude(Opus/Sonnet,走 Anthropic 原生);弱 = 便宜模型(DeepSeek / GLM / Qwen / 本地,多数走 OpenAI 兼容端点)。
- **关键洞察:** 大多数便宜 provider 暴露 **OpenAI 兼容** API,所以「**一个 Anthropic 实现 + 一个 OpenAI 兼容实现**」就能覆盖整条阵容——工程成本远低于想象。多 provider crate(§4)进一步替你抹平。
- **主要复杂点 = 工具调用归一化:** Anthropic 用 `tool_use`/`tool_result` 块,OpenAI 用 `tool_calls`/function calling。统一成内部表示。**有些国产 provider 的 tool-calling 有坑**——所以保留"对某个 provider 降级到裸 reqwest 自己拼"的能力,这正是要把第三方 crate 包在自己 trait 后面的原因。

---

## 7. 验证层(质量主来源,`rc-verify`)

- **配置驱动、语言无关:** 目标项目放一个 `ridge.toml` 声明验证命令(`build = "cargo build"`,`test = "cargo test"`,`lint = "cargo clippy"`…);没有则按项目类型自动探测。
- 跑命令(`tokio::process`)→ **解析输出成结构化 `Diagnostic`** → 失败时把诊断回喂 Repair Loop 当反馈。
- v1 至少支持 **Rust(cargo)** 和 **一种 JS/TS** 项目。

---

## 8. 并发执行的隔离

并行 worker 同时改代码会冲突。方案:**每个 worker 在隔离工作副本里改**——首选 **git worktree**(天然隔离 + 易合并),或内存 overlay。Integrator 阶段再合并 + 跑全量验证检查接缝。改动文件有重叠的子任务,Scheduler 应**串行化**而非并行。

---

## 9. eval 闭环(整个成本论点的证明,`rc-eval`)

**这不是最后才做的锦上添花,是 go/no-go 闸。** 建议在 M2 期间就搭最小版。

- **任务集:** 一组真实小型编码任务(带客观验收:测试通过)。
- **指标:** 成功率、**每任务成本(USD)**、**强模型 token 占比**、wall-clock 延迟。
- **基线对比:** 「全程强模型 single-agent」 vs 「ridge-code 混合编排」。
- **判定:** 必须量出"**同等质量下,成本显著低于全强模型基线**"。达不到 → 调路由阈值/分解粒度/review 策略,或重新审视方案。

---

## 10. 里程碑路线图

> **关键排序原则:先让一个 player 能演奏,再搭交响乐团。** 别在单 agent 还不能可靠完成一个任务时就建多 agent 编排。

- **M0 · Walking skeleton(单模型跑通):** `rc-types` 最小类型 + `rc-providers` 一个 impl + 流式 + `rc-tools`(读/写/shell/列目录)+ `rc-cli` 一个**最简单 agent loop(单模型、无编排)**。
  *DoD:* 单模型能在真实小项目里完成"加个函数且能编译"这类任务,端到端跑通。
- **M1 · 验证器 + 修复循环:** `rc-verify` 跑 build/test/clippy 并解析错误;单 agent 失败时带反馈重试。
  *DoD:* 任务失败能自动修复到验证通过;支持 Rust + 一种 JS/TS。
- **M2 · 编排大脑(核心):** `rc-core` 全链路(Planner→DAG→Scheduler→Router→Workers→Integrator→Reviewer)+ 跨 provider 路由上线 + worktree 隔离。
  *DoD:* 一个需 3–5 子任务的中等任务由编排端到端完成,且强模型 token 占比 < 设定阈值。
- **M3 · eval 闭环:** `rc-eval` 任务集 + 指标 + 基线对比。
  *DoD:* 量出"同等质量、成本显著更低"。**这是验证整个项目假设的关口。**
- **M4 · MCP + 打磨 + 打包:** `rc-mcp`(rmcp)接外部工具;ratatui 实时视图;cargo-dist 单二进制跨平台分发。

---

## 11. 非目标(v1)

- ❌ 通用任务 agent(只聚焦编码——因为它有免费验证器)。
- ❌ GUI / Web UI(就是 CLI)。
- ❌ 自训/自托管模型(只调用现成 provider)。
- ❌ 在硬推理上击败前沿模型(那是模型的事,本项目靠编排 + 验证 + 省钱取胜)。

---

## 12. 风险与开放决策

| 项 | 说明 | 缓解 |
|---|---|---|
| 跨 provider 工具调用归一化 | 主要工程复杂点,国产 provider 可能有坑 | 统一内部表示 + 充分 provider 测试 + 可降级裸 reqwest |
| 弱模型能力天花板 | 某些"简单"子任务仍超出弱模型 | 路由阈值靠 eval 调;升级路径兜底 |
| review 成本泄漏 | 强模型全量 review 吃掉省的钱 | 客观闸在前,强 review 选择性;eval 盯"强 token 占比" |
| 延迟 | 级联 + DAG 增加 wall-clock,交互式 CLI 体验吃这个 | 流式/进度展示;允许后台执行 |
| 分解质量 | Planner 切不好,下游全崩 | spec 自包含 + 验收标准明确;失败退回重规划 |
| 并发改文件冲突 | 并行 worker 改同文件 | worktree 隔离 + 重叠子任务串行化 |
| **待定** | 弱模型具体选型(DeepSeek/GLM/Qwen/本地?)、路由难度阈值、review 触发规则 | M2/M3 期间靠 eval 定 |
