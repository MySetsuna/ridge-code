# RidgeCode 工作日志(append-only,新条目在顶)

## 2026-07-23 · 笔记本初始化 + iter-48 合同落定(nlm 迭代工作流启动)

- **遗留清理**:未提交 8 文件(无 key 可开 TUI + TUI 内 Claude OAuth + RIDGE_PROXY)验绿提交(af88070,212 tests)。
- **笔记本初始化**:「RidgeCode现状、愿景、路线图」8 旧源分诊 → 新 PROJECT-STATE(31KB)上传 → 5 条规划 notes(订阅接入/投机分支/TUI 残余/能力全景/Grok Build)→ 原文存档 docs/nlm-digest/ → 用户确认后删 8 旧源。终态:来源恒 1。
- **iter-48 合同**(NLM 规划 + 对抗评审):主线多订阅接入 —— G1 OAuth 纯核泛化(OAuthConfig)→ G2 Codex OAuth(login --codex,诚实验证边界)→ G3 订阅档一等公民化(use_oauth 进 providers[])→ G4 TUI codex-oauth 行 → G6 /provider 页热切订阅档 → G5(勘察)TUI 遗留 4 bug。驳回 NLM:G2 wire 想当然、G5 假验收、providers-history 幻觉。
- **插入需求**:Models 页 provider 分栏 —— 核实 af88070 已实现,补行内剥前缀(1011c86),重装生效。
- **下一步**:执行 CONTRACT-iteration-48;里程碑参考:iter-49 Gemini 订阅 + 代理体验,iter-50 跨订阅 maker/checker 分工。
