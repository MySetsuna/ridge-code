# 开发 / 规划 / 归档工作流(NotebookLM 驱动的迭代循环)

这个项目用 **loop engineering 的最佳实践** 持续迭代,直到做出媲美 Claude Code 的 agent。
每一轮迭代是一个闭环:**开发 → 带时间戳报告 → 传回 NotebookLM 当来源 → 让它给下一步计划 → 归档 → 下一轮**。

> 这个闭环已沉淀为全局 skill `notebooklm-iteration-loop`(`~/.claude/skills/`),可在任意项目复用。

## 一次迭代的七步

1. **读 contract 与 log**:开工先读 `docs/iterations/CONTRACT-iteration-{N}.md`(本轮目标+验收)和 `docs/LOG.md` 末尾 5–10 条(跨迭代长期记忆)。
2. **开发**:按 contract 的优先级实现,受 constraints 约束。
3. **确定性验证**:`cargo test --workspace` + `cargo clippy -- -D warnings` + `cargo fmt --check` 必须全绿——这是 maker-checker 里 checker 的客观信号,不靠「看起来对」。
4. **写带时间戳的迭代报告**:`docs/iterations/{YYYY-MM-DD}-iteration-{N}.md`,含:做了什么、测试状态、能力对照(距 Claude Code 差什么)、开放问题、请 NotebookLM 定夺的点。
5. **上传当来源**:把报告(必要时加 `trace.json`/报错日志)上传到 NotebookLM 笔记本「手搓agent」(`source_add`,`source_type=file`)。
6. **取下一步计划**:`notebook_query` 让 NotebookLM 结合全部来源给「下一迭代优先级 + 确定性验收信号 + 里程碑」,原文归档为 `docs/iterations/{date}-notebooklm-guidance-{N}.md`。
7. **归档 + 落下一份 contract**:据指导写 `CONTRACT-iteration-{N+1}.md`;在 `docs/LOG.md` 追加一条本轮记录。

## 三类文件(artifacts / contracts / logs)

| 类型 | 位置 | 作用 |
|---|---|---|
| **Contract** | `docs/iterations/CONTRACT-iteration-{N}.md` | 每轮一份:目标、工作流、边界、**可验证的验收信号**、预算/停机条件。开工必读 |
| **Artifact** | `docs/iterations/{date}-iteration-{N}.md` + `-notebooklm-guidance-{N}.md` | 每轮产出:进度报告 + NotebookLM 指导归档。这些也是回传 NotebookLM 的来源 |
| **Log** | `docs/LOG.md` | 全局单文件,append-only:跨迭代的决策/偏好演进。开工前读末尾几条 |

## 规则(别破坏)

- **停机条件写成合同,不是愿望**:验收必须是编译器/测试/退出码能客观判定的(「`cargo test` exit 0 且覆盖某功能」),而非「改进代码」。
- **maker ≠ checker**:生成的 agent 不给自己打分;验证只认确定性信号。
- **授权阶梯当前 = Level 2 (Draft)**:改动在分支/worktree,人做物理信号验证后合并,不 auto-merge。往上爬一级要等当前级稳定产出「本来就会手动合的」质量。
- **每轮都留熔断记录**:硬回合上限 / 预算 / 无进展检测触发的情况写进报告——这是判断引擎健壮性的核心指标。
- **NotebookLM 是规划器与根因分析器,不是执行器**:它读来源给方向;代码由本仓库的开发环写。

## 里程碑地图(来自 NotebookLM 指导,详见 `docs/iterations/2026-07-13-notebooklm-guidance-01.md`)

M1 物理闭环 → M2 MCP 协议 → M3 故障容错(serde 快照) → M4 团队协作(独立 checker) → M5 自主规划(子任务 DAG)。当前在 **M1**。
