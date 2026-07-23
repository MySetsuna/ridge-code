# CONTRACT · iteration-49 —— 订阅体系收口(刷新收敛 + Gemini + 跨订阅分工布线)

> maker = NotebookLM(guidance-49);checker = 对抗评审(Gemini device flow 驳回改 authcode、verify-provider 驳回改 sub-agent 路径、刷新核落点修正)。前置:iter-48 六目标已落。

## 目标(按序)

1. **P0 token 刷新收敛**(agent login.rs):`resolve_valid_token(provider_id) -> Option<OAuthToken>` 统一核(oauth_get → needs_refresh → refresh → 落盘);启动回退链与 TUI switch_provider oauth 分支共用(热切路径从「不刷」升级为「刷」——switch_provider 改 async 或主环异步分支)。验收:模拟过期 token 断言触发 refresh 且 oauth.json 更新;三调用点收敛单测。
2. **P0 Gemini/Google One 订阅**(provider::oauth + login.rs):`GOOGLE OAuthConfig` const(authcode + localhost 回调,与 OPENAI 同型;禁 Google SDK,恒走 ReqwestClient);`login --gemini`;`oauth_defaults("google")`;bearer 走 OpenAiProvider 兼容端点(base_url 可覆盖;活验证诚实边界同前)。验收:authorize_url(&GOOGLE) 端点/scopes 断言;oauth.json "google" 键位独立。
3. **P1 sub-agent 引 use_oauth 档**(agent agents/build_agents):`provider:` 字段引 use_oauth 命名档时,build_agents 经 oauth 凭据构造 bearer provider(跨订阅 maker/checker 分工的物理布线:主 agent Claude Max,reviewer 引 chatgpt-plus)。验收:构造断言 —— use_oauth 档被 sub-agent 解析出 bearer provider,无凭据时明确降级不崩。
4. **P1(条件)codex wire 补丁**(provider::openai):**仅当**用户实跑 `login --codex` 活验证失败(401/404)才做 —— OpenAiProvider 加 wire 分支(端点/header/SSE 包装),StreamAcc 层隔离。验收:Codex 格式 SSE 块喂 StreamAcc 断言解析出 Completion。未触发则顺延。
5. **P2 /models 缓存持久化**(agent config/tui):`~/.ridge/cache/models.json` 落盘;启动即用缓存 ctx_window,/models 刷新。验收:缓存写读纯核单测。
6. **P2 TUI 渲染加固**(agent tui render):未闭合 ``` 围栏在静态提交不致整屏错染。验收:md_line_spans 未闭合围栏回归测试。

## 边界(不做)

- 不做 device flow(YAGNI,authcode 覆盖);不引 Google SDK;不做 verify 节点 LLM 化(verify 恒确定性纯逻辑,铁律);不做 provider 层 429 重试(任务级重试已有,实测证明需要再说)。

## 确定性验收信号

门禁 `cargo test --workspace && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` 全 exit 0;各目标新增测试见上。

## 停机

单轮;各目标独立提交。收尾:回写 ARCHITECTURE/PROJECT-STATE、报告、LOG、NLM 替换来源、提交带 `iter-49`。
