# ridge-code · 交接文档

> **读者:** 接手实现的人(你自己 / 协作者 / 实现型 agent)。
> **配套:** 方向与架构见 `PLAN.md`,本文件讲"从零怎么动手"。
> **现状(2026-06-24):** 全新仓库,目前只有 `PLAN.md`、`HANDOFF.md` 两份种子文档,**尚无代码**。

---

## 1. 已定的方向(不要再纠结的决策)

- **形态:** Rust 单二进制 CLI。
- **领域:** 编码 agent(因为编码有免费的客观验证器:编译/测试/类型/lint)。
- **核心架构:** 强模型规划+评审、弱模型执行、客观验证器做质量闸、级联路由压成本。详见 `PLAN.md §2`。
- **模型:** 跨 provider 混合(强=Claude,弱=便宜模型,多走 OpenAI 兼容端点)。
- **技术栈:** 见 `PLAN.md §4`(已核实 2026-06 的 crate 选择)。

## 2. 还没定、需要在实现中拍板的(见 PLAN §12)

- 弱模型具体选型(DeepSeek / GLM / Qwen / 本地?)——M2/M3 靠 eval 比。
- Router 的难度阈值、Reviewer 的触发规则——靠 eval 调。
- 多 provider 用 `llm-connector` 还是 `llm`——M0 时各起一个最小 spike 比较 **tool-calling 归一化质量**再定。

---

## 3. 第一步:M0 Walking Skeleton(先做这个)

**目标:让单个模型在真实小项目里完成一个会编译的小改动,端到端跑通。** 不要先碰编排——先证明"provider + 工具循环 + 应用 diff"这条最小链路。

### 3.1 建 workspace

```bash
cd /c/code/ridge-code
cargo new --bin crates/rc-cli --name ridge-code
# 其余 crate 用 cargo new --lib crates/rc-types 等按需建
```

根 `Cargo.toml`(workspace):

```toml
[workspace]
resolver = "2"
members = ["crates/*"]

[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
reqwest = { version = "0.12", features = ["json", "stream"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
schemars = "0"
clap = { version = "4", features = ["derive"] }
anyhow = "1"
thiserror = "2"
tracing = "0"
tracing-subscriber = "0"
# 版本号实现时按 crates.io 最新核对再钉
```

> 约定:版本统一在 workspace `[workspace.dependencies]` 管;子 crate 用 `dep.workspace = true`。

### 3.2 M0 任务清单(按序)

1. **`rc-types`** — 先放最小类型:`Message`、`ToolCall`、`ToolResult`、`Patch`、`Event`。`#[derive(Serialize, Deserialize)]`。
2. **`rc-providers`** — `LlmProvider` trait(见 `PLAN.md §6`)+ **一个**实现(建议先 OpenAI 兼容,便宜好测)+ 流式 `complete`。先不做记账。
3. **`rc-tools`** — `read_file` / `write_file` / `list_dir` / `run_shell` 四个工具;每个用 `schemars` 出参数 schema;一个 `dispatch(ToolCall) -> ToolResult`。
4. **`rc-cli`** — `clap` 解析;读 `~/.ridge/config.toml`(provider key + model);一个**最简单 agent loop**:
   `收任务 → 调模型(带工具 schema)→ 若有 tool_call 则执行并回灌 → 循环到模型给出最终答复 → 应用 Patch → 打印结果`。
5. 初始化 `tracing-subscriber`,**从第一天就把每步打到 tracing**(编排系统的调试命脉)。

### 3.3 M0 Definition of Done

- 在一个真实的小 Rust(或 JS)项目里,跑 `ridge-code "给 X 加一个返回 Y 的函数"`,它能改文件、且改完 `cargo build` 通过。
- 全程有 tracing 日志能看清每步。

---

## 4. 之后的里程碑

M1 验证器+修复循环 → M2 编排大脑 → M3 eval 闭环 → M4 MCP+打磨+打包。
**每个里程碑的 DoD 见 `PLAN.md §10`。** 排序铁律:**单 agent 可靠跑通(M0/M1)之前,不要建多 agent 编排(M2)。**

⚠️ **M3 eval 不是最后才做。** 它证明"省钱"这个核心假设。建议 M2 期间就搭最小 eval,边做边量"强模型 token 占比"和"每任务成本"。

---

## 5. 工程约定

- **错误:** 库 crate 用 `thiserror` 定义类型化错误;应用层(rc-cli)用 `anyhow`。
- **可观测性:** 一切走 `tracing`;关键编排事件同时产出 `Event`(给将来的 TUI/报告)。**不要用 `println!` 调试编排。**
- **配置:** 全局 `~/.ridge/config.toml`(providers + keys);项目级 `ridge.toml`(验证命令 + 路由偏好)。密钥可从环境变量覆盖,**永不写日志**。
- **类型边界:** `rc-types` 保持纯数据、零业务依赖(对应 pi-web 的 `@pi-web/protocol`)。
- **第三方 provider crate 包在自己的 `LlmProvider` trait 后面**,绝不让 `rc-core` 直接依赖它——保证可替换、可对单个 provider 降级到裸 reqwest。
- **测试:** 每个 crate 带单测;provider 层用录制的固定响应做离线测试(类比 pi-web 的 stub agent),避免每次跑都烧 key。

---

## 6. 参考资料

- **架构与方向依据:** 本仓库 `PLAN.md`(尤其 §1 方向依据、§2 编排大脑、§9 eval)。
- **MCP Rust SDK(官方):** `rmcp` — <https://github.com/modelcontextprotocol/rust-sdk> · <https://docs.rs/rmcp>
- **多 provider LLM crate(M0 评估二选一):**
  - `llm-connector` — <https://crates.io/crates/llm-connector>(明确列出 Anthropic + DeepSeek/Zhipu(GLM)/Aliyun(Qwen)/Moonshot 等)
  - `llm` — <https://crates.io/crates/llm>
- **设计来源:** 本方案是"强主控→分解→弱执行→客观验证→强 review"的精炼版,关键改进是**带修复循环的 DAG + 级联式验证/评审 + eval 闭环**(而非一次性直线管道)。
