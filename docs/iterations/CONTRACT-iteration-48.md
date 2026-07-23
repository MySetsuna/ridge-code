# CONTRACT · iteration-48 —— pi-agent 式多订阅接入(Codex OAuth + 订阅档一等公民化)

> maker = NotebookLM(guidance-48)+ 用户目标「多订阅接入优先」;checker = 对抗评审(见 2026-07-23-notebooklm-guidance-48.md,G2 wire 假设已修正、G5 改勘察型)。大迭代:六目标,依赖 G1→G2→G3→{G4,G6},G5 独立。

## 背景(代码核实)

- `provider::oauth` 纯核已有(iter-43):Pkce/authorize_url/parse_token_response/exchange_code/refresh,但常量硬绑 `anthropic_oauth`(client_id/authorize/token/redirect/scopes)。
- `~/.ridge/oauth.json` 按 provider 名索引(`oauth_upsert("anthropic", …)`),天然多键位。
- `ProviderProfile{kind,model,base_url,key_env,api_key}` 无 OAuth 概念;OAuth 凭据仅作「key 全无时回退」(`resolve_claude_oauth_provider`)。
- TUI /login 页有 `CLAUDE_OAUTH_ROW` 先例(browser auth → 贴 code → 就地 exchange → 热切)。

## 目标(按序)

1. **G1 OAuth 纯核泛化**(`provider::oauth`):`anthropic_oauth` 常量泛化为 `OAuthConfig{client_id, authorize_url, token_url, redirect_uri, scopes, …}`;`authorize_url`/`exchange_code`/`refresh` 收 `&OAuthConfig` 参数;`anthropic_oauth()` 返回该 struct(现行为零变化)。
2. **G2 Codex OAuth 接入**(`provider::oauth` + `login.rs`):`openai_oauth()` OAuthConfig(auth.openai.com PKCE 流,scope openid profile email offline_access);CLI `login --codex`(流程同 `--claude`:URL → 贴 code → exchange → `oauth_upsert("openai", …)`);启动回退链扩展(anthropic 无则试 openai → `OpenAiProvider` bearer 档)。**诚实验证边界**:活端点/client_id/codex wire(或需 Responses API)属灰区,常量集中可 env/config 覆盖,先 bearer + 标准 chat wire,活验证用户实跑。
3. **G3 订阅档一等公民化**(`agent::config` + main/tui 解析):`ProviderProfile.use_oauth: Option<bool>`(serde-default);resolve 链:`use_oauth=true` → `oauth_get(kind)` →(需刷则刷 + 落盘)→ `new_oauth` 构造;`login --claude/--codex` 顺手写入对应命名档(`claude-max`/`chatgpt-plus`,use_oauth=true)。「key 全无回退」保留(兼容)。
4. **G4 TUI /login 页 codex-oauth 行**(`tui::panel`/`command`):照 `CLAUDE_OAUTH_ROW` 先例加 `CODEX_OAUTH_ROW`;`begin_*_oauth`/`apply_*_oauth_code` 泛化共用(收 OAuthConfig + provider 名)。
5. **G6 /provider 页可列可切订阅档**(`tui::command::switch_provider`):档 `use_oauth=true` → 走 oauth.json 凭据构造 bearer provider 热切;/provider 页 value 列注明 `oauth` 徽标。
6. **G5(勘察)TUI 遗留 4 bug**:底部多余状态条 / 状态条换行 / Shift+Enter 失效 / 光标卡首行。先复现定根因;可修的带回归测试修复;终端环境相关而不可确定性测的,写明诊断结论 + 缓解方案入报告。

## 边界(不做)

- 不做 Gemini/其他订阅源(iter-49 候选);不内置 openai-oauth 型本地代理(base_url 覆盖已可间接支持)。
- 不引 Responses API 全量 wire(除非 G2 勘察证明 bearer + chat wire 完全不可用,则最小补丁并记录)。
- 不改引擎层;不动 auth.json(API key 库)语义。

## 确定性验收信号

门禁 `cargo test --workspace && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` 全 exit 0。新增测试:
1. G1:`authorize_url(&anthropic_oauth(), …)` 输出与 iter-43 现值逐字节相等(回归);`authorize_url(&openai_oauth(), …)` 含 auth.openai.com + S256 + 正确 scope。
2. G2:`parse_token_response` 兼容 OpenAI token JSON(含缺 refresh_token 容错);`oauth_upsert("openai")` 与 "anthropic" 键位互不覆盖。
3. G3:config 解析 `use_oauth` 缺省 None(旧 config 零破坏);resolve 链单测(use_oauth 档 → oauth 凭据命中;无凭据 → 明确报错不 panic)。
4. G4/G6:login 页含 CODEX_OAUTH_ROW(照 tests.rs:196 先例);switch_provider 对 use_oauth 档产出 bearer 档(构造断言)。
5. G5:每个可修 bug 一个回归测试;不可确定性测的写诊断入报告。

## 停机

单轮;各目标独立提交(可回滚)。收尾:回写 ARCHITECTURE.md(§7 OAuth 节)+ PROJECT-STATE.md、报告、LOG.md、提交带 `iter-48`。
