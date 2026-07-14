# NotebookLM 指导归档 + 对抗评审 —— 针对 Iteration 10(即 Iteration 11 计划)

- **时间戳**: 2026-07-14
- **来源**: NotebookLM,基于《Ridge迭代报告 Iteration-10》(source `1044a0b9`)+ 全部来源
- **conversation_id**: `68791fb7-659a-4ad6-a86c-beb7ac694781`

## NotebookLM 给的 Iteration 11

- **P0** 多文件批量编辑 + 汇总确认(EditBuffer,交互硬门槛)
- **P1** LLM token 逐字流式(SSE,体验硬门槛)
- **P2** kill-9 恢复接进 REPL(`ridgecode --resume <ID>`,韧性硬门槛)
- **P3** 任务清单/TODO 可视化
- **P4** Ctrl-C 中断 / `@file` 引用 / 更丰富斜杠命令
- 更新 DoD 6 条:原子化批量审核、零延迟流式、崩溃级恢复、进度全透明(spinner↔TODO)、精准上下文注入(`@file`+/compact)、环境感知搜索。

## 对抗评审(不全信 NotebookLM)

- ✅ **采纳排序**:P0 多文件批量编辑(离线可测、直击 Claude Code 重构体验)、P1 token 流式、P2 kill-9 resume(引擎级已具备,接 REPL)、P3 TODO、P4 `@file`/Ctrl-C。整体与我方候选一致。
- ⚠️ **张冠李戴(揪出)**:P0 的引用 `[1]` 与沙箱引用 `[22]` 指向一份**完全无关的营销页**(「Ruh.AI / AI Workforce / 建筑估价」——某个混进笔记本的脏来源)。P0 的真实依据在 Ridge 各迭代报告(edit_file 语义、preview_call diff),**不是** Ruh.AI。忽略该来源。(已在提问里要求「不要杜撰无关引用」仍被引入 → 正是要对抗评审的原因。)
- ⚠️ **驳回 mockito 测 SSE**:NotebookLM 荐 `mockito` 模拟流式响应。但本项目**早已弃用 mockito**(本机代理下真打 localhost 挂 127s,改捕获替身)。SSE 流式测试应用**假流式 HttpClient**(把 chunk 序列喂给解析器),**不引 mockito**。
- 📝 **P1 SSE 工作量**:需 provider 层从「单请求单响应」升级到「流式读 SSE + 增量转 `StreamEvent::Chunk`」,是本轮最大块,单独排。

## 采纳后的 Iteration 11(见 CONTRACT-iteration-11)

**P0 多文件批量编辑(EditBuffer/apply_edits,原子 + 汇总 diff 一次确认)= 本轮已实现并 GLM 实测**。→ P1 token 逐字流式(假流式 HttpClient 测,非 mockito)→ P1 kill-9 `--resume`(引擎 FileCheckpointer 已具备)→ P2 TODO 可视化 / `@file` 引用 / Ctrl-C。重量级沙箱 = 已知限制。
