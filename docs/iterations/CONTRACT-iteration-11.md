# CONTRACT —— Iteration 11:媲美 Claude Code 的全部用户体验

- **开工时间戳**: 2026-07-14
- **里程碑**: 从「交互原子化」到「工程整体化」—— 批量编辑 + 逐字流式 + 崩溃恢复 + 进度透明
- **依据**: `docs/iterations/2026-07-14-notebooklm-guidance-10.md`(NotebookLM + 对抗评审)

## 目标(End State)

补齐距 Claude Code **用户体验**的最后几块:重构级批量改动一次确认、答案 token 级流式、kill-9 后能续、计划进度可见。

## 任务与验收信号

| 优先级 | 任务 | 确定性/可离线单测验收信号 | 状态 |
|---|---|---|---|
| **P0** | **多文件批量编辑**:`apply_edits` 跨文件多处、汇总 diff 一次确认、**原子**(全成/全不改) | 单测:2 文件各改一处→都改、返回 2;一处 old 缺失→整批不落盘;同文件多处顺序叠加;`edits_diff` 按文件分组 | ✅ `00a3903`(GLM 实测一次 apply_edits 改 2 文件) |
| **P1** | **LLM token 逐字流式**:provider 层读 SSE → 增量经 TokenBus 转发,REPL 边到边显(而非节点级) | 单测:**假流式 HttpClient**(喂 SSE 帧,**不用 mockito**)→ 文本逐字拼接==全文 + 分片工具调用/usage 正确;不支持流式→降级整段。**GLM 实测**逐字出、🤖 1 次无重复 | ✅ `4d45452` |
| **P1** | **kill-9 恢复**:`ridgecode --resume`——REPL 每轮把对话 history 落盘(`RIDGE_SESSION`/`~/.ridge/session.json`),重启读回 | 单测:save→load history 内容一致、缺文件→空;**GLM 实测**:进程1 记住事实→落盘;全新进程 `--resume`→「已恢复 N 条」→ 答对 | ✅ `dd6dd9b` |
| **P2** | **任务清单/TODO 可视化**:planner 拆的子任务在 REPL 渲染 `[x]/[ ]`,边做边勾 | 单测:3 子任务 + 完成 1 → 渲染 1 勾 2 空 | ⬜ |
| **P2** | **`@file` 上下文引用**:输入里 `@path` 自动注入文件正文(存在才注、同路径一次、截断防爆) | 单测:`@存在文件`→注入正文+来源标注、`@不存在`原样留;GLM 实测 `@note.txt`→模型不调工具一步答出文件里的密标 | ✅ `57db13c` |
| **P2** | 任务清单/TODO 可视化 + Ctrl-C 中断 | (待:TODO 需把 planner 接进 REPL;Ctrl-C 需 tokio signal + 取消语义) | ⬜ |

## 「媲美 Claude Code 全部用户体验」Definition of Done

- [x] REPL 彩色流式(节点级)+ spinner —— Iter10
- [x] 权限门 + diff 预览 + skip-danger —— Iter08/10
- [x] 精准 edit_file + search + 分段读 —— Iter08
- [x] **多文件批量编辑 + 汇总 diff 一次确认** —— 本轮 P0 `00a3903`
- [x] web 研究闭环(web_search/fetch_url)+ 无 key 多引擎 + 环境感知 —— Iter09/10
- [x] MCP/Skills 插件式扩展 + config.toml —— Iter07/10
- [x] trace 审计 + /compact —— Iter06
- [x] **零延迟 token 逐字流式**(SSE + TokenBus,graceful 降级)—— `4d45452`
- [x] **崩溃级恢复 `--resume`**(kill-9/重开续接会话)—— `dd6dd9b`
- [ ] **进度全透明 TODO 清单**(本轮 P2)
- [x] **精准上下文注入 `@file`**(输入 `@path` 注入文件正文)—— `57db13c`
- [ ] TODO 清单可视化 + Ctrl-C 中断(剩余 P2 打磨)

## 已知限制

- **重量级沙箱**(Docker/gVisor)—— 权限门 + 危险命令拦截 + diff 确认先顶着,标已知限制。
- **rmcp 替换自写 stdio**、子任务并行 = backlog。

## 边界

不破坏现有 75 测试 + clippy/fmt 干净;密钥不写 trace/日志/config;批量编辑必须原子;流式测试**不引 mockito**(用假流式 HttpClient)。
