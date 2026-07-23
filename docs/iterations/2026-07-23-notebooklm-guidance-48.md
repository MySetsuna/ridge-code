# NotebookLM 指导 · iteration-48(2026-07-23)+ 对抗评审

> 笔记本「RidgeCode现状、愿景、路线图」(24ef1fd0);来源已达终态(恒 1 份 PROJECT-STATE);conversation_id 68791fb7。本轮为初始化后首次规划 query。

## 初始化记录

- 旧 8 来源分诊:ARCHITECTURE.md 旧快照(已过时)删;5 份研究/规划源摘为 notes(订阅接入/投机分支/TUI 残余/能力全景/Grok Build)后删;原文全量存档 repo `docs/nlm-digest/`(零损失)。用户当面确认后批量删除。
- 新 PROJECT-STATE(31KB,A 定位/B 近况/C 能力对照/D 开放问题/E codegraph 架构事实全文)上传并 rename,终态来源恒 1。

## NotebookLM 决策(原文要点)

1. **Codex OAuth 设计**:泛化 `provider::oauth` 纯核 —— Anthropic 常量重构为 per-provider `OAuthConfig`;PKCE 流程(URL/交换/刷新)oauth 模块统一,wire 差异封装在 Provider 实现 `LlmProvider` trait 内。
2. **一等公民化**:`ProviderProfile` 加 `use_oauth: Option<bool>`;providers[] 可含 `{name:"chatgpt-plus", kind:"openai", use_oauth:true}`;「key 全无回退」保留兜底;OAuth 档优先经 oauth.json 加载凭据,/provider 页可列可切。
3. **排序**:泛化重构 → Codex OAuth → 一等公民化(第二订阅源就位后多订阅并存才有物理验证环境)。
4. **TUI bug 穿插**:协议核之后、UI 接入之前。
5. 合同草案 G1-G6(见 CONTRACT-iteration-48.md);风险:端点协议漂移 / UI 布局冲突 / oauth.json 多 provider 键位隔离;里程碑:iter-49 Gemini/Google One 订阅 + 代理体验,iter-50 跨订阅 maker/checker 分工(主 agent 用 Claude Max、reviewer 用 Codex)。

## 对抗评审(checker)

| 建议 | 裁决 | 理由 |
|---|---|---|
| G1 泛化 OAuthConfig | ✅采纳 | codegraph 核实:`oauth::anthropic_oauth` 常量已集中,泛化零阻力;回归验收(anthropic URL 逐字节不变)可判 |
| G2 「OpenAI 格式标准 JSON 即可」 | ⚠️修正 | Codex 订阅实走 backend-api/Responses 风格,非标准 chat completions;NLM 无该 wire 细节来源,属想当然。修正为 iter-43 同款「诚实验证边界」:常量集中可覆盖、先 bearer+标准 wire、活验证用户实跑 |
| G3 use_oauth 字段 | ✅采纳 | serde-default 向后兼容;resolve 链与 resolve_top_level_key 收敛先例一致 |
| G4/G6 TUI 接入 | ✅采纳 | CLAUDE_OAUTH_ROW 先例(tests.rs:196)直接照抄泛化 |
| G5 验收「断言 Shift+Enter 产生 \n」 | ❌驳回 | 该测试已存在且绿(InputState 状态机);bug 在终端实际行为(CSI u 支持度/终端差异),纯函数断言测不到。改勘察型目标 |
| 「订阅档进 AgentState history 处理流」 | ❌驳回 | providers[] 与 history 无涉,张冠李戴幻觉 |
| 排序与里程碑 | ✅采纳 | 记入 LOG 供 iter-49/50 参考 |

## 插入需求(用户,本轮)

- Models 页按 provider 分栏 + 显示 provider:核实为已实现(af88070 分栏标题 + 跨 provider 收集),补行内剥前缀(1011c86);旧安装版不含,重装生效。
