# CONTRACT · iteration-43 —— OAuth 订阅登录 `ridgecode login --claude`(端到端)

> maker = 用户裁定(AskUserQuestion 选「OAuth 订阅登录 · 你的愿景」,明确接受 ToS 灰区风险并授权推进);checker = 我(正确性门禁 + 端到端非死脚手架)。价值:用户反复点名的唯一真实剩余愿景缺口(`ridgecode login --claude`)。

## 背景(代码核实,非记忆)

- 现状只有 `api_key`/`key_env` 档(auth.json 明文密钥库),**无 OAuth 流**。用户愿景:「像 Opencode 一样…用 `Ridgecode Login --claude` 登录第三方订阅接入模型」。
- 注入点已核实:`AnthropicProvider`(provider/lib.rs:1311)补全头硬编 `x-api-key`+`anthropic-version`;`make_provider`(main.rs:569)按 kind 装配;`run_login`(main.rs:281)解析 preset + flags;`real_provider`(main.rs:612)顶层 key 解析。
- **教训防重蹈**:iter-42 删掉的 workspace.rs 是「提前造、无人接」的死脚手架。本轮**端到端**——登录产出 token 且 **chat 补全路径真用它**,不留无消费者的变体。

## 目标(P0,单一方向端到端)

1. **`provider::oauth` 模块**(纯核 + HTTP 走接缝):
   - `OAuthToken { access_token, refresh_token, expires_at_epoch }`;`needs_refresh(&self, now_epoch) -> bool`(纯,now 传参,不读墙钟)。
   - `Pkce { verifier, challenge }`:`challenge = base64url_nopad(sha256(verifier))`(S256)。
   - `authorize_url(pkce_challenge, state) -> String`(纯)。
   - `parse_token_response(json, now_epoch) -> Result<OAuthToken>`(纯:`expires_at = now + expires_in`)。
   - `exchange_code(http, code, verifier, now) -> Result<OAuthToken>` / `refresh(http, refresh_token, now) -> Result<OAuthToken>`(HTTP 经 `HttpClient` 接缝,测试用捕获替身零联网)。
   - Anthropic OAuth 常量(client_id/authorize/token/redirect/scopes/beta header)**集中一处**,注释标明「公开的 Claude Code OAuth client 尽力值,活端点由用户实跑验证」。
2. **`AnthropicProvider` bearer 模式**:新构造(如 `new_oauth` 或 auth-mode 字段)→ 头用 `Authorization: Bearer <access_token>` + `anthropic-beta: oauth-2025-04-20` + `anthropic-version`,**去掉 `x-api-key`**。verify 与 chat 共用此路径。
3. **oauth 存储** `~/.ridge/oauth.json`(0600,独立于 config.json;`RIDGE_OAUTH` 可覆盖):`parse`/`upsert`/`get`(纯,keyed by provider id)。
4. **`login --claude` CLI 流**(main.rs `run_login` 加分支):PKCE+state → 打印授权 URL(引导用户浏览器授权,**Claude 不碰凭据**)→ 读粘回的 code → `exchange_code`(真 `ReqwestClient`)→ 存 oauth.json → best-effort 校验(bearer 打一次最小调用;失败仅告警不判失败,因活 wire 我无法核验)。
5. **`real_provider` 解析接线**:存在有效/可刷新的 Anthropic OAuth token(且顶层无更高优先级 key)→ `needs_refresh` 则 `refresh` 并落盘 → 构造 bearer-mode `AnthropicProvider`。**chat 真用它**。

## 边界(不做)

- **不做浏览器自动拉起 + 本地回调抓 code**:走「打印 URL + 用户粘回 code」流,合安全姿态(同 NLM 登录:用户本人操作,Claude 不碰凭据),且免起本地 HTTP server。
- **不做泛化多供应商 OAuth 抽象**:Anthropic 专用(YAGNI,无单产物的工厂);他家 OAuth 将来再泛化。
- **不做补全热路径逐调用刷新**:本轮仅**构造时**刷新(`needs_refresh`→`refresh`→落盘);逐调用刷新留 `ponytail:` 注释与 iter-44。
- 不改引擎/命令/Hook/既有 api_key 路径(OAuth 是新增旁路,api_key 优先级不变)。
- 凭据绝不进 config.json / NLM / notes;oauth.json 0600;access/refresh token 不回显于日志。

## 确定性验收信号(离线纯函数/数据结构断言,无计时/网络/PTY/进程信号)

门禁 `cargo test --workspace && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` 全 **exit 0**。新增测试:
1. `pkce_challenge_is_s256_base64url_of_verifier`:固定 verifier → challenge 等于已知 base64url(sha256) 向量(RFC 7636 附录 B 向量)。
2. `authorize_url_carries_challenge_scopes_state`:纯字符串含 `code_challenge=`、`code_challenge_method=S256`、scopes、`state=`。
3. `parse_token_response_sets_expiry_from_now`:给定 `{access_token,refresh_token,expires_in:3600}` + now=1000 → `expires_at_epoch==4600`。
4. `needs_refresh_true_past_expiry_false_when_fresh`:纯,now 传参两断言。
5. `oauth_store_roundtrips`:`upsert`→`parse`→`get` 身份;空/坏文本 → 空表(不 panic)。
6. `anthropic_oauth_headers_bearer_beta_no_apikey`:bearer-mode provider 经捕获 `HttpClient` 替身发一次 → 断言头含 `Authorization: Bearer …` 与 `anthropic-beta`、**不含** `x-api-key`(复用 provider/lib.rs:1709 捕获替身模式)。
7. `exchange_code_and_refresh_parse_via_fake_http`:替身返固定 token JSON → `exchange_code`/`refresh` 得预期 `OAuthToken`(零联网)。
8. base64url 手写编码:对 RFC 向量断言(纯)。

## 诚实的验证边界(gate 之外)

确定性门禁证明**机器正确**(PKCE/URL/解析/刷新逻辑/存储/头构造,给定常量)。门禁**不能**证明 Anthropic 活 OAuth 端点/client_id/所需 system prompt/beta header 值当前有效——那属 ToS 灰区且需用户订阅。**用户实跑 `ridgecode login --claude` 才是活验证**;常量集中且 base_url 可配置覆盖,便于将来校准。报告须显式声明此边界。

## 依赖(安全原语,非「铁律」外置能力)

新增 workspace 依赖 `sha2`(S256,crypto 绝不手搓)+ `getrandom`(PKCE verifier/state 安全随机)。二者是**基础安全原语**,非 CLAUDE.md「内核精简铁律」所指的外置可装能力(RAG/AST/tiktoken 那类应走 MCP/SKILL);OAuth 登录是内核 auth 特性,无法做成 skill。base64url 是纯编码(非 crypto),手写 + 测,省一依赖。

## 停机

单轮;收尾:回写 ARCHITECTURE(§7 auth 增 OAuth 旁路 + oauth.json 存储 + bearer provider 模式)、报告(含验证边界声明)、提交带 `iter-43`、替换 NLM 架构来源。iter-44 备选:补全热路径逐调用刷新 + 泛化他家 OAuth(仅当有真实需求)。
