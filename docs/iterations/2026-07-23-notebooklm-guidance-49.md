# NotebookLM 指导 · iteration-49 规划(2026-07-23)+ 对抗评审

> iter-48 收尾 query;来源已替换为 iter-48 版 PROJECT-STATE(331fd708),终态恒 1。

## NotebookLM 决策(要点)

1. codex wire 补丁 = OpenAiProvider 内 wire_mode 分支(逻辑隔离 StreamAcc 层),不做独立 CodexProvider。
2. Gemini 需 OAuthFlow 枚举(AuthCode|DeviceCode)+ poll_device_token。
3. 跨订阅 maker/checker:build_llm_agent_full 加 checker_provider_name,verify 节点独立检索命名档。
4. token 刷新必须收敛:provider::oauth 抽 get_valid_token 闭环。
5. 合同草案 P0 刷新收敛 + P0 Gemini + P1 checker 独立 + P1 codex wire + P2 /models 缓存 + P2 渲染加固。
6. 里程碑:iter-50 投机分支(invoke_best_of + 工作区隔离回归)、iter-51 GEPA 轨迹自改进。
7. 风险:device flow 阻塞 TUI 主环 / 跨订阅 429 拖慢超步 / 铁律禁 Google SDK。

## 对抗评审(checker)

| 建议 | 裁决 | 理由 |
|---|---|---|
| codex wire = OpenAiProvider 内分支 | ✅采纳(条件化) | 差异局部于端点/header/SSE 包装;但仅当用户实跑 login --codex 活验证失败才做(诚实边界) |
| Gemini device flow + OAuthFlow 枚举 | ⚠️修正 | 想当然:Gemini CLI 实用 authcode + localhost 回调(与 codex 同型),现 OAuthConfig 直接兼容,仅加 GOOGLE const;device flow YAGNI 不做 |
| verify 节点独立 provider(checker_provider_name) | ❌驳回 | 架构误解:verify 是确定性纯逻辑(verify_ok 只认 tests:passed 等信号,不调 LLM),无 provider 可换。跨订阅分工真实落点 = sub-agent reviewer 的 provider: 字段引 use_oauth 档 → 改为「build_agents 解析支持 use_oauth 档」 |
| get_valid_token 放 provider::oauth | ⚠️修正落点 | oauth 纯核恒无 IO(落盘在 agent 层);收敛核置 agent login.rs(resolve_valid_token:needs_refresh→refresh→落盘),供启动/热切共用 |
| /models 缓存持久化 + 渲染加固 | ✅采纳 | 源自 notes(TUI 残余);验收可判 |
| 429 重试强化 | 推迟 | 已有 TUI 层 10 次任务级重试;provider 层限流重试待实测证明需要(YAGNI) |
| 里程碑 iter-50/51 | ✅记录 | 投机分支回归须先解决分支工作区隔离(iter-42 被证伪教训在案) |
