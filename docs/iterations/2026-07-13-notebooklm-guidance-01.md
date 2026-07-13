# NotebookLM 指导归档 —— 针对 Iteration 01 报告

- **时间戳**: 2026-07-13
- **来源**: NotebookLM 笔记本「手搓agent」,基于《Ridge迭代报告 2026-07-13 Iteration-01》(source `90df4b23`)+ LangGraph / MCP / loop engineering 全部来源
- **性质**: NotebookLM 生成的下一步指导,原文归档(供后续迭代回读)

---

## 1. Iteration 02 优先级(核心:建立「物理闭环」)

当前 agent「真空运行」(离线脚本大脑 + stub 工具)。下一迭代目标是让它能触碰真实世界。

| 优先级 | 模块 | 为什么第一位 | 依赖 | **确定性验收信号** |
|---|---|---|---|---|
| **P0** | 真实工具集(FS/Shell) | 没有真实反馈,验证器无法工作 | 独立于 LLM | 运行后本地文件哈希发生预期变化 + shell 返回 `exit 0` |
| **P1** | 真实 LLM 集成 | 验证 ReAct 在真实推理下收敛 | 依赖 P0 | agent 修复一个真实 `rustc` 报错,`verify` 判定 `approved=true` |
| **P2** | MCP 客户端 | 标准化接入外部生态,免逐个写适配器 | 依赖 P1 | `tools/list` 发现外部工具并成功执行一次带参调用 |
| **P3** | 成本控制 | 真实 LLM 下防跑飞的经济护栏 | 依赖 P1 | 输出 token 账单;触达预算(如 $5)强制熔断 + 落快照 |

> 一句话:**Iteration 02 别在 prompt 上纠结,全力补 Rust 物理工具链 + `serde` 快照能力**——这是生产级「确定性」的关键。

## 2. 真实 LLM 集成最佳实践

- **Provider trait 抽象**:定义 `LlmProvider`,把 Anthropic 单个 `tool_use` 与 OpenAI `tool_calls` 数组归一化到自定义 `ToolRequest`。
- **结构化输出约束**:用 `schemars` 为工具派生 JSON Schema 注入 system prompt;解析层 `serde_json::from_str` + `Result`,失败即回退重试。
- **流式工具解析**:`tokio::mpsc` 处理流式 token,缓冲区状态机探测第一个 `{`,捕获到完整 JSON 立即触发节点更新,降低首字延迟。

## 3. 里程碑地图(MVP → 媲美 Claude Code)

- **M1 物理闭环**:自主读写代码 + shell 调编译器修错。验收:修复含 5 个语法错的 Rust 项目基准(exit 0)。
- **M2 协议标准**:内置 `rmcp`,调第三方 MCP 服务器。验收:自主调 PostgreSQL MCP 查 schema 再据此写码。
- **M3 故障容错**:`serde` 检查点持久化 + 跨会话时间旅行。验收:进程被杀后重启,从 checkpointer 加载状态、从超步 N 自动续跑。
- **M4 团队协作**:独立模型/权限的 checker 审计 maker。验收:checker 拦截 maker 删失败测试的「作弊」。
- **M5 自主规划**:planner 把模糊目标拆成子任务 DAG 异步执行。验收:在 10k 行未知代码库,凭「重构支付接口」完成跨 3 模块原子改动。

## 4. NotebookLM 驱动的迭代工作流(loop engineering 最佳实践)

- **Contracts**:`docs/iterations/CONTRACT-iteration-{N}.md` 定义本周期目标,必须含物理证据标准(编译通过、测试通过率 > 90%),别写「改进代码」。
- **Artifacts**:把 agent 执行轨迹(`trace.json`)、编译报错日志作来源上传;NotebookLM 负责根因分析 + 修复建议。
- **Logs**:全局 `docs/LOG.md` 记录跨迭代的偏好演进;agent 开新迭代前先读。
- **授权阶梯**:当前保持 **Level 2 (Draft)**——agent 在独立 Git Worktree 改码提 PR,人做物理信号验证,别跳到 auto-merge。
- **停机条件**:迭代报告要含物理熔断记录(如连续 3 轮 `cargo check` 报错未变而停机),这是引擎健壮性核心指标。
