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
| 09 | `CONTRACT-iteration-09.md`(批量编辑顺延 10) | `2026-07-14-iteration-09.md`(web_search + GFW 探测) | `2026-07-14-notebooklm-guidance-09.md` |
| 10 | `CONTRACT-iteration-10.md`(全部完成) | `2026-07-14-iteration-10.md`(UX+web闭环+config+AnySearch) | `2026-07-14-notebooklm-guidance-10.md` |
| 11 | `CONTRACT-iteration-11.md`(6/6 全完成) | `2026-07-15-iteration-11.md`(Claude Code UX 全套达成) | `2026-07-15-notebooklm-guidance-11.md` |
| 12 | `CONTRACT-iteration-12.md`(发布打磨) | (进行中:--help/--version+样例✅) | (含于 guidance-11) |
| 13 | `CONTRACT-iteration-14.md`(下轮:约束守卫) | `2026-07-17-iteration-13.md`(标准存储库+停机原因) | `2026-07-17-notebooklm-guidance-13.md` |
| 15 | `CONTRACT-iteration-16-signals.md`(下轮:signals) | `2026-07-17-iteration-15-hardening.md`(补硬伤+扬长研判) | (含于 iteration-15) |
| 16 | `CONTRACT-iteration-16-signals.md`(✅已交付) | (信号复利闭环,见 LOG iter-16) | — |
| 17 | — | (三者皆做,见 LOG iter-17) | — |
| 18 | `CONTRACT-iteration-14.md`(✅全交付) | `2026-07-17-iteration-18.md`(护栏套件收尾) | `2026-07-17-notebooklm-guidance-18.md` |
| 19 | `CONTRACT-iteration-19.md`(✅已交付) | (巨型输出截断,见 LOG iter-19) | (含于 guidance-18) |

> 注:另有并行子轨「token 节约之路」(token-iter-1~4 + VISION-COMPLETE)与「轻量安全护栏」(iter-5 jail / iter-6 read-only),文件名带对应后缀,未并入上表主线编号。
