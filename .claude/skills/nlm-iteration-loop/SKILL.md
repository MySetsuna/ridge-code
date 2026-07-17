---
name: nlm-iteration-loop
description: NotebookLM 驱动的完整迭代闭环:上传架构现状为来源 → 问 NLM 拿下一迭代计划初稿 → 落 spec/contract → 对抗评审 → 写代码验绿 → 更新架构文档 → 替换 NLM 来源。当用户说「跑一轮迭代」「继续迭代」「用 nlm 推进」或 /loop 持续迭代时用。愿景全部达成即终止循环。
---

# NotebookLM 迭代闭环

每轮 = **现状上行 → 规划下行 → 对抗评审 → 实现验绿 → 现状回写**。NLM notes 存长期规划目标(愿景),`docs/ARCHITECTURE.md` 存代码现状;来源永远只留最新现状,规划永远基于「愿景 − 现状」差集。

## 前置(每轮开工先做)

1. NLM 工具是深加载的:先 `ToolSearch` select `mcp__notebooklm-mcp__notebook_list / notebook_get / source_add / source_delete / notebook_query / note` 等。
2. 定笔记本:用户指定 > 记忆/上轮 > `notebook_list` 找名含「手搓agent」或「RidgeCode」者;都无则问用户。
3. 定轮次 N:`docs/iterations/` 现存最大 CONTRACT 编号 + 1;目录空则看 `git log --oneline` 里最近 `iter-N` + 1。
4. 确认 `docs/ARCHITECTURE.md` 反映最新代码:若上轮后代码有变而文档未更,先用 codegraph(`codegraph_context`/`codegraph_explore`)增量校订。文档不存在则全量重生成。

## 七步闭环

### ① 上传现状为来源
`source_add(notebook_id, source_type="file", file_path=<绝对路径>/docs/ARCHITECTURE.md, wait=true)`,记下返回的 `source_id`(本轮收尾要替换它)。上传前把标题体现在文档首行(NLM 用它显示)。

### ② 问 NLM 拿下一迭代计划初稿
`notebook_query`:「结合全部来源(尤其 notes 中的规划目标/愿景)与最新架构现状,给下一迭代(iter-N)的:a) 优先级排序的目标(P0/P1);b) 设计方案初稿;c) **确定性验收信号**(测试/退出码可判定);d) 明确不做的边界。指出愿景中哪些已被现状覆盖、哪些还差。」
原文归档 `docs/iterations/{YYYY-MM-DD}-nlm-guidance-{N}.md`。

### ③ 落 spec(contract)
据初稿写 `docs/iterations/CONTRACT-iteration-{N}.md`:目标 / 边界(不做什么)/ **可验证验收信号**(`cargo test --workspace` exit 0 且新增覆盖某行为)/ 预算与停机条件。验收必须是编译器/测试/退出码能客观判定的,不是「改进代码」。

### ④ 对抗评审(关键步,别跳)
NLM 是计划 **maker**,不是裁判 —— 会硬凑引用、概念放错层、过度设计。对每条关键建议独立 check:
- 引用真支撑结论?(向 NLM 追问出处,或 `source_describe` 核对)
- 第一性原理 + **当前代码现实**(codegraph 查证符号真貌)成立?
- 高影响决策:派干净上下文子 agent(Explore/general-purpose)当对抗评审员找反例。
- 守设计不变量(ARCHITECTURE.md §8):引擎零 LLM、外置能力走 MCP/SKILL 不进内核、maker≠checker、有界注入。
**采纳/驳回 + 理由**写回 guidance 归档,并据裁决修订 contract。驳回先例:NLM 曾把 app 层预算塞进 `GraphError`、为成本记账引不相关 IoT 论文 —— 均驳回。

### ⑤ 实现 + 确定性验绿
按 contract 实现(遵 ponytail:最小可用改动)。门禁全绿才算完成:
`cargo test --workspace && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`
非平凡逻辑留可跑测试。写迭代报告 `docs/iterations/{YYYY-MM-DD}-iteration-{N}.md`(做了什么/测试状态/开放问题/请 NLM 定夺的点)。

### ⑥ 回写架构文档
用 codegraph 重读改动面,增量更新 `docs/ARCHITECTURE.md`(新符号/新不变量/新护栏)。提交本轮全部改动(代码 + docs),commit message 带 `iter-{N}`。

### ⑦ 替换 NLM 来源,收轮
`source_delete(source_id=①记下的旧架构来源, confirm=true)` → `source_add` 传更新后的 ARCHITECTURE.md。**只删本 skill 管理的架构现状来源,绝不动其他来源/notes**;用户启动本 skill/loop 即视为对这一替换的预授权。一轮完成。

## loop 模式(持续迭代)

- 由 /loop 或用户「连跑 K 轮」驱动时:每轮走完①–⑦再进下一轮,轮间不闲聊。
- **终止条件**:②的答复中 NLM 明确「notes 愿景已全部被现状覆盖,无有意义的下一迭代」,且本地无未完成 contract → 报告收束并停(loop 场景调 ScheduleWakeup stop)。
- 熔断:同一 contract 连续 2 轮验收不过 → 停下向用户报告,不硬冲。
- 每轮报告留熔断/停机记录(回合上限/预算/无进展),这是引擎健壮性核心指标。

## 规则

- maker ≠ checker:实现不给自己打分,验收只认确定性信号。
- NLM 出的每个关键决策必过④;跳过对抗评审的轮次无效。
- 密钥/凭据绝不进来源或 notes。
