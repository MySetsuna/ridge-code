# RidgeCode 工作日志(append-only,新条目在顶)

## 2026-07-23 · iter-48 完成:pi-agent 式多订阅接入

- 六目标全落(24765be + 9e75b2b):OAuth 纯核泛化(OAuthConfig/TokenWire)、login --codex(本地回调+state 防 CSRF)、use_oauth 一等公民档、TUI codex-oauth 行、/provider oauth 热切、TUI 4 bug 勘察(2 已修于 af88070,Shift+Enter 平台约束,光标卡首行根因修复)。
- 门禁:217 tests + fmt + clippy 全绿。诚实边界:codex wire 待用户实跑 login --codex 活验证。
- 插入需求:Models 页 provider 分栏(af88070 已有,1011c86 剥前缀)。
- 下一步:iter-49 候选 = codex wire 回填 / Gemini 订阅 / token 刷新收敛;NLM 待答 PROJECT-STATE D 节四问。

## 2026-07-23 · 笔记本初始化 + iter-48 合同落定(nlm 迭代工作流启动)

- **遗留清理**:未提交 8 文件(无 key 可开 TUI + TUI 内 Claude OAuth + RIDGE_PROXY)验绿提交(af88070,212 tests)。
- **笔记本初始化**:「RidgeCode现状、愿景、路线图」8 旧源分诊 → 新 PROJECT-STATE(31KB)上传 → 5 条规划 notes(订阅接入/投机分支/TUI 残余/能力全景/Grok Build)→ 原文存档 docs/nlm-digest/ → 用户确认后删 8 旧源。终态:来源恒 1。
- **iter-48 合同**(NLM 规划 + 对抗评审):主线多订阅接入 —— G1 OAuth 纯核泛化(OAuthConfig)→ G2 Codex OAuth(login --codex,诚实验证边界)→ G3 订阅档一等公民化(use_oauth 进 providers[])→ G4 TUI codex-oauth 行 → G6 /provider 页热切订阅档 → G5(勘察)TUI 遗留 4 bug。驳回 NLM:G2 wire 想当然、G5 假验收、providers-history 幻觉。
- **插入需求**:Models 页 provider 分栏 —— 核实 af88070 已实现,补行内剥前缀(1011c86),重装生效。
- **下一步**:执行 CONTRACT-iteration-48;里程碑参考:iter-49 Gemini 订阅 + 代理体验,iter-50 跨订阅 maker/checker 分工。
