# docs/iterations —— 迭代归档

NotebookLM 驱动的迭代循环产出物都放这里。命名规范:

- `CONTRACT-iteration-{N}.md` —— 第 N 轮的**合同**:目标 + 可验证验收信号 + 边界 + 停机条件。开工必读。
- `{YYYY-MM-DD}-iteration-{N}.md` —— 第 N 轮的**进度报告**(带时间戳),上传到 NotebookLM 当来源。
- `{YYYY-MM-DD}-notebooklm-guidance-{N}.md` —— NotebookLM 针对该报告给出的**下一步指导**归档。

全局工作日志在上一层 `docs/LOG.md`;工作流说明在 `docs/WORKFLOW.md`。

## 目录

| 轮次 | 合同 | 报告 | 指导 |
|---|---|---|---|
| 01 | (无,首轮推倒重来) | `2026-07-13-iteration-01.md` | `2026-07-13-notebooklm-guidance-01.md` |
| 02 | `CONTRACT-iteration-02.md` | `2026-07-13-iteration-02.md` | `2026-07-13-notebooklm-guidance-02.md` |
| 03 | `CONTRACT-iteration-03.md` | (P0-P2 完成) | — |
| 04 | — | `2026-07-14-iteration-04.md`(GLM 实测) | `2026-07-14-notebooklm-guidance-04.md` |
| 05 | `CONTRACT-iteration-05.md` | `2026-07-14-iteration-05.md`(REPL) | `2026-07-14-notebooklm-guidance-05.md` |
| 06 | `CONTRACT-iteration-06.md` | (MCP 接入/trace/compact) | — |
| 07 | — | `2026-07-14-iteration-07.md`(转向通用框架+Skills) | `2026-07-14-notebooklm-guidance-07.md` |
| 08 | `CONTRACT-iteration-08.md`(config 部分顺延 09) | `2026-07-14-iteration-08.md`(驾驭工程+用户交互) | `2026-07-14-notebooklm-guidance-08.md` |
| 09 | `CONTRACT-iteration-09.md` | (待) | (待) |
