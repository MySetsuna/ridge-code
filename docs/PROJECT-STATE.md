# RidgeCode PROJECT-STATE(2026-07-23 · iter-48)

> 本文是 NotebookLM 中**唯一**的 RidgeCode 来源,每轮迭代覆盖式更新并替换。
> 结构:A. 项目定位与北极星(稳定段)→ B. 近期迭代与验证证据 → C. 能力对照与差距 → D. 开放问题与请 NotebookLM 定夺的问题 → E. 已落地架构详情(codegraph 生成的代码事实全文)。

## A. 项目定位与北极星(稳定段,少改)

RidgeCode 是一个**模块化、跨领域可扩展的通用 agent 框架**(单二进制 `ridgecode`,Rust workspace,当前 v0.4.0+,住 `crates/agent`)。既能像 Claude Code 写代码,又能做编程以外的事。**加新能力 = 加一个 MCP server 配置或一个 SKILL.md,而不是改 Rust 源码。**

四层解耦(已全部落地):
1. **内核** —— `langgraph` 纯图引擎(StateGraph + Pregel BSP 超步 + checkpoint 时间旅行,零 LLM 依赖);
2. **协议** —— MCP 客户端接万物(多 server、命名空间、降级不崩);
3. **知识** —— 声明式 Skills(SKILL.md)+ 自定义命令(~/.ridge/commands/*.md)+ 项目规则注入;
4. **协作** —— maker≠checker(reason/act 生成、verify 独立只认确定性信号)+ 只读 sub-agent;
   附**安全层**:权限门 + 危险命令硬拦截 + 写沙箱 jail + 外置沙箱包裹 seam(docker/wsl)。

**已锁定决策(不变量,改码前须知)**:maker≠checker;reducer 显式;引擎零 LLM;外置能力走 MCP/SKILL 不进内核;provider 边界(第三方 SDK 包在 trait 后);一切注入块有界截断;危险命令拦截不可绕过、sub-agent 恒只读;注入块有序稳态利 prompt 缓存。内核 token 节约四判据已收束:历史有界自动压缩 / 静态底噪极小 / Lean 输出 / durable-state 事实驱动。

## B. 近期迭代与验证证据

- **iter-43**:OAuth(PKCE)订阅登录 `ridgecode login --claude` 端到端 —— 接 Claude Pro/Max 订阅(非 API key)。纯核 `provider::oauth`(Pkce S256 / authorize_url / exchange_code / refresh / needs_refresh,HTTP 走 HttpClient 接缝离线可测);凭据独立 `~/.ridge/oauth.json`(0600);启动 key 全无时自动回退订阅凭据,过期自动刷新。
- **iter-44**:tracing 可观测埋点核心闭环(execute_tool_call / reason LLM 调用 / write_run)。
- **iter-46**:外置沙箱包裹 seam —— config `sandbox_cmd` 模板({cwd} 占位),run_shell 经 sandbox_argv + run_argv 直执 argv,真隔离交 docker/wsl;危险命令拦截仍在包裹之前(纵深防御)。
- **iter-47**:根因修复 LLM 调用无超时致任务永久冻结 —— ReqwestClient 加 connect_timeout(30s) + 非流式整请求超时 + 流式逐块 idle 超时(RIDGE_HTTP_TIMEOUT 可调,默认 180s)。
- **2026-07-23(上午)**:无 key 可开 TUI(ScriptedProvider 兜底提示 /login);TUI /login 页 claude-oauth 行;RIDGE_PROXY env;Models 页 provider 分栏。
- **iter-48(本轮,pi-agent 式多订阅接入)**:①OAuth 纯核泛化 `OAuthConfig` per-provider(`ANTHROPIC`/`OPENAI`)+ `TokenWire::{Json,Form}` 分流 + `post_form` 接缝;②`login --codex` 接 ChatGPT Plus/Pro(PKCE + 本地回调 listener 127.0.0.1:1455 state 防 CSRF + `parse_callback_path` 纯核);③订阅档一等公民化 `ProviderProfile.use_oauth`(serde-default 零破坏),OAuth 登录自动登记 claude-max/chatgpt-plus 命名档;④TUI codex-oauth 行(openai 流贴回调 URL 提码,零 listener);⑤/provider 页 oauth 徽标 + 热切订阅档;⑥TUI 遗留 4 bug 勘察:2 个 af88070 已修(旧装版所见)、Shift+Enter 为 Windows 平台约束(Ctrl+J 恒可用,已注提示)、光标卡首行根因修复(`up_fallback_is_home`:长首行折行时 Up 先跳行首免历史突变)。
- **验证证据**:`cargo test --workspace`(217 tests)+ `cargo fmt --all --check` + `cargo clippy --workspace --all-targets -- -D warnings` 全 exit 0(2026-07-23 实跑)。

## C. 能力对照与差距(距「pi-agent 式多订阅接入」)

对标 pi-agent(pi-coding-agent / @mariozechner/pi-ai)的订阅接入架构:OAuth PKCE 授权模块 + Token 存储刷新模块 + Bearer/SSE 流式客户端,`/login` 内选择订阅源即可绑定。

| 能力 | RidgeCode 现状 | 差距 |
|---|---|---|
| API-key 登录(14 内置 preset) | ✅ CLI + TUI,先校验后落盘 | — |
| Claude Pro/Max 订阅 OAuth | ✅ CLI `login --claude` + TUI 行;PKCE;自动刷新;启动回退 | 活端点校验依赖用户实跑 |
| ChatGPT Plus/Pro(Codex)订阅 OAuth | ✅ **iter-48**:CLI `login --codex`(本地回调)+ TUI codex-oauth 行(贴 URL) | codex wire 活验证待用户实跑;或需 Responses API 最小补丁 |
| 订阅档一等公民化 | ✅ **iter-48**:use_oauth 档自动登记,/provider 可列可切(oauth 徽标) | 热切路径不刷 token(ponytail,启动路径有自动刷) |
| 其他订阅源(Gemini/Google One 等) | ❌ 无 | NLM 里程碑排 iter-49 |
| 跨订阅 maker/checker 分工 | ❌ 无 | NLM 里程碑排 iter-50(主 agent Claude Max、reviewer Codex) |

## D. 开放问题(请 NotebookLM 结合全部来源定夺)

1. **codex wire 验证策略**:若用户实跑 `login --codex` 发现 ChatGPT 订阅 token 打 api.openai.com/v1 标准 chat completions 401/404(订阅实走 chatgpt.com backend Responses API),最小补丁应做成什么形态?OpenAiProvider 内 wire 分支,还是独立 CodexProvider?
2. **Gemini/Google One 订阅接入(iter-49)**:Google OAuth 的 device flow 与现 OAuthConfig 抽象兼容吗?需要哪些字段扩展?
3. **跨订阅 maker/checker(iter-50)**:sub-agent `provider:` 字段已可引命名档 —— use_oauth 档能直接被 sub-agent 引用吗?verify 节点独立用另一订阅的最小改动路径?
4. **token 刷新收敛**:现刷新散在启动回退链与(不刷的)热切路径;是否值得收敛为统一「取有效 token」纯核(needs_refresh → refresh → 落盘)供三处共用?

---

# E. 已落地架构详情(codegraph 代码事实,2026-07-23 快照)

## 0. 总览

单二进制 `ridgecode`(住 `crates/agent`,package 名 `agent`)。Rust workspace,6 crate,edition 2021,v0.4.0+:

```
crates/langgraph   纯图引擎(零 LLM 依赖):StateGraph + Pregel 超步 + checkpoint
crates/provider    LLM 抽象:LlmProvider(Anthropic/OpenAI/Scripted)+ SSE 流式 + web 工具
crates/tools       std-only 文件读写/编辑/搜索/shell/危险命令拦截
crates/mcp         MCP 客户端(JSON-RPC + 可插拔传输 + 命名空间)
crates/agent       装配层:ReAct 图(reason→act→verify)+ TUI/headless + 二进制 ridgecode
crates/eval        离线评测 harness(ScriptedProvider 场景:pass/stuck 等)
```

依赖统一在根 `Cargo.toml [workspace.dependencies]`:tokio / thiserror / anyhow / serde(_json) / async-trait / reqwest(rustls-tls,无 openssl 系统依赖)/ tracing / ratatui 0.29 / crossterm 0.28。CI 门禁:`cargo test --workspace` + `clippy -D warnings` + `fmt --check` 全绿。

## 1. langgraph:图引擎

- **`state.rs::GraphState`** trait:`type Update` + `fn apply(&mut self, Update)`。reducer **显式强制** —— 每种状态自己声明合并语义,防并发覆盖丢更新。
- **`graph.rs::StateGraph`** 构建器:`add_node`(异步节点,收状态快照、返回 Update delta)、`add_edge`(静态边,重复 from = fan-out)、`set_entry`、`add_conditional_edge`(router 看**合并后**状态,优先于静态边)、`compile`(校验入口存在、静态边不悬空 → `CompiledGraph`)。节点无出边则隐式 END。
- **`CompiledGraph::invoke_with(initial, RunConfig, Option<Checkpointer>, Option<StreamEvent 发送端>)`** → `run_loop`:**BSP 超步**执行环。每超步:state.clone 快照 → frontier 全节点吃同一快照、`tokio::spawn` 并发 → 同步点统一 `apply` → 据合并后状态路由。`RunConfig`(Clone)`.max_supersteps` 防跑飞 → `GraphError::StepLimit`。另有 `resume`(从 checkpoint 续跑)。
- **`invoke_best_of`**(Best-of-N 投机分支,**通用引擎原语**):`Arc<Self>` + JoinSet 并发 N 份初始状态各跑一遍图;失败分支丢弃,调用方评分器 `Fn(&S)->i64` 择优、平分低索引确定性胜;空/全败 → `GraphError::NoWinner`。分支间无副作用隔离(并发真实写会互踩),真实 agent 接入需先做每分支工作区隔离。**iter-42**:agent 侧曾造的 `workspace.rs`(worktree/影子拷贝隔离 + 胜者合回)与 `branch_score` 评分器 17 轮零接线 → 作被证伪的过度设计**已删**;`invoke_best_of` 作库原语保留(langgraph 自带测试)。
- **`checkpoint.rs`**:`Checkpoint{step, frontier, state}`;`Checkpointer` trait;`MemoryCheckpointer`(append-only,`history/get(step)/latest` 时间旅行)+ `FileCheckpointer`(落盘)。
- **`StreamEvent`**:`NodeFinished` / `Superstep{state}`,供 TUI 实时渲染。
- 错误:库层 `thiserror`(`GraphError`),节点错误归一化 `BoxError`;应用层 `anyhow`。

## 2. agent:状态、图装配与护栏

### 2.1 AgentState + Patch reducer(`lib.rs`)

`AgentState` 关键字段:`task / messages / last_action / tool_output / approved / steps / issues / pending_call(ToolCall) / total_tokens / budget_tokens / stall(无进展轮数) / err_streak(连续报错轮数) / history(Vec<Message>,发 provider 的真身) / todos / modified_files(BTreeSet,有序稳态) / last_error / signal_block`。
`Patch` enum(Message/Action/ToolOutput/Approved/PendingCall/AddTokens/SetStall/SetErrStreak/PushHistory/SetTodos/RecordModified/SetLastError/BumpStep/Batch…)经 `apply` 合并 —— messages 是 append reducer,其余多为覆盖/累加。

### 2.2 图形态与路由

`build_llm_agent_full(provider, mcp, approver, skills, token_bus, agents, read_only)` → `build_core` 装配 reason→act→verify:

- **reason**:`to_messages` 组上下文 → provider 补全(流式经 token bus)→ 产 `pending_call` 或 finish。
- **act**:`execute_tool_call` 执行内置/MCP 工具;副作用工具过 `Approver` 权限门;shell 过 `is_dangerous_command` 硬拦截;未知工具名归错(`"tool error: 未知工具…"`,喂熔断计数,防幻觉工具静默空转)。
- **verify(独立 checker)**:`verify_ok` 只认确定性信号 —— shell 输出前缀 `exit 0:` / `tests: passed`;或「模型 finish 且无失败信号」(开放式任务放行)。不信模型自述。
- 路由:`reason_route`(must_stop 或 finish → verify,否则 act);`verify_route`(approved 或 must_stop → END,否则回 reason)。

### 2.3 多层停机护栏(loop engineering:停机是设计的一半)

`must_stop` = `steps >= MAX_STEPS`(硬回合上限)‖ `over_budget`(total_tokens ≥ budget_tokens,0=不限)‖ `stalled`(连续 MAX_STALL 轮工具输出相同)‖ `circuit_broken`(连续 MAX_ERR_STREAK 轮报错 —— 报错内容轮轮不同时 stall 不触发,由此兜底)。全 O(1) 字段判定。终态 `halt_reason` 另做诊断重标签,含 `context_rotted`(压缩后仍超 CONTEXT_ROT_TOKENS = 单条巨消息压不掉)。

### 2.4 Token 运行时(上下文对长任务保持有界)

- **历史有界**:`est_tokens` 本地估算(CJK≈1 tok/字、ASCII≈1/4 字符,零依赖不引 tiktoken);`to_messages` 超 `AUTO_COMPACT_TOKENS` 自动 `compact_history`(保尾部 AUTO_COMPACT_KEEP;压缩窗口首端裁悬空 role=tool 防端点 400)。
- **观测截断**:`bound_observation` 超 `OBS_CHAR_CAP` 头尾各半保留 + 截断标记(标记刻意避开 error/failed/exit 等判据词,不污染成功/失败信号);磁盘原文不动,可 `read_file` 区间重取。
- **Durable State 事实块**:`durable_state_block` 把 `modified_files` + `last_error` 编成 `<durable_state>` 注入 messages **末尾**(role=system;首部 system prompt 冻结利缓存)。体量 O(去重文件数),不随步数膨胀 —— 事实驱动而非消息驱动。
- **静态底噪极小**:`BASE_SYSTEM` terse(含 edit>write、search+区间 read、web 闭环、确定性验证、lean 输出、truncation 契约、勿删测试、signal_write 沉淀);工具 description 有守卫测试卡 <120 字/工具。

### 2.5 信号复利(跨会话记忆)

`.ridge/signals/*.md` 结构化 Signal(id/status);`signal_write` 工具沉淀发现/摩擦/待办;`load_open_signals` 按 id 排序(确定性)→ `signals_block` 有界注入块(超 SIGNALS_BLOCK_MAX 截断)→ run 启动时经 `load_signal_block` 进 `AgentState.signal_block`。opt-in 自动抽取器(`signal_extract_enabled`)在 run 收尾用 LLM 提炼本轮信号;失败自动落信号。

### 2.6 sub-agent(恒只读)

agent 定义 = frontmatter `.md`:内置 fastcontext/explorer/reviewer 编进二进制 + `~/.ridge/agents/*.md` 用户目录(同名覆盖)。`Agents{defs, providers}` 注册表;`provider:` 字段引 config 命名档(FastContext 走廉价模型省钱)。主 agent 经 `dispatch_agent` 自动派 / TUI `/agent` 手动派。**双重防御只读**:`READONLY_TOOLS = ["read_file", "search"]` 白名单裁剪(`readonly_tool_specs`),不下放写/shell;`SUBAGENT_MAX_STEPS = 15`。独立上下文、只回结论文本,不回灌工具轨迹 —— 省主上下文 token 的关键。

### 2.7 Skills 与项目规则

`SKILL.md` 声明式技能:`RIDGE_SKILLS_DIR` env > config `skills_dir` > `~/.ridge/skills`。cwd 的 `CLAUDE.md`/`AGENTS.md` 经 `load_project_rules` 注入 system prompt。`@file` 引用注入正文(MENTION_CAP=20000 截断)。

## 3. provider:LLM 抽象

- `Role/Message`(tool_calls + tool_call_id 归一化)、`ToolSpec`(name/description/JSON Schema)、`ToolCall`(arguments 已解析成 Value)、`Completion{text, reasoning, tool_calls, usage}`、`CompletionRequest`；Anthropic `type=="thinking"` 与 inline `<think>` 均进入 `reasoning`。
- **`LlmProvider`** trait:`complete` + `complete_streaming`(SSE 逐 token 回调,默认回落到 complete —— 不支持流式的 provider 零改动)。实现:`AnthropicProvider`(`new` = `x-api-key` 档;**`new_oauth` = OAuth 订阅 bearer 档**,iter-43:`Authorization: Bearer` + `anthropic-beta` + system 首块注入 Claude Code 身份)/ `OpenAiProvider`(HTTP,`HttpClient` 传输接缝可离线测,`StreamAcc` 累积 SSE)/ `ScriptedProvider`(离线按序吐预设 Completion,demo/测试用)。
- **`SwapProvider`**:`Mutex<Arc<dyn LlmProvider>>` 热切换 —— TUI `/model` `/provider use` 换芯不重建图;锁只持到 clone,不跨 await。
- **`ReqwestClient` 超时护栏**(iter-47 根因修复):真实 HTTP 客户端设 `connect_timeout(30s)` + 逐调用超时——非流式整请求超时,**流式则响应头等待 + 逐块 idle 超时**(`tokio::time::timeout` 包 `send`/`chunk`)。**没有它**,端点(如 GLM 经代理)偶发流式卡住会令 reason 节点在超步内 `await` 永不返回 → 任务永久冻结(`max_supersteps`/`MAX_STEPS` 在超步**之间**检查,拦不住超步**内**的 hang)。超时秒数 env `RIDGE_HTTP_TIMEOUT` 可调(默认 180)。
- web 工具:`web_search`(GFW 探测自动换引擎、无 key 多引擎 fallback)+ `fetch_url`(抓正文),`WebFetch` 接缝可离线测。
- **`provider::models`**(iter-29):`fetch_models(HttpClient, kind, base_url, key)` 向 `{base_url}/models` 发鉴权 GET(`HttpClient::get_json` —— 对称 `post_json`,默认 Err 仅 `ReqwestClient` 真实现)→ `parse_model_list`(**纯函数**,兼容 OpenAI/OpenRouter/Anthropic 的 `{data:[..]}` + 顶层数组,坏/空 → 空列表;context 多路探测含嵌套 `top_provider.context_length`)→ `Vec<ModelInfo{id, context: Option<u64>}>`。供 TUI `/models` 列实时模型 + 上下文大小。

## 4. tools:std-only 真实工具

`read_file` / `read_file_range`(区间读)/ `edit_file`(唯一匹配替换,0 处或多处即报错引导)/ `apply_edits`(多文件原子批量:全体校验唯一匹配 → 落盘,单写失败回滚已写)/ `write_file` / `search`(递归 + glob,SEARCH_CAP 截断提示)/ 跨平台 shell / `is_dangerous_command` 灾难命令硬拦截(任何模式下不可绕过)。
**写沙箱 jail**(`execute_tool_call` 里 write/edit/apply_edits 路径守卫):`jail = jail_guard(allow_jailbreak(), path)`,`jail_guard` 纯函数 —— 关(默认)时钳在**进程 cwd 子树**,越狱 → `BLOCKED`。**地址越狱开关**(iter-34)= 进程级 `AtomicBool`(`set_allow_jailbreak`/`allow_jailbreak`,启动读 `config.allow_jailbreak`、TUI `/jailbreak [on|off]` 实时切):开则 `jail_guard` 放行 cwd 外写,**但只放宽这一条** —— 危险命令拦截、受保护路径(tests/.git)守卫、只读模式不受影响;开启时 TUI 顶栏红底 `⚠越狱` 徽标警示。默认关,持久化经 `/config set allow_jailbreak true`。
**外置沙箱包裹 seam**(iter-46,`run_shell` 的 OS 隔离):config `sandbox_cmd`(模板,`{cwd}` 占位)→ 进程级 `SANDBOX_CMD: OnceLock`(`set_sandbox_cmd` 启动装,set-once)。配了则 `run_shell` 走纯核 `sandbox_argv`(`sandbox_split` 引号感知分词 → `{cwd}` 替换 → **user_cmd 作最后单个 arg 追加**)+ `tools::run_argv`(`Command` 直执 argv,**不经 cmd/sh 二次解析** → 免跨平台引号地狱);真隔离交平台(docker/wsl)。**纵深防御**:危险命令拦截 + 约束守卫在包裹**之前**先过(即便沙箱也挡灾难命令)。合「外置能力不进内核」铁律——内核只提包裹接缝,不引 OS 隔离 FFI(AppContainer/Landlock 均驳:重 FFI + 难确定性测)。留空 = 宿主直跑(现状),零行为变化。纯核 `sandbox_split`/`sandbox_argv` 离线可测。

## 5. mcp:客户端协议层

`McpTransport` trait(`request` + `notify`,JSON-RPC 信封内部处理)→ `StdioTransport::spawn` 子进程 / 闭包 FnTransport(测试)。`McpClient`:`initialize` 握手(initialize 请求 + `notifications/initialized` 通知,合 MCP 规范)、`list_tools`、`call_tool`(拼 content text 块)、`namespaced` = `server__tool`。`McpError`(Transport/Rpc/BadResponse)。
agent 侧 `resolve_mcp`:多 client 各自握手 + 列工具 → 归一化 `McpTools{specs, router}`;**降级不崩** —— 单 server 失败跳过。config 支持多 `mcp` 声明 + 兼容旧 env `RIDGE_MCP_CMD`。

## 6. 入口与交互(`main.rs` / `tui.rs`)

- **`main`** 分流:`login` 子命令(iter-37 api-key / iter-43 `--claude` OAuth,见 §7)在解析任务前拦下;否则有 key(`RIDGE_API_KEY`/顶层内联/顶层 `key_env`→auth/`providers[]` 档)→ 真实路径,key 全无 → **回退 OAuth 订阅凭据**(iter-43,`resolve_claude_oauth_provider`)→ 仍无 → 离线脚本 demo。
- **可观测(tracing,iter-44)**:`init_tracing`(启动装 `EnvFilter` subscriber,默认 warn,`RUST_LOG` 可调)+ **核心闭环埋点** —— `execute_tool_call`(入口 debug / 危险拦截 warn / 出口 debug+ok)、reason 节点 LLM 调用(request/response debug 含 step/tokens)、`write_run` 收尾(`info` 含 halt reason/steps/tokens)。纯 side-channel 事件,不改控制流。`RUST_LOG=agent=debug,langgraph=debug ridgecode …` 可观 agent 每步。确定性可测:同步埋点(`execute_tool_call`)经线程本地捕获 subscriber 断言事件内容。
- **TUI**(TTY,ratatui):**主屏内联 REPL**(iter-26)—— `Viewport::Inline(LIVE_HEIGHT=14)`,不占备用屏;历史行经 `Ui.commits` 队列 → `flush_commits` → `Terminal::insert_before` **静态提交**进终端原生 scrollback(原生滚动/选取/搜索保留),入历史即永不重绘;Live 视口五槽定长布局(iter-31):顶状态行(provider·model·**ctx%**·tokens·todo·spinner;busy 时徽标转暖色)+ 流式尾巴(`stream_tail` 尾 K 行)+ **忙碌粘条**(仅 busy 有高度:`fmt_busy_bar` = phase·读秒·token·tok/s·todo d/n)+ 动态高度输入框 + **自定义底栏**(`config.status_bar` 模板,`render_status_template` 替 `{provider}{model}{ctx}{tokens}{cwd}`,空则 `DEFAULT_STATUS_BAR`);审批模态覆视口、↑↓ 滚动接 Paragraph 偏移;TODO 变更快照历史化(无侧边栏)。`flush_commits` 每块前置空白行分栏(iter-31 需求 5)。计时/计量纯函数化:`token_rate`/`ctx_percent`/`fmt_busy_bar` 全收数值入参,时钟采样(`task_started: Instant`)与 token 累计(`ui.stream_tokens` 经 `est_tokens`)留主环,测试零 wall-clock;ctx% 分母 `meta.ctx_window`(默认 200K,`/models` 命中当前模型即缓存真实窗口)、分子 history `est_tokens` 和。**视觉与反馈**(iter-28)—— 语义化色角色层 `Role`→`role_color`(ANSI 16 具名色,零 RGB 硬编码,尊重终端主题);`md_line_spans` 行级 md 轻渲染**只在静态提交时染**(围栏/块内/标题/行内 code/bold,未闭合按字面);`fold_lines(20)` 呈现层折叠(留头 + `+N` 尾标);审批 detail 按 `+`/`-` DiffAdd/DiffDel 着色;`splash_frame` 启动 banner 列渐显(tick 驱动,纯函数帧序列;iter-36:`SPLASH_TICKS=14` 更平滑、`indent`+`splash_pad` 居中动画);**落定 banner `splash_block(width)`**(iter-36 修「标识乱了」):宽 ≥ `SPLASH_W(48)` → 居中艺术字(逐行 `trim_end` → 每行 ≤ width,`flush_commits` 的 Wrap 不再折行撕裂)+ 英文 tagline,窄 → 紧凑单行 `◆ RidgeCode`;busy 流尾青色 `█` 呼吸游标。**显示语言(iter-36)**:所有用户可见串(TUI 提示/命令/Panel/状态栏/弹窗、CLI help/日志/phase)一律**英文**;代码注释与 `lib.rs` 模型侧串(system prompt/observation)保留中文(非显示)。**高级输入**(iter-27)—— CSI u best-effort(`DISAMBIGUATE_ESCAPE_CODES`,Drop 时 Pop);`InputState{buffer,cursor,history,hist_idx,draft}` 多行编辑状态机:光标插删/Left/Right/Home/End/多行列钳位/CJK 安全,**首逻辑行 Up=历史召回**(draft 存还),Shift/Alt+Enter/Ctrl+J 换行,真光标 `set_cursor_position` **按 wcwidth 显示列**(iter-30,`cursor_display_col`/`char_cells`/`str_cells`;CJK/emoji 占 2 格,`cursor` 仍字符序,只渲染换算用显示宽 —— 修中文输入光标偏左根因;`wrapped_rows`/浮窗宽同口径);**补全浮窗**:`Popup{items,selected,anchor}` 纯文本补全 —— 打 `/`/`@` **即弹随打随滤**(iter-35:`Insert`/`Backspace` 后重算 `build_popup`,余字符 →None 自然关;Tab 仍显式触发),行首 `/` 补命令(`SLASH_COMMANDS`)、`@` 补路径(单层 read_dir ≤20 有界),Enter 经 `apply_completion` 写回缓冲。**交互页 Panel**(iter-35,取代 iter-32 的 ModelPick 浮窗特例):`Ui.panel: Option<Panel{kind,title,query,rows,view,sel,editing}>`,模态居中覆视口 = 搜索框(随打随滤 `panel_filter` 纯函数)+ 过滤列表(选中高亮)+ 选中动作。六命令开页:`/config`(就地编辑:↑↓ 选键→Enter 编辑→`persist_config`+`apply_config_live` live 应用→刷新)、`/provider`(Enter `switch_provider` 切档)、`/tools`(只读)、`/model`(无参 = 实时模型页,Enter `swap_model`+缓存 ctx_window)、`/agent`(只读)、**`/login`**(iter-38 登录页:↑↓ 选内置 preset→Enter 就地输入 key 掩码→Enter 异步校验+接入);`panel_action` 纯路由,`panel_enter` 分派(先 clone 选中项释放借用再 mut;Login 的 key 提交因需联网校验走主环异步分支而非 `panel_enter`)。键位模态优先级 = 审批 > **Panel** > 浮窗 > 输入。`swap_model`/`switch_provider` 单点收敛热切换(密钥 `current_api_key` env 优先/回落 config 内联),文本命令与页共用。**输入排队**(iter-33)—— busy 时 Enter → `InputAction::Queue` 入 `Ui.queued: VecDeque`(空闲 → `Submit`);起任务/跑命令逻辑收敛为**主环顶唯一提交点**(消费 `pending_submit`,键入提交与 `done` 后 `queued.pop_front()` 接跑共用),消除重复 spawn 码;中断 `clear` 队列(abort=取消全部待跑);忙碌粘条 `⏳N` 示待跑深度。**事件驱动主环**(iter-23)—— 阻塞读线程转发键盘事件入 tokio mpsc,`tokio::select!` 六路复用,dirty 标记空闲零重绘;纯决策函数 `approval_action` / `should_draw` / `wrapped_rows`(`input_height`+`commit_height` 共用)。`TuiApprover`(请求 tokio unbounded,应答 std sync_channel);**Bracketed Paste**(iter-24,best-effort + `sanitize_paste`,粘贴并入 InputState 光标事务)与**动态输入高度**(3..=8 行);斜杠命令 `/help /cost /login[ list| <id> <key>] /model[ <name>]（无参=实时模型页,iter-37 合并 /models） /provider[list|use|add] /agent /commands /config[ set] /jailbreak[ on|off] /reset /compact /tools /exit` **只在 TUI**;**自定义/skill 命令(iter-39)**:`~/.ridge/commands/*.md`(name=文件名,frontmatter `description`,body=Prompt 模板)+ 每个 skill(name→/name)经 `load_commands` 合成命令表(文件优先于同名 skill),`resolve_command` 查、`expand_command` 展开(`$ARGS`→参数,无占位且有参则追加);`/name [args]` 由 `run_command` 展开置 `ui.run_task`,主环唯一提交点据此**以任务身份起跑**(内置命令永远先匹配、不被 shadow);命令名经 `DYNAMIC_COMMANDS`(set-once)并入斜杠补全(多数列表/配置类开交互 Panel,见上)(`run_command` 为 `async` —— `/models` 内 `fetch_models` 裹 15s `timeout` 防挂);每轮 `save_session` 落盘。
- **headless**(非 TTY:管道/CI/重定向):逐行 stdin 当任务串行跑,跨行携带 history,恒 `AutoApprove`(危险命令仍硬拦截)。
- **`run_once`**(CLI 带任务):一次性;`--every <dur>` = 时间触发器(常驻,每轮重载信号、单轮出错不掀翻循环)。
- CLI:`--cwd` / `--yolo`(skip_danger)/ `--resume`(kill-9 恢复会话)/ `--read-only` / `--every`。
- 每轮落 `.ridge/runs/<id>/`(manifest.json + trace.json)审计;`trace_and_report` 打终态 + `halt_reason`。

## 7. config(`~/.ridge/config.json`,env 覆盖)+ auth 密钥库(iter-37)+ OAuth 订阅登录(iter-43)

providers 命名档(kind/model/base_url/**key_env**)/ 顶层 `provider/model/base_url` + 可选内联 `api_key` / 可选顶层 **`key_env`**(iter-37,`login --default` 设)/ budget_tokens / skip_danger / 多 `mcp{name,cmd,args}` / skills_dir / **`status_bar`**(iter-31 底栏模板,占位 `{provider}{model}{ctx}{tokens}{cwd}`,空用默认)。`RIDGE_CONFIG` 指路径;TUI `/config set` 持久化回写;TUI `/provider add ...` 经 `parse_provider_add`+`config_add_provider` 增/覆盖档(`api_key` skip_serializing,明文永不因工具落 config)。

- **内置供应商 preset 表**(iter-37,`PROVIDER_PRESETS`,编进二进制,纯静态):世界顶级 `openai/anthropic/gemini/grok` + 中国顶级 `glm/kimi/deepseek/qwen/hunyuan/minimax` + 聚合 `openrouter/siliconflow/together/groq`;每条 `{id,label,kind,base_url,default_model,key_env}`。纯核 `preset_by_id`/`preset_to_profile`/`apply_login`(据 preset 加档 +(make_default)set 顶层四键 + 抹顶层残留 `api_key`,产物**绝不含 key**)。
- **`~/.ridge/auth.json` 密钥库**(iter-37):`login` 存 API key 处,**独立于 config,key 不进 config**;按 `key_env` 名索引(纯核 `auth_parse`/`auth_upsert`/`auth_get`,坏/空不崩;仅收字符串值,**OAuth 凭据另存独立 `oauth.json`**,此处跳过任何对象值);写盘 best-effort chmod 600(unix)。`RIDGE_AUTH` 可指路径。
- **key 解析**(`resolve_key_env(name,&auth)` = env[name] 非空 或 auth[name];`ProviderProfile::resolve_key_with(&auth)` = 内联 `api_key` > 该档)。**顶层 key 解析收敛为单一 `resolve_top_level_key(cfg,auth)`**(iter-41,原 `real_provider`/`current_api_key` 各写一遍 → 消重):`RIDGE_API_KEY` env → 顶层内联 `api_key` → 顶层 `key_env`→(env/auth)。启动 `real_provider` = `resolve_top_level_key` → 否则 `providers[]` 迭代(auth-aware)。`build_agents`/TUI `switch_provider`/`current_api_key` 同 auth-aware(后者即调 `resolve_top_level_key`)。
- **`ridgecode login` 子命令**(iter-37,`main` 在解析任务前拦下):`login [--list]` 列内置表;`login <id> [KEY] [--model M] [--name N] [--no-default] [--no-verify]` → key 写 auth.json(缺 KEY 从 stdin 读,**绝不回显/落 argv**)+ 据 preset 写档 +(默认)设顶层默认。
- **OAuth 订阅登录(iter-43,`login --claude`)**:接 Claude Pro/Max 订阅(区别于 api-key 档)。纯核在 `provider::oauth`(离线可测):`Pkce`(S256=`base64url_nopad(sha256(verifier))`,手写 base64url + `sha2`/`getrandom`)、`authorize_url`、`parse_token_response`(`expires_at = now+expires_in`,now 传参)、`OAuthToken::needs_refresh(now)`、`exchange_code`/`refresh`(HTTP 走 `HttpClient` 接缝);Anthropic 常量(client_id/authorize/token/redirect/scopes/beta/system-identity)集中 `oauth::anthropic_oauth`。流程:`run_login_claude_oauth` 生成授权 URL(**Claude 不碰凭据**,用户本人浏览器授权)→ 读回贴的 `code#state` → `exchange_code` → **独立 `~/.ridge/oauth.json`**(纯核 `oauth_parse`/`oauth_upsert`/`oauth_get`,0600,`RIDGE_OAUTH` 可指路径)→ best-effort bearer 校验(失败仅告警)。启动 `resolve_claude_oauth_provider`(**key 全无时回退**):读 oauth.json → `needs_refresh` 则 `refresh` 并落盘 → 构造 `AnthropicProvider::new_oauth`。**诚实的验证边界**:确定性门禁证「机器」(PKCE/URL/解析/刷新/存储/头),Anthropic 活端点/client_id/所需 system 前缀属 ToS 灰区,由用户实跑 `login --claude` 验证;常量集中且 base_url 可配置覆盖。**区域**:claude.ai 有地域限制(实测受限区直连撞 `app-unavailable-in-region`);受限区浏览器授权 + token 交换/刷新均需代理(reqwest 自动读 `HTTP_PROXY`),`login --claude` 启动提示已注明。**活校验进度**:iter-43 经代理 E2E 到达 claude.ai 登录页,授权 URL(client_id/redirect/scope/PKCE/state)被 claude.ai 接受并 returnTo 保留 → 常量获活佐证;token 交换待可用订阅账号。
- **多订阅接入(iter-48,pi-agent 式)**:纯核泛化 —— `oauth::OAuthConfig` per-provider 常量集(`ANTHROPIC`/`OPENAI` const;client_id/端点/scopes/`extra_query`/`token_wire`),`authorize_url`/`exchange_code`/`refresh` 收 `&OAuthConfig`;`TokenWire::Json`(anthropic)/`Form`(标准 RFC 6749,openai)分流 token body,`HttpClient` 新增 `post_form` 接缝(默认 Err)。**`login --codex`** 接 ChatGPT Plus/Pro:PKCE 授权 → 本地回调 listener(`127.0.0.1:1455`,循环 accept 跳过杂请求、state 防 CSRF,纯核 `parse_callback_path` 提 code)→ form 交换 → `oauth_upsert("openai")`。TUI:`/login` 页 `codex-oauth` 行(`CODEX_OAUTH_ROW`)、`begin_oauth`/`apply_oauth_code` 泛化(openai 流贴回调 URL 提码,TUI 不起 listener);`/login --codex` 快捷。**订阅档一等公民化(G3)**:`ProviderProfile.use_oauth: Option<bool>`(serde-default 零破坏);OAuth 登录自动登记 `claude-max`/`chatgpt-plus` 命名档(`register_oauth_profile`);`/provider` 页 oauth 徽标、`switch_provider` 对 oauth 档走 oauth.json 凭据 bearer 热切(ponytail:同步路径不刷 token,近期过期仅提醒;启动路径自动刷新)。启动回退链泛化:anthropic → openai 依次找订阅凭据。**诚实验证边界**(同 iter-43):openai client_id/端点/codex wire 属活验证,先 bearer + 标准 chat wire,`RIDGE_MODEL`/`RIDGE_BASE_URL` 可覆盖。
- **连接校验(iter-38)**:登录**先校验**再落盘 —— `verify_key_via(&dyn HttpClient, kind, base_url, key)`(经 `fetch_models` 打 `{base_url}/models` 鉴权 GET;`get_json` 非 2xx 返 Err → 错 key/坏端点如实失败;HttpClient 接缝可离线测)+ `verify_provider_key`(真 `ReqwestClient` + 15s `timeout`)。连通 → 存 + 激活 + `✓ connected (N models)`;失败 → 不落盘。CLI `--no-verify` 跳过(离线配 / 不支持 `/models` 的端点兜底)。
- **交互登录页(iter-38)**:TUI `/login`(无参)开 `PanelKind::Login` 供应商选单(14 家 preset,可搜索)→ ↑↓ 选一家 → Enter 就地输入 key(**掩码 `•`**)→ Enter 异步校验(主环唯一异步 Enter 分支,`login_apply_verified`)→ 连通则写 auth+config、热切、关页;失败留输入态可重试。`/login <id> <key>` 快捷路径同样校验。
- **Hook 系统(iter-40)**:config `hooks: [HookCfg{event, matcher, command, blocking}]` + `notify`。四事件 `pre_tool`/`post_tool`/`session_start`/`stop`;`matcher`=工具名子串(`*_tool`,缺=全部)。**触发点**:`execute_tool_call` 前后串 pre/post_tool(`run_pre_tool_hooks` 的 blocking hook 非 0 退出 → BLOCKED 拦下工具;`run_post_tool_hooks` fire-and-forget),`main` 启动 `session_start`、各任务毕 `stop`(tui done / run_once / headless)。hook 命令经带 `.env()` 的 Command 跑(注入 `RIDGE_TOOL`/`RIDGE_TOOL_ARG`,**不设全局 env**,BSP 并发安全),且**先过 `is_dangerous_command` 灾难 denylist**(iter-41:`hook_is_safe` —— hook 与 run_shell 工具同守「危险命令拦截不可绕过」,命中即不执行 + 审计 `hook_blocked`)。**内置(总是安全)**:会话审计留痕(`audit`→`~/.ridge/audit.log`,前置 epoch 秒)、任务完成响铃(`notify`→`\x07`);格式化/危险确认作为 config 示例 hook 随 `install.ps1` 下发。全局 `HOOKS: OnceLock`/`NOTIFY: AtomicBool`(set-once,与 jailbreak/dynamic-commands 先例一致);纯核 `hooks_for_event`/`audit_line` 可单测。

## 8. 设计不变量(改码前须知)

1. **maker ≠ checker**:verify 独立、只认确定性信号。
2. **reducer 显式**:新状态字段必须进 Patch + apply,不得绕过。
3. **引擎零 LLM**:langgraph 不依赖 provider;预算/成本等 app 概念不进 GraphError。
4. **外置能力走 MCP/SKILL 不进内核**:squeez/RAG/AST/tiktoken 等一律外置。
5. **provider 边界**:第三方 SDK 包在 trait 后。
6. **有界注入**:一切注入块(观测/信号/事实/@file)皆有上限截断。
7. **危险命令拦截不可绕过**;sub-agent 恒只读。
8. **有序稳态**:注入块用 BTreeSet/排序,字节稳定利 prompt 缓存。

## 当前迭代目标

- 执行 `REQ-20260801-01`：重构 TUI 展示与交互，使实际模型输出/回答清晰可读，工具调用默认折叠且可显式展开，并保持终端适配、性能与既有安全语义。

## 已验证代码事实

- `crates/agent/src/tui/app.rs::Ui::push_chunk` 已将 `StreamChunk::Answer` 与 `StreamChunk::Reasoning` 分道累积。
- `crates/agent/src/tui/eventfmt.rs::summarize_event` 将工具调用转摘要/预览；`crates/agent/src/tui/app.rs::flush_commits` 以 `fold_lines` 后静态提交。
- `crates/agent/src/tui/draw.rs::draw` 已按 live 输出尾、状态条、输入框、底栏布局；`InputState` 为纯方法编辑器。
- `preflight.py --strict`：exit 0；CodeGraph index：ready。

## 相关模块与 symbol

- `crates/agent/src/tui/app.rs`：`Ui`、`Ui::push_chunk`、`flush_commits`
- `crates/agent/src/tui/draw.rs`：`draw`、`draw_panel`
- `crates/agent/src/tui/eventfmt.rs`：`summarize_event`
- `crates/agent/src/tui/input.rs`：`InputState`
- `crates/agent/src/tui/tests.rs`：TUI 纯逻辑回归测试

## 最近完成与当前 diff

- 最近完成:`REQ-20260801-01` v0.2.0 需求晋级、执行 intake 与首个 Markdown 提交增量。
- 当前 diff:`samples/config.json` 用户已有修改；`.iteration/` 与需求治理文档为本轮运行态；TUI app/render/mod/tests 已有本轮实现与测试变更。

## 验证状态

- 历史记录（2026-08-01）：`requirements_gate.py assert-task-executable`、`preflight.py --strict`、Rust 质量命令均 exit 0；当前闸门结果以下方“本轮验证”为准。

## 当前失败信号与风险

- 失败信号:`state_snapshot.py build` 首次因旧 PROJECT-STATE 缺少本工作流章节而失败，补齐后已通过。
- 风险:深研任务仍 `in_progress` 且报告为空；TestBackend 已覆盖窄终端折行与 CJK/emoji 宽度，真实 PTY 长任务帧延迟、复制/搜索仍待验收。

## 架构边界

- 目标/非目标:`改 TUI 展示/交互层及其直接 provider 解析接缝；不改 langgraph 协议、安全门与确定性验证。`
- 锁定决策:`reasoning 只来自实际 StreamChunk；工具默认摘要，detail 有界且显式请求。`
- 基线依据:`main / CodeGraph ready / preflight exit 0。`
- 模块与落点:`TUI app 状态、command/panel 交互、event formatter、draw/layout、input、Anthropic parser 与 tests。`
- 关键接口/直接路径:`StreamChunk → Ui::push_chunk → draw；agent event → summarize_event → flush_commits。`

## 需求—代码—测试追踪

| Active REQ | 状态 | 代码证据 | 测试/质量证据 |
| --- | --- | --- | --- |
| `REQ-20260801-01` | active v0.2.0 | `tui/app.rs`、`command.rs`、`draw.rs`、`eventfmt.rs`、`input.rs`、`mod.rs`、`panel.rs`、`render.rs`、`transcript.rs`、`provider/anthropic.rs` | intake/gate、fmt、build、clippy、workspace tests 与 TestBackend 窄端验收已过 |

## Busy-bar tool intent projection (2026-08-02)

- NLM follow-up 68791fb7-659a-4ad6-a86c-beb7ac694781 returned `PROCEED` after CodeGraph confirmed `AgentState.pending_call` is produced by `reason`, consumed by `act`, and cleared by `Patch::PendingCall(None)`.
- Implemented the minimal TUI projection: `Ui.pending_call` synchronizes at `StreamEvent::Superstep`; submit/abort/done/retry paths clear stale state; `fmt_busy_bar` shows a bounded sanitized tool name and safe argument summary.
- Safety boundary: file/search tools expose only sensitive-marker-redacted, clipped `path`; `apply_edits`/`todo_write` expose counts; shell commands, contents, old/new text, URLs, and credential-like values stay hidden. No graph/protocol/approval/cancellation/key/scrollback semantics changed.
- Deterministic acceptance: `busy_bar_projects_bounded_safe_tool_intent` verifies name visibility, path clipping, control stripping, and payload secrecy. Agent tests: 99; TUI tests: 82.

## Reasoning→Answer transition rail (2026-08-02)

- NLM follow-up 16 proposed a one-cell visual branch rail for the live reasoning→Answer transition. CodeGraph showed `visible_lines` aggregates bounded rendered rows by class, so the implementation deliberately uses adjacent rendered `LiveLineKind` values only; it does not infer hidden reasoning causality.
- `draw.rs::live_rail` now starts a visible reasoning segment with `┌`, marks an immediately following Answer with `╰`, and retains `┃/│/▌/┆` for ordinary Answer, reasoning continuation, focused tool, and detail rows. Every rail is one display cell, preserving narrow-terminal width budget.
- `reasoning_answer_transition_rail_is_bounded` covers transition selection and cell width; `long_reasoning_clamp_preserves_answer_and_input_slots` covers the actual TestBackend frame. Agent tests: 99; TUI tests: 82; clippy/build/fmt passed.
- No new key, state, provider field, hidden-reasoning content, tool-folding behavior, static `insert_before` content, or PTY policy changed.

## Active reasoning tail focus (2026-08-02)

- NLM follow-up 17 proposed step-owned LiveBlock coloring. CodeGraph rejected it: `LiveBlock` has no step, `push_chunk`/`push_reasoning` receive none, and `vitals.step` exists only at draw time; adding a field would invent causal ownership and alter stream merge semantics.
- NLM follow-up 18 was accepted after correction: `active_reasoning_tail_role` uses only `ui.busy`, rendered tail position, and `LiveLineKind::Reasoning`. Busy last-visible reasoning gets existing `Primary` on its rail and real metadata; idle/non-tail reasoning remains `Muted`.
- `active_reasoning_tail_focus_is_render_only` covers pure role selection; `full_tui_frame_survives_narrow_cjk_and_escape_text` checks busy `Primary` versus idle `Muted` in TestBackend. No new state, key, provider, scrollback, or hidden-reasoning semantics.

## Live Answer code-fence scope rail (2026-08-02)

- NLM follow-up 19 proposed a code-fence scope rail for streaming Answer rows. Local correction keeps the scope bounded to state confirmed while walking the visible tail: `├` marks a visible fence line and `┊` marks a visible body line after a visible opener.
- `live_code_rail` is render-only and uses existing ANSI16 `Role::Border`/`Role::Muted`; every rail remains one display cell. If the opener was tail-clipped, the frame retains the ordinary rail rather than guessing or persisting parser context. No `LiveBlock` field, scrollback, key, provider, or Markdown content semantics changed.
- `live_code_fence_rail_is_bounded` covers role selection and width; the code path in `full_tui_frame_survives_narrow_cjk_and_escape_text` covers fence/body rails, colors, and a tail-clipped opener in TestBackend. Agent tests: 99; TUI tests: 82.

## Visible Reasoning→Tool→Answer connector rail (2026-08-02)

- NLM follow-up 21 proposed a full-chain rail. Local correction is render-only: `draw.rs` sees `LiveLineKind`, `previous_kind`, and the focused-tool marker, but `LiveLine` has no block/step causal identity; the rail therefore describes only adjacent rendered rows.
- A ToolSummary immediately following a visible Reasoning row uses one-cell `├` (`Primary` when focused, otherwise `Info`); an Answer immediately following ToolSummary/ToolDetail uses one-cell `╰`. Ordinary tool focus `▌`, details `┆`, and Answer/Reasoning rails remain unchanged elsewhere.
- `reasoning_tool_answer_connector_rail_is_bounded` covers symbol/width selection; the full TestBackend frame covers the actual `Reasoning → focused Tool → Answer` rendering and ANSI16 colors. No tool outcome, hidden reasoning, key, provider, scrollback, or data-model semantics changed. Agent tests: 99; TUI tests: 83.

## Deterministic Tool failure rail (2026-08-02)

- NLM follow-up 22 suggested elevating failed Tool rails. CodeGraph verified the existing shared `tool_output_failed` predicate feeds `summarize_event`, whose failed summary/detail lines already carry `Role::Error`; `ToolBlock::from_lines` preserves those colors.
- `live_tool_rail_role` now maps only Error-colored ToolSummary/ToolDetail rows to the existing `Role::Error` rail. It does not inspect strings, add a status field, or infer outcomes. Answer header pinning is implemented separately with the existing Answer budget and transient Markdown state.
- `tool_failure_rail_uses_existing_error_role`, `summarize_event_overviews_tools`, and the full TestBackend frame pass. Agent tests: 99; TUI tests: 84.

## Answer context anchor (2026-08-02)

- NLM follow-up 22 judged a minimal Answer header anchor worthwhile, but warned about row budgeting and clipped Markdown fences. Local implementation limits it to the latest `LiveBlock::Answer` and the already computed `answer_budget`.
- When the latest Answer exceeds its budget, `pin_answer_header` emits only `header + one UI ellipsis + tail`; with fewer than three Answer rows or an empty header, existing tail behavior remains. The anchor is Answer chrome, not fabricated reasoning or persisted state; `visible_lines(max_rows)` still owns all row caps and input/reasoning/tool slots.
- A visible opening fence in the pinned header advances the existing transient Markdown state; a later opener outside the visible slice remains ordinary. `answer_header_anchor_preserves_budget_and_fence_boundary`, the long-frame TestBackend regression, and all workspace quality gates pass. Agent tests: 99; TUI tests: 85.
- NLM follow-up 23 then proposed raw PTY ANSI instrumentation and a viewport floor. Local correction: no PTY harness exists, `flush_commits`/`insert_commit` retain native `insert_before`, and the draw path already has independent output/input/status slots with `LIVE_HEIGHT=14`; no raw-log, batching, or duplicate floor mutation was added.

## Focused detail rail glow (2026-08-02)

- NLM follow-up 24 proposed focused-block rail glow, resource-pressure styling, and a Superstep divider. CodeGraph corrected the first premise: `ToolBlock::append_live_tail` marks only the focused summary; contiguous visible `ToolDetail` rows had no focus rail.
- The draw loop now carries the existing focused marker across adjacent visible detail rows; `live_rail` maps focused detail to existing ANSI16 `Primary`, while `live_tool_rail_role` still lets existing `Error` colors override. No `focus_index`, LiveBlock field, new key, outcome inference, or data-model change was added.
- Resource-budget styling remains rejected because the current TUI contract exposes observed `ctx_used`/`task_tokens` and already has deterministic `context_pressure_role`; a new Superstep divider remains deferred because it needs a separate visible step-transition contract. `live_rail_uses_semantic_kind_and_focus_only`, the focused-detail TestBackend frame, and agent/TUI 99/85 tests pass.

## Fenced language badge (2026-08-02)

- NLM follow-up 25 proposed language branding for fenced Answer code. CodeGraph verified `md_line_spans` currently colors the complete fence line and the Live rail is one cell; the local implementation therefore uses a separate bounded `‹language›` badge only for a valid opening token.
- `fence_language` accepts only ASCII language-token characters and at most 10 display cells. The badge consumes prefix width, and is suppressed when fewer than four content cells would remain; without the badge, the original fence text is rendered unchanged. The parser advances from a normalized bare fence only when the badge is actually shown, so Markdown semantics and static `insert_before` scrollback remain untouched.
- TestBackend coverage proves `rust` appears in a wide Live frame; pure tests cover token validation and normalization. `cargo test -p agent` passes agent 99 and TUI 86 tests; no PTY, key, provider, approval, cancellation, or data-model change.

## PTY/native scrollback evidence audit (2026-08-02)

- NLM follow-up 14 is `NEEDS_MORE_EVIDENCE`: first run an experimental Windows PTY audit of native Ctrl+F and mouse-copy fidelity; do not change `insert_before` batching or ANSI policy yet.
- Local ratatui 0.29 source confirms `insert_before_no_scrolling_regions` renders a temporary `Buffer` of `Cell`s, while `CrosstermBackend::draw` emits style/control sequences from those cells. RidgeCode sanitizes committed model text before `insert_before`; styling is introduced at the backend boundary.
- `markdown_commit_renders_through_inline_scrollback`, `static_scrollback_preserves_order_and_sanitizes_controls`, and `full_tui_frame_survives_narrow_cjk_and_escape_text` pass. They do not prove native Windows Terminal search or mouse-copy paste fidelity, so no production refactor was made.
- A real Windows Terminal/PTY run must still verify styled CJK/emoji search and copy with zero escape fragments; until then retain `Viewport::Inline` and native `insert_before` semantics.
- Local harness probe found no `winpty`, `pywinpty`, `ptyprocess`, `pexpect`, `ConEmuC`, or repo PTY dependency; only `wt.exe`/`wsl.exe` are present, so no automated native PTY claim is made.
- NLM re-queried with the audit evidence and again advised no code change. Its proposed sanitizer/reset/width remedies are conditional failure hypotheses only; a failed physical test must first isolate the smallest violated contract.
- NLM follow-up 23 proposed optional raw ANSI logging and a minimum transcript floor; both remain rejected pending bounded physical evidence and a demonstrated layout failure. Keep the existing native scrollback and adaptive slots.
- Deep research task `b578cfd6-26b3-49f3-af59-1282376d7a9f` latest poll at `2026-08-02T08:37:52+08:00`: `in_progress`, 10 candidate sources, empty report, not imported. Do not call `research_import` yet.

## Manual deep-research import (2026-08-02)

- User manually imported NotebookLM source `8862d715-ffad-4288-b7eb-173260e2dcff`, `Architectural Optimization of High-Frequency Inline Terminal User Interfaces for AI Coding Agents`; `notebook_get` now reports four sources and the source body is 40,374 characters.
- Its usable strategy themes are bounded/virtualized visible rendering, collapsible tool blocks, incremental stream formatting, width-aware layout caching, native `insert_before` scrollback, async input/render separation, and progressive keyboard-protocol compatibility. Its code/API examples and performance claims remain hypotheses until matched to this repository and ratatui version.
- This manual source is distinct from task `b578cfd6-26b3-49f3-af59-1282376d7a9f`: the task API still says `in_progress`, report empty, `imported: null`. Do not relabel the task completed or invoke `research_import`; use the manually imported source as strategy evidence only.

## Contextual input hints (2026-08-02)

- NLM follow-up 26 proposed stronger reasoning/Answer contrast, contextual `Ctrl+O`/`Ctrl+R` guidance, and a badge-width guard. Local verification found the contrast, bounded tool affordance, and badge fallback already present; only the discoverability gap was real.
- `status.rs::input_chrome` now uses width-tiered ANSI16 hints: medium/wide idle and queue states expose the already implemented `Ctrl+R` reasoning inspection and `Ctrl+O` tool details; narrow queue mode remains `Q:[N]`. No key, focus, parser, provider, PTY, or input-state semantics changed.
- `cargo test -p agent --offline --quiet -- --test-threads=1` passed: 99 agent + 87 TUI tests. Workspace test, clippy, build, and `ridgecode` smoke also passed; smoke exit `0`, with only the pre-existing missing `codegraph-mcp` executable warning.

## Reasoning inspection state hint (2026-08-02)

- NLM follow-up 27 prioritized a PTY write-fidelity audit, but its raw ANSI logging proposal conflicts with the approved no-raw-log/unchanged-native-scrollback boundary and no reproducible Windows PTY harness exists. Its repeated reasoning recolor and language-badge proposals are already satisfied or redundant locally.
- The real remaining discoverability gap was state feedback: after existing `Ctrl+R` expands actual reasoning, the input chrome still said only `Ctrl+R reasoning`. `LiveTranscript::is_reasoning_expanded` now feeds `input_chrome`; collapsed shows `Ctrl+R reasoning`, expanded shows `Ctrl+R collapse`. No key, input-state, hidden-reasoning, provider, parser, PTY, or scrollback semantics changed.
- TestBackend full-frame wiring plus `cargo test -p agent --offline --quiet -- --test-threads=1` passed: 99 agent + 87 TUI tests.

## Inline viewport floor audit (2026-08-02)

- NLM follow-up 28 proposed body recolor, duplicate floor state, and extra pressure gauges. CodeGraph/local facts reject all three: reasoning already uses `DarkGray` + `DIM/ITALIC`, `context_pressure_role(ctx)` already styles the observed status bar, and `draw` reserves fixed input/status slots around bounded live rows.
- A bounded TestBackend probe kept `Input` and the status slot visible at `12×6` and `18×8`; `8×4` is physically clipped but remains panic-free. Added `tiny_frames_keep_input_slot_visible` as a permanent regression; no production layout/state/key/scrollback change.
- Targeted result: 99 agent + 87 TUI tests passed.

## Manual report reconciliation (2026-08-02)

- The manually imported 40,374-character source is readable. Its strategy themes align with the current bounded LiveTranscript, collapsed ToolBlock/Tool History, inline viewport, native `insert_before`, width-aware rendering, and async input/render goals; its dependency versions, sample symbols, performance figures, and code blueprints remain hypotheses, not repository facts.
- NLM follow-up 29 proposed focus/action decoupling, a running-tool marker, and historical tool-space decay. CodeGraph and tests show focus movement plus Ctrl+O already target the focused live block; `pending_call` already appears as bounded redacted intent in the busy bar, but no deterministic pending-call-to-live-row identity exists; live/history retention is bounded with no measured failure. No production code change.

## Busy focus discoverability (2026-08-02)

- NLM follow-up 30 proposed pending-row correlation, reasoning recolor, and a narrow language-badge fallback. Local CodeGraph rejected the first as unstable without a provider-to-TUI identity, and the latter two as already satisfied; summary-render consolidation remains deferred without measured allocation/latency evidence.
- A local audit found the existing `Alt+↑/↓` live-tool focus action was not advertised while busy. `input_chrome` now shows it only when live tools exist, uses a compact medium-tier string so the focus hint is not right-clipped, and retains the complete reasoning/focus/details set at wide width; no-tool and narrow branches remain bounded. Input routing, Ctrl+O, queue/cancel, provider, scrollback, PTY, and safety semantics are unchanged.
- Targeted result: `cargo test -p agent --offline --quiet -- --test-threads=1` passed 99 agent + 87 TUI; `cargo fmt --all -- --check` and `git diff --check` passed.

## Static reasoning hierarchy (2026-08-02)

- NLM follow-up 32 proposed live reasoning recolor, an active-tail pulse, and narrow badge hiding. CodeGraph verified all three already exist or are width-safe; no duplicate live change was made.
- A distinct gap remained in the static `insert_before` path: `CommitBlock::Reasoning` had Muted color but no DIM/ITALIC modifiers, unlike the Live reasoning projection. `flush_commits` now applies the existing DIM/ITALIC hierarchy only to static reasoning spans; actual text/metadata, Answer Markdown, commit order, native scrollback, and input/tool/provider semantics remain unchanged.
- `reasoning_commit_renders_in_inline_scrollback` now asserts the TestBackend cell modifiers. Targeted result: 99 agent + 87 TUI; fmt and diff checks pass.

## Observed step in busy chrome (2026-08-02)

- NLM follow-up 34 proposed a new Superstep divider, historical tool recession, and narrow badge/rail fallback. CodeGraph rejected the first two as lacking step-owned ToolBlock identity and measured failure evidence; the existing badge/rail width guard already covers the third premise.
- Accepted only the grounded adaptation: pure `fmt_busy_phase` decorates the existing `fmt_busy_bar` with observed `Vitals.step` when step > 0; step 0 remains byte-for-byte unchanged. No new LiveBlock, row, state, key, provider, tool, or scrollback semantics.
- `busy_bar_shows_observed_step_only_when_available` covers the display contract. Full verification: 99 agent + 88 TUI tests, workspace tests, clippy, build, smoke, fmt, and diff checks pass.

## NLM follow-up 35 — PTY evidence gate (2026-08-02)

- NLM ranked a no-code Windows PTY/native scrollback search-and-copy audit first. No production change is authorized before physical failure evidence.
- The proposed Answer floor budget is rejected as unproven/redundant: existing `answer_budget`, `pin_answer_header`, `visible_lines`, and long-frame tests preserve Answer tail plus Input/status slots. The adaptive badge fallback is rejected because actual cell-width guards and the existing badge regression cover its stated narrow failure premise.
- Environment audit found `wt.exe` and `wsl.exe`, but no `winpty`, `ConEmuC.exe`, `pywinpty`, `ptyprocess`, or `pexpect`; automated PTY evidence remains unavailable. Required next evidence: one physical Windows terminal run with styled mixed CJK/emoji Answer text, Ctrl+F, mouse copy, and resize. Keep `insert_before`, ANSI policy, sanitization, and batching unchanged meanwhile.

## NLM follow-up 36 — native search and performance audit (2026-08-02)

- NLM again ranked evidence before code: PTY cross-wrap search/copy, collapsed-tool native search, and 64-block long-text rendering.
- CodeGraph confirms collapsed static `ToolBlock::commit_lines` emits summary only; details remain bounded in `Tool History`, and `panel_filter` searches both summary key and detail value. This is an intentional native-scrollback/collapse trade-off, not a reproduced regression; no scrollback change is made.
- Existing bounds are deterministic: `MAX_LIVE_BLOCKS=64`, `MAX_LIVE_TEXT_CHARS=32768`, `MAX_TOOL_DETAIL_LINES=20`, and 14-row Live viewport. No CPU/latency threshold or failing frame exists; no optimization is authorized. PTY evidence remains pending.

## Known failed approaches

- `state_snapshot.py build` 首次直接套现有旧状态文档失败（exit 1，缺必需章节）；本轮改为补齐稳定章节后重试。
- `cargo test --workspace --offline --quiet` 首次在 `knowledge::tests::builtin_init_command_present_and_overridable` 失败（exit 1）：该测试与 `load_commands_merges_files_and_skills` 共用 `ridge_cmds_<process-id>` 临时目录，默认并行时发生覆盖/删除竞态。已将两测试目录按用途隔离；同一 workspace 命令复跑通过。

## 下一项已批准工作

- `/history`、reasoning 动态钳位、Live 语义侧轨、Reasoning→Tool→Answer connector、失败工具 rail、Answer context anchor、focused detail rail、fenced language badge、`Ctrl+R` reasoning inspection view、宽度自适应输入提示与展开/收起状态提示、busy 工具焦点提示、静态 reasoning 层次样式、busy chrome 观测 step 已落地；下一项先做真实 PTY/原生 scrollback 物理一致性验收，保持不引入批处理/原始日志，深研完成后仍须导入逐条回核。

## 本轮 delta

- 变更:`docs/PROJECT-STATE.md` 结构与 v0.2 状态补齐；`.iteration/decision.json`、`.iteration/context.json`、预算与 intake 运行态；`input_chrome` 增加有工具时的 busy 焦点提示；静态 reasoning 提交增加既有 DIM/ITALIC 层次；busy bar 显示已观测 step；knowledge 命令测试临时目录按测试用途隔离。
- 直接影响:`state_snapshot.py` 可生成有界运行快照；TUI 提交类型与 Markdown 语义渲染路径已更新。
- 验证:`requirements_gate`/`preflight` exit 0；`iteration_gate` 当前因既有用户改动与工作流状态文件超出允许写集而报 `write_scope_exceeded`，不放宽写集掩盖。
- 质量:无 coverage/E2E/Sonar 配置；不宣称覆盖收益。
- Agent 编排:尚未派发；先完成 NotebookLM 冷闸。
- 模型路由:主 Agent；无 Worker。
- Worker 回收:无。

## 本轮完成

- `REQ-20260801-01` 已执行：TUI 新增 `LiveTranscript` 有界块模型，统一承载 answer、reasoning、tool summary/detail、splash；Anthropic 原生 `thinking` 块已接入真实 reasoning 展示链路，reasoning 在超步/任务收尾时以独立灰色静态块保留；首行附真实 step、task token 估算、elapsed 元数据，未观测 step 不伪造；Answer 可见时 Live 默认保留一行真实 reasoning，`Ctrl+R` 可展开真实 reasoning 且仍保留 Answer/焦点工具摘要，思考阶段不钳位；Live 行增加 ANSI16 语义侧轨，焦点突出而不改变数据/输入语义。
- `input_chrome` 现在仅在存在 live 工具块时提示既有 `Alt+↑/↓ focus`；busy 中宽度档采用不右裁焦点键的紧凑提示，宽屏保留 `Ctrl+R`、焦点与 `Ctrl+O` 全量提示；无工具与窄端路径不虚报、不溢出。未改变 `tool_focus_action`、`Ctrl+O`、队列/取消或审批语义。
- 静态 `CommitBlock::Reasoning` 复用 Live reasoning 的既有 `Muted + DIM/ITALIC` 视觉层次；Answer 仍走 Markdown 语义渲染，正文、元数据、`insert_before` 顺序与原生 scrollback 不变。
- busy chrome 复用既有 `Vitals.step`：仅 step > 0 显示 `· step N`，step 0 原样省略；`fmt_busy_phase` 为纯显示辅助，不增加状态、行预算或跨层耦合。
- NLM follow-up 26 的可执行部分已落地：`input_chrome` 按宽度提示既有 `Ctrl+R`/`Ctrl+O` 操作，并保留窄端队列回退；未采纳重复的 reasoning 重着色、事件格式焦点后缀、额外 badge 状态、power meter 或代码深度 rail。
- 工具调用默认显示摘要；`Alt+↑/↓` 在 live 工具块间移动焦点，`Ctrl+O` 切换焦点详情；任务结束后 `Ctrl+O` 或可发现的 `/history` 打开有界 Tool History，`Enter` 展开选中详情；详情上限 20 行，history 索引上限 64，流文本上限 32768 字符；历史预览按终端 cell 宽计折行，长详情裁尾时仍保留选中块 `▸` 摘要；`/tools` 仍只列 MCP 工具。
- answer 保留优先显示；超步清理仅移除回答/思考，工具块继续 live；真实 reasoning 先独立提交为灰色 `💭` 块，最终 Answer 仍走 Markdown 路径；任务完成或中断时转入 `insert_before` 静态 scrollback。Live Answer 复用行级 Markdown 语义色（inline code/bold/header/fenced code），先推进完整行围栏状态再按终端 cell 宽裁切，静态历史仍完整折行；忙碌态输入框以 ANSI16 Warn 显示 `Queue [N]`，空闲态以 Primary 显示 `Input`，窄端降为 `Q:[N]`；底部状态栏依据实际观测 `ctx%` 在 `<80/80–94/≥95` 映射 Muted/Warn/Error，不推断 budget；检视器独立于不可变 scrollback，故可后展开详情。
- 弹窗打开时 `Ctrl+O` 不再穿透切换后台工具详情；流式 token 每次唤醒最多合并 256 chunk，下一轮优先响应取消键。
- live transcript 按当前视口行数以 `VecDeque` 保留尾部，避免忙碌帧反复物化全部工具详情；流文本维护字符计数后仅在超限时裁前缀，截断后仍补回可见回答/思考徽标。
- 未改 langgraph、权限门、危险命令拦截、输入队列/取消语义；provider 仅补 Anthropic thinking 解析；未触碰用户既有 `samples/config.json`、`test_codegraph.ps1`。

## 本轮验证

- `cargo fmt --all` 通过。
- `cargo clippy --workspace --all-targets --offline -- -D warnings` 通过。
- `cargo test --workspace --offline --quiet` 通过：agent 99 + TUI 88 + 1 + 9 + 2 + 43 + 17 tests，含 doctest；此前并行竞态失败已由测试目录隔离修复。
- 新增 `tui::transcript` 有界尾部/徽标/焦点/流文本上限、Answer/reasoning 行预算与动态钳位、Live 语义侧轨与 reasoning 展开回归，静态 reasoning/Answer 分层与 DIM/ITALIC 修饰、真实 reasoning 元数据纯函数与 Live 投影、Live Answer Markdown 语义色/围栏状态/宽裁切回归、fenced language badge、输入框 Submit/Queue chrome 状态、实际 ctx% 压力色边界回归、`reason#N: (final)` 终答 Markdown 路由、`/history` 可发现入口、静态 scrollback 多块顺序/控制序列净化/宽字符约束、静态提交后 Tool History 折叠/展开/分页/上限与 `Alt+↑/↓`/`Ctrl+O`/`Ctrl+R` 输入路由回归、Reasoning→Tool→Answer connector、既有 Error 色失败 rail、Answer context anchor、focused detail rail、busy bar 观测 step；history 详情按 cell 宽折行；inline viewport 采用稳定 14 行上限，由 ratatui 按当前终端高度裁切，Resize 后可放大恢复；TUI 测试组现为 88 项。

## 当前风险与后续

- NLM 远端深研状态不再查询；本轮以手动导入源 `8862d715-ffad-4288-b7eb-173260e2dcff`（40,374 字符）作为唯一理论与策略依据。远端 `in_progress` 仅保留为历史元数据，不作完成或导入证据；本轮未调用 `research_status`、`research_import` 或远端 note 操作。
- 当前实现保留原生 scrollback，详情展开作用于 live 焦点工具块或 Tool History 检视器；`/history` 已补齐可发现入口；Answer 多行时 Live 默认保留一行真实 reasoning，`Ctrl+R` 可展开其余真实尾部，语义侧轨只存在于 Live 投影；TestBackend 已有长 reasoning/input 槽位、多块顺序/净化基线与 Inline Resize 回归，18×8、12×6、8×4 窄端渲染已通过，真实 PTY 长任务帧延迟与复制/搜索体验仍待验收；手动导入源已完成内容读取与本地策略核验。

## 本轮 delta

- 代码：`crates/agent/src/tui/{app,command,draw,eventfmt,input,mod,panel,render,status,tests,transcript}.rs`；`crates/agent/src/knowledge.rs` 测试目录隔离；`crates/provider/src/{anthropic,lib,tests}.rs`；`status.rs`/`draw.rs`/`tests.rs` 补齐 busy 工具焦点提示。
- 工作流：`.iteration/research.json` 已切为 `manual_only`，并记录 `polling_disabled=true`；项目状态与快照已按本轮切片重建并核验。
- 公开资料核验确认 Ratatui inline viewport 的 `insert_before` 适合作为静态 scrollback 边界，键位仍由 Crossterm `KeyEvent/KeyModifiers` 承载。
- Token:未作 `token-usage --all` A/B，不宣称节省。

## v0.2.0 执行增量（2026-08-01）

- 已批准将 NLM 候选渲染方案纳入 `REQ-20260801-01`：需求版本升至 `v0.2.0`；Pending 已晋级并清除。
- `CommitBlock` 已区分普通文本、Markdown 回答与工具块；最终回答由事件类型显式进入 Markdown 路径，不再由渲染层猜测 `🤖` 前缀；同时兼容引擎 `reason#N: (final) ...` 与直接 `(final) ...` 两种事件形态。
- `markdown_lines` 跨行维护代码围栏；回答徽标用 `Role::Primary`，代码/标题/粗体走 ANSI 16 色语义角色；工具摘要/详情折叠边界不变。
- 工具块获会话稳定 id；焦点只存 TUI 状态，模态页/补全浮窗优先，不改变 provider/langgraph 或输入回退语义。
- live answer/reasoning 首行分别显 `🤖`/`💭` 标记，续行不重复；标记与正文仍来自实际 `StreamChunk`，不生成隐藏思考。
- 新增回答语义 span、跨行围栏、无 ANSI 残留、Markdown 提交类型及带 `reason#N` 前缀终答路由测试。
- 显示净化在流入路径执行：剥离 ANSI CSI/OSC 与控制字符，避免污染 live buffer、状态栏及原生 scrollback；详情总上限收紧为 20 行，LiveTranscript 保持 64 块上限并有回归测试。
- `flush_commits`/`insert_commit` 改为泛型 `Backend`，工具摘要与详情合并为一次带色块提交；可用 `TestBackend + Viewport::Inline` 验证 Markdown/工具静态提交确实走 native scrollback 路径。
- `ratatui::backend::TestBackend` 已复现 12×8 窄终端 Markdown 渲染，并以 18×8、12×6、8×4 完整 TUI 帧覆盖 CJK/emoji、输入框、状态栏、live 流与交互面板；宽字符折行通过且无 ANSI 残留。
- 质量证据：`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --offline -- -D warnings`、`cargo build --workspace --offline`、`cargo test --workspace --offline -- --test-threads=1` 均通过。
- 深度调研任务仍异步；截至 2026-08-02 02:06，认证已刷新，`research_status` 仍返回 `in_progress`，报告为空，未导入、未宣称完成。NLM 回答保存在 `.iteration/notebooklm-response.json`，仍须本地代码/终端验收。
- 运行 smoke：`cargo run -p agent --bin ridgecode --offline` exit 0，应用成功连接 NotebookLM MCP；本机 `codegraph-mcp` 可执行文件缺失，但 CLI CodeGraph index 仍 ready。

## 认证与深研最新状态（2026-08-02）

- NotebookLM 认证已恢复：auth-flow verify、`refresh_auth` 返回 success；`nlm login --check` exit 0；`notebook_get` 成功读取笔记本，当前 3 个来源。
- 深度调研仍为 `in_progress`：截至 2026-08-02 07:07，当前 task id 为 `b578cfd6-26b3-49f3-af59-1282376d7a9f`，完整状态仍显示 10 个候选来源、报告正文为空，尚未导入；未获 `completed` 且报告非空前不调用 `research_import`。
- `.iteration/research.json` 已同步上述状态；本轮轮询遇服务端超时仅停止等待，不把超时解释为完成。
- NLM 异步查询 `5217c5e163c7` 已完成；后续查询 `68791fb7-659a-4ad6-a86c-beb7ac694781` 已经 `notebook_gate.py validate-output` 通过。其 `/history`、真实 reasoning 元数据、动态钳位、语义侧轨与“Answer 到达后自动收缩/可手动展开”建议均已按本地代码核验；`Ctrl+R` 作为无副作用 Live inspection 已落地；真实 PTY 验收仍待证据，定时 `insert_before` 批处理仍未采纳。
- 本轮 NLM 先误推“把工具 Diff 着色下沉 render.rs”；CodeGraph 证实 `edit_file` 已由 `diff_lines` 着色、`ToolBlock` 保留逐行 Color，故拒绝重复渲染器。复问后锁定真实缺口：`apply_edits` schema 为 `edits[{path,old_string,new_string}]`，而 `summarize_event` 误读顶层 `path`，折叠态只显一行。
- 已落地 `crates/agent/src/tui/eventfmt.rs::apply_edits_summary`：折叠态显示文件数/编辑数/最多 3 路径；展开态显示有界文件头与 ± 预览；参数仅来自 tool-call，不读磁盘，不改协议、状态裁决、键位、安全门或 scrollback。新增 TUI 回归覆盖四文件摘要上限、遗漏标记与红绿角色。
- 本轮 `cargo test -p agent --offline --quiet -- --test-threads=1` 通过：agent 99、TUI 78；`cargo fmt --all -- --check` 通过。NLM 建议与本地代码/测试已同步至 `.iteration/notebooklm-response.json` 与 Active Note。
- NLM 后续“流式 Answer 主动 insert_before”经 CodeGraph 拒绝：最终 Answer 由完整 final event 一次提交，部分迁移会重复内容或破坏 Markdown 围栏；复议后的 Live 尾部虚拟窗口亦已由 `visible_lines(max_rows)` 与独立输入/状态布局满足。回归现覆盖 100 行 reasoning + 100 行 Answer，最新 Answer 尾、语义侧轨与 Input/Queue 槽均可见。
- NLM 曾建议焦点侧轨与多块详情展开，但 CodeGraph/测试已证明 `Alt+↑/↓` 后 `Ctrl+O` 已作用于焦点块，故拒绝重复实现；其纠正后建议为 Live Answer Markdown 语义着色，已按局部边界落地并通过 agent/TUI 回归；再后建议的 Submit/Queue 输入 chrome 亦已落地，未改键义；资源压力建议仅采纳有真实 `ctx%` 数据的状态栏着色，拒绝虚构 `budget_tokens`/`Danger`。
- NLM 另建议“Answer 段落原子提交与历史元数据持久化”。CodeGraph 核验：流式 Answer 只进 `LiveTranscript`，终答经 `summarize_event` 形成单一 Markdown `CommitBlock` 再走 `insert_before`；故该建议在静态边界已满足，不增设 `answer_buffer`，免重复状态与改变流式语义。

## NLM follow-up 37 — Markdown structure fidelity（2026-08-02）

- NLM 排序：活动工具详情虚拟滚动、Markdown 结构保真、PTY 原生搜索/复制验收。虚拟滚动需新增 `scroll_offset` 与 `Alt+PageUp/PageDown`，超出本轮批准键位/状态边界；当前工具详情已有 20 行确定性上限，暂缓。
- 已采纳纯呈现层缺口：`md_line_spans` 现将引用前缀映射为 ANSI16 `Info` 侧栏 `│`，有序/无序列表保留原缩进与标记并着 `Info`，正文继续走既有 `inline_md_spans`。不改 Answer/Reasoning 内容、fenced-code 状态、静态 native scrollback、工具、键位或 provider 语义。
- 新增 `markdown_structure_preserves_quote_and_nested_list_hierarchy` 与 `live_markdown_structure_stays_within_narrow_cell_bound`；agent 99、TUI 90、workspace tests、clippy `-D warnings`、build、fmt、diff、ridgecode smoke 全通过。Smoke 连接 NotebookLM MCP；仅有既存 `codegraph-mcp` 可执行文件缺失警告。
- PTY/native scrollback 仍须真实终端证据；本机无自动 PTY harness，故不改 `insert_before`、ANSI、sanitization 或 batching。

## Current checkpoint（2026-08-02 09:24）

- 手动导入报告源仍可读：source `8862d715-ffad-4288-b7eb-173260e2dcff`，正文 40,374 字符；与原深研 task 分离。
- 原深研 task `b578cfd6-26b3-49f3-af59-1282376d7a9f` 仍异步 `in_progress`，10 个候选来源，报告空，`imported: null`；未满足 completed+非空前置条件，不调用 `research_import`。
- NotebookLM 仅有一个 Active Note，无过期/已完成 Note；follow-up 37 已写入 Active Note 与 `.iteration/notebooklm-response.json`，输出验证通过。
- `iteration_gate` 仍仅报既有 `write_scope_exceeded`（workflow runtime 与用户既有改动）；预算未超，不放宽写集。

## Research basis override（2026-08-02 09:28）

- 用户明确要求停止原深研 task 的后续 `research_status` 查询；其远端最后已知状态不再作为本轮完成/导入证据。
- 本迭代将手动导入源 `8862d715-ffad-4288-b7eb-173260e2dcff`（40,374 字符）作为唯一理论与策略依据；报告中的代码蓝图、依赖版本、性能数字仍须本地 CodeGraph、测试与实际运行验证。
- `.iteration/research.json` 已切为本地 `manual_only`，记录 `polling_disabled=true`；不调用 `research_import`，不再等待或查询 NLM 深研状态。

## Live Answer fence-context projection（2026-08-02）

- CodeGraph 定位到真实 Live 展示缺口：`draw` 仅从当前可见尾部起算 fenced-code 状态；代码围栏 opener 滚出视口后，代码体会失去语义色与 `┊` rail。
- `LiveLine` 现携带由实际 Answer 文本计算的 `fence_before` 呈现元数据；`append_answer_tail` 只生成有界可见尾行，`draw` 以该上下文驱动既有 Markdown/rail 渲染。Answer/Reasoning 内容、工具、键位、状态与 static `insert_before` 均未改变。
- 回归覆盖长 Answer 隐藏 opener、窄 TestBackend 帧与普通 fence 行；当前 agent 99、TUI 92，workspace tests、clippy、build、smoke、fmt/diff 均通过。该投影保持 32K 文本上限与视口行界。

## Live row renderer cohesion（2026-08-02）

- `draw` 原先在主布局函数内同时处理 Live 行焦点传播、围栏/语言 badge、rail、marker、语义色与 cell-width 预算，耦合高且难以单测。
- 新增纯 `render_live_line` + `LiveRowState`：集中消费真实 `LiveLine`、既有 `Vitals` 与当前宽度；主循环仅负责取有界尾行、传递帧状态、绘制游标与布局。无新增状态所有权、键位、provider、工具、Answer/Reasoning 内容或 scrollback 语义。
- 全量验证：agent 99、TUI 92，workspace tests、clippy `-D warnings`、build、smoke、fmt/diff 均通过；smoke 仅有既存 `codegraph-mcp` 缺失警告。

## Combined reasoning/tool inspection（2026-08-02）

- CodeGraph 发现交互断点：`Ctrl+R` reasoning inspection 时，`visible_lines` 原将焦点工具预算固定为 1 行；`Ctrl+O` 虽改变展开状态，详情却不可见。
- 修复为仅在工具已显式展开时借用剩余 Live 行：焦点摘要始终优先；视口有空间时保留一行实际 reasoning，Answer 仍保留；视口不足时不越界。
- `expanded_tool_details_remain_visible_during_reasoning_inspection` 覆盖组合路径；全量 agent 99、TUI 93，workspace tests、clippy、build、smoke、fmt/diff 均通过。无新增键位或状态语义。

## Non-focused tool compact projection（2026-08-02）

- CodeGraph 复现边界：已展开工具失去焦点后，其详情尾会与后续工具合并裁剪，可能先挤掉该工具摘要；这违背“摘要常驻、详情按焦点检视”的当前 Live 契约。
- `ToolBlock::append_live_tail` 现令非焦点工具回到摘要 + `Ctrl+O` 提示；只有当前焦点工具投影详情。焦点切换不改 `expanded` 数据，也未新增键位、状态、provider、工具协议或 scrollback 语义。
- 新增 `non_focused_expanded_tool_keeps_summary_when_tail_is_tall`；agent 99、TUI 94 定向通过，workspace/clippy/build 门禁亦通过。

## Single-row Answer priority（2026-08-02）

- CodeGraph 发现极窄 Live 预算边界：`Ctrl+R` 检视与焦点工具并存且 `max_rows=1` 时，原预算会先给焦点工具预留一行，导致实际 Answer 消失。
- `answer_budget` 现仅在 `max_rows > 1` 时为焦点工具预留空间；单行时 Answer 优先，reasoning 与工具按零剩余自然裁剪。无新增键位、状态、provider、工具协议或 scrollback 语义。
- 新增 `reasoning_inspection_keeps_answer_at_single_row_with_focused_tool`；agent 99、TUI 95、fmt/diff 通过。

## Busy cursor width reservation（2026-08-02）

- CodeGraph 与 TestBackend 复现：忙碌态满宽 Live Answer 先占满输出行，再追加 `█`，终端裁剪后游标不可见。
- `draw` 仅对忙碌态最后可见行把内容宽度减一格，保留既有 rail/marker/Markdown 投影并让呼吸游标稳定落入视口；旧宽度路径的回归确实失败，修复后通过。
- 新增 `busy_live_cursor_keeps_one_cell_at_width_edge`；无新增状态、键位、provider、工具协议、Answer/Reasoning 内容或 native scrollback 语义。

## Ctrl+O live-collapse event consumption（2026-08-02）

- CodeGraph 复现交互断点：`toggle_details()` 返回的是展开后的状态值；收起时为 `false`，旧的 `toggle_details() || open_tool_history()` 因而会误判未处理并打开历史页。
- `Ui::toggle_details_or_history` 现先判断是否存在 Live 工具；有则消费 Ctrl+O 并切换其详情，无则才打开 Tool History。收起/展开不改工具数据或键义。
- `collapsing_live_tool_consumes_ctrl_o_before_history_fallback` 覆盖“已有历史 + 当前 Live 工具 + 展开→收起”；旧逻辑确实失败，修复后通过。

## Busy tool affordance across width tiers（2026-08-02）

- 本地 TestBackend/`input_chrome` 审计确认：宽度 72–95 的 busy 工具提示只显示 focus 与裸 `Ctrl+R`，隐藏 `Ctrl+O details`；宽度 56–71 亦不提示展开入口。
- 压缩 busy 工具文案：72+ 与 56+ 均保留真实 `Alt+↑/↓ focus · Ctrl+O details · Ctrl+R`（按 cell 宽裁切）；仅改提示呈现，未改键路由、输入、工具、reasoning 或状态语义。
- `input_chrome_exposes_submit_or_queue_mode` 新增宽度档断言；旧文案确实失败，修复后通过，提示仍按 cell 宽裁切。

## Busy reasoning hint state parity（2026-08-02）

- 第二轮本地审计发现：busy + live tools 的压缩宽度档把 `reasoning_expanded` 丢掉，Reasoning 已展开时仍显示裸 `Ctrl+R`，与实际收起动作不一致。
- 72+ 与 56–71 分支现复用既有 `reasoning_hint`；展开态显示 `Ctrl+R collapse`，收起态显示 `Ctrl+R reasoning`。仅修正可见提示，未改键路由、Reasoning 数据、工具详情、状态所有权或行预算。
- `input_chrome_exposes_submit_or_queue_mode` 新增展开态断言；旧实现确实失败，修复后定向测试通过。

## Reasoning toggle requires actual content（2026-08-02）

- CodeGraph 与回归测试复现：无实际 `LiveBlock::Reasoning` 时，`Ctrl+R` 仍会翻转 `reasoning_expanded`；输入 chrome 可进入 `Ctrl+R collapse`，但视口没有 reasoning 可检视。
- `LiveTranscript::toggle_reasoning` 现先确认有实际 Reasoning block；无内容保持收起，有真实 reasoning 才切换。仅修正状态与可见提示一致性，不生成隐藏推理、不改 Answer、工具、键位或 scrollback 语义。
- `reasoning_toggle_is_noop_without_actual_reasoning` 覆盖空 transcript、仅 Answer、真实 reasoning 三态；旧实现确实失败，修复后通过。

## Superstep frontier busy projection（2026-08-02）

- CodeGraph 对照 `CompiledGraph::run_loop` 确认：`Superstep` 事件携带的 `active` 是下一超步 frontier；旧 TUI 却无条件把 `ui.busy` 置 false。
- `Superstep.active` 非空时现保持 busy，输入 Enter 继续入队、Ctrl+C 仍可中断；空 frontier 才恢复空闲 Submit。避免多超步任务同步点短暂显示可发送并覆盖仍运行的 task。
- `superstep_is_busy` 与 `input_action_routes_keys` 覆盖非空/空 frontier 的 Queue/Submit 路由；未改图引擎、节点、provider、键位定义或安全语义。

## Approval resume busy projection（2026-08-02）

- 审批模态期间 `busy=false` 是必要的视图状态，但旧回执分支发送 y/n 后未恢复；任务仍等待后续图事件时，Enter 可误走 Submit。
- `TuiApprover` 回执发送成功才调用 `Ui::resume_after_approval` 恢复 busy；发送失败表示任务已不存活，保持空闲，不制造假运行态。审批内容、权限结果、键位与安全语义不变。
- `input_action_routes_keys` 覆盖恢复后的 Enter → Queue；与 `Superstep.active` 投影共同保持任务未结束期间的单任务入口。

## Task startup handle guard（2026-08-02）

- 末超步同步点到 `done_rx` 消费之间，图任务可能已发出空 frontier 但本地 `task` 句柄尚未收走；仅依据 `busy` 判断会留下覆盖旧句柄的窗口。
- 统一提交点现由 `can_start_task(busy, task_running)` 双条件守护：只有空闲且无旧 task 才启动；done 分支清理句柄后，排队任务仍按原路径接跑。
- `input_action_routes_keys` 覆盖三态：空闲无 task 可启动、空闲有 task 禁止、busy 无 task 禁止；未改任务执行、队列、审批、安全或键位语义。

## Tool History expansion marker（2026-08-02）

- TestBackend 审计确认 Tool History 已能展开详情，但选中展开项与普通可展开项都显示 `▸`，展开状态缺少即时视觉反馈。
- `draw_tool_history_panel` 现按详情状态显示 `▾`/`▸`/`·` 三态 marker；仅改变列表呈现，不改变 ToolBlock 数据、详情上限、搜索、键位或原生 scrollback。
- `tool_history_is_collapsed_and_expandable_after_static_commit` 新增展开 marker 断言；旧输出确实失败，修复后通过。

## Ctrl+O chrome state parity（2026-08-02）

- 本地调用图确认 `Ui::toggle_details_or_history` 的真实路径：有 live 工具时切换详情；无 live 工具时仅在历史非空才打开 Tool History。
- `input_chrome` 现接收已有 Tool History 非空事实：live 工具提示 `Ctrl+O details`，仅历史提示 `Ctrl+O history`，两者皆无则省略 `Ctrl+O`，避免输入框展示不可执行动作。
- 仅改变提示文案与状态参数传递；未改 Ctrl+O 路由、工具详情/历史数据、键位、队列、审批、安全、provider 或原生 scrollback。`input_chrome_exposes_submit_or_queue_mode` 覆盖三态。

## Actual stream channel badge（2026-08-02）

- 本地调用图确认顶部 busy chrome 原先只显示图节点 `phase`；这不能直接说明当前 Live 视口最近收到 Answer、Reasoning 还是 Tool。
- `LiveTranscript::active_channel` 从最后一个真实 `LiveBlock` 派生通道，顶部以 ANSI16 badge 显示 `[THINK]`、`[ANSWER]` 或 `[TOOL]`；无 LiveBlock 时不显示，不伪造隐藏推理或预测模型状态。
- 这是纯呈现投影：不增加第二份流状态，不改变 Answer/Reasoning 内容、工具折叠、队列、取消、审批、安全、provider、图执行或 native scrollback。窄端帧、三通道切换与清理路径由现有 TUI 回归覆盖；本轮 agent/TUI 测试为 `99/99`，workspace、clippy、build、smoke 均通过。

## Input End allocation boundary（2026-08-02）

- CodeGraph 审计发现 `InputState::end` 每次按键都把完整输入复制成 `Vec<char>`，与当前字符索引模型重复分配。
- 现改为从当前字符光标向当前逻辑行尾扫描；保持换行停止、CJK 字符偏移与 `Home/Up/Down` 语义，仅移除整段 buffer 的临时分配。
- `input_state_cursor_editing` 补覆盖 CJK 与换行边界；目标测试通过。该切片不改变输入键义、队列、取消、Answer/Reasoning、工具或 scrollback。

## Top chrome projection boundary（2026-08-02）

- `draw` 原先直接拼装品牌徽标、jailbreak 警示、真实流通道 badge、busy bar 与 ready/todo 状态，布局与 chrome 责任交叠。
- 新增 `top_chrome(ui, vitals, width)` 单一呈现投影；主布局只负责四个槽位、光标、补全浮窗与模态覆盖。实际 `[THINK]/[ANSWER]/[TOOL]`、phase、step、pending intent 与 ANSI16 角色均保持原语义。
- 宽度不足时优先保留安全警示与实际通道 badge，再裁剪 busy/phase 文案；`full_tui_frame_survives_narrow_cjk_and_escape_text` 覆盖 18/12/8 列帧。此重构不新增状态、键位、provider、工具协议或 scrollback 语义。

## Live Answer 尾部物化边界（2026-08-02）

- CodeGraph 审计发现 `LiveTranscript::append_answer_tail` 已以 `VecDeque` 保持有界尾部，却在设置首行 marker 前再次收集成 `Vec`，形成逐帧临时拷贝。
- 现直接在 `VecDeque::front_mut` 设置 marker，再将其交给既有 `append_tail` 消费；保留 Answer 行序、`fence_before`、marker 与 `max_rows` 语义，仅移除一层收集。
- `answer_tail_keeps_fence_context_after_opener_leaves_viewport` 与 `answer_header_anchor_preserves_budget_and_fence_boundary` 通过；随后 workspace tests、clippy、build、smoke、requirements/preflight/notebook 输出闸均通过。未改 Answer/Reasoning 显示优先级、Ctrl+R、工具、输入、provider 或 scrollback。

## Live Reasoning 尾部物化边界（2026-08-02）

- `append_text_tail` 原以尾部 `Vec` 收集后再 `reverse`，每帧处理 Reasoning/非聚焦工具尾部时多一次临时容器与反转。
- 现用容量受 `max_rows` 限制的 `VecDeque` 从尾向前 `push_front`，直接交给既有 `append_tail`；保持行序、marker、尾部裁剪与 Answer/Reasoning 预算语义。
- `visible_tail_is_bounded_before_render_and_keeps_marker`、`long_reasoning_clamp_preserves_answer_and_input_slots` 通过；随后 workspace tests、clippy、build、smoke、requirements/preflight/notebook 输出闸均通过。未改模型内容、Ctrl+R、工具折叠、输入、provider 或 scrollback。

## Pre-Answer Live 时间序边界（2026-08-02）

- CodeGraph 审计发现无 Answer 阶段时，`visible_lines` 分别收集 Reasoning 与非聚焦 Tool，最后再拼接；Reasoning→Tool→Reasoning 可能显示为 Reasoning→Reasoning→Tool，遮蔽模型实际事件顺序。
- 现无 Answer 时共用一个有界 timeline，按 `LiveBlock` 原序追加 Reasoning/Tool；一旦出现 Answer，仍保持既有 Answer 优先、Reasoning 保底一行、焦点 Tool 详情预算。
- `pre_answer_reasoning_and_tool_tail_keep_block_order` 覆盖该回归；随后 workspace tests、clippy、build、smoke、requirements/preflight/notebook 输出闸均通过。未改 StreamChunk、工具协议、折叠键位、Answer/Reasoning 内容、输入或 scrollback。

## Narrow Reasoning marker 边界（2026-08-02）

- `render_live_line` 原把动态 `fmt_reasoning_meta` 全量 marker 放入行，未先扣除 rail/终端宽度；窄端可能由 step/token 元数据占满行，实际 reasoning 文本不可见。
- 新增 `fit_live_marker`：先按 rail 后算可用 cell；元数据放不下时保留 `💭` compact marker，把文本槽位让给真实模型输出；完整计量仍由顶栏显示。
- `full_tui_frame_survives_narrow_cjk_and_escape_text` 增加 12 列、高 step/token 的 Reasoning 帧断言；随后 workspace tests、clippy、build、smoke、requirements/preflight/notebook 输出闸均通过。未改模型内容、Answer 优先级、Ctrl+R、工具、输入或 scrollback。

## Answer 到达时的 Reasoning 视图边界（2026-08-02）

- 用户可在思考阶段用 Ctrl+R 展开 Reasoning；旧路径在 Answer 到达后仍保留展开态，导致回答只占一行、模型结果被检视内容压低。
- `push_answer` 首次开启 Answer block 时现自动恢复默认 Answer 优先视图；连续 Answer 增量不打断用户状态，Ctrl+R 仍可显式重开完整思考检视。
- `answer_arrival_collapses_reasoning_inspection` 与既有 `expanded_reasoning_keeps_answer_visible` 通过；随后 workspace tests、clippy、build、smoke、requirements/preflight/notebook 输出闸均通过。未改 StreamChunk、工具折叠、输入、provider、安全或 scrollback。

## 手动工具焦点锁定边界（2026-08-02）

- 旧路径中 `push_tool` 无条件把最新工具设为 `focused_tool`；用户用 Alt+↑ 检视旧工具详情时，后到工具会突然夺焦点，Ctrl+O 目标随之改变。
- 新增 `focus_pinned`：用户选至非最新工具即锁定焦点，新工具只保持紧凑摘要；再次选至最新工具解除锁定，后续恢复自动跟随。清流、splash、工具淘汰时同步清理锁定状态。
- `manual_tool_focus_stays_pinned_until_latest_is_selected` 与既有工具详情预算回归通过；随后 workspace tests、clippy、build、smoke、requirements/preflight/notebook 输出闸均通过。未改工具协议、详情内容、键位、Answer/Reasoning、输入或 scrollback。

## Medium Ctrl+O 提示一致性（2026-08-02）

- `input_chrome` 在 56+ 列且有 live 工具时原显示 `O details`，但事件路由只接受 Ctrl+O；提示与可执行键义不完全一致。
- 现统一显示 `Ctrl+O details`，并以 56/64 列回归确认仍能保留 details、Reasoning 入口；随后 workspace tests、clippy、build、smoke、requirements/preflight/notebook 输出闸均通过。未改快捷键路由、工具状态、Answer/Reasoning、输入或 scrollback。

## Busy chrome 静态文案边界（2026-08-02）

- 高频重绘审计发现顶栏品牌、JAIL、通道 badge 先经 `format!`/`to_owned` 再进入 `Span<'static>`，静态文案每个 busy 帧重复物化。
- `push_chrome_fit` 现接收静态字符串并直接借用；`push_channel_badge` 用固定 badge 映射，保留已有宽度优先级、compact code、颜色与视觉输出，动态 busy/status 文案路径不变。
- `full_tui_frame_survives_narrow_cjk_and_escape_text` 与输入提示回归通过；随后 workspace tests、clippy、build、smoke、requirements/preflight/notebook 输出闸均通过。未改模型内容、工具协议、键位、输入、provider 或 scrollback。

## Live 行 marker 所有权边界（2026-08-02）

- CodeGraph 审计发现 `render_live_line` 将已有静态 marker `Cow` 再 `to_owned`，并将 fenced-language badge `String` 再 `clone` 后送入 `Span<'static>`；均属高频呈现临时复制。
- 现直接把 marker 与 badge 的已有所有权交给 `Span`，宽度计量先保存 cell 数再移动值；Live 文本、Markdown/rail、颜色、窄端预算与 Answer/Reasoning/Tool 语义不变。未以未测 benchmark 宣称性能收益。
- `full_tui_frame_survives_narrow_cjk_and_escape_text` 与 `live_answer_uses_bounded_markdown_roles_and_fence_state`、fmt/diff 检查通过；随后 workspace tests、clippy、build、smoke、requirements/preflight/notebook 输出闸复核。未改 provider、输入、键位或 scrollback。

## Live Markdown 围栏状态推进边界（2026-08-02）

- `live_markdown_line` 原先先对完整行调用 `md_line_spans` 取得围栏下一态，再对裁切行调用一次；首份 Span 向量只为状态计算，随后丢弃。
- 新增独立 `next_fence_state` 纯函数，静态/Live 路径共享同一围栏切换规则；Live 只对裁切可见行构造 Span，保留 CJK/窄端裁切、代码 rail、Markdown 颜色与 fence context 语义。
- `live_answer_uses_bounded_markdown_roles_and_fence_state`、`clipped_live_answer_uses_actual_fence_context`、`markdown_render_survives_narrow_test_backend` 与 fmt/diff 检查通过；未改模型内容、工具、键位、输入、provider 或 scrollback。

## 非 Answer Live 文本所有权边界（2026-08-02）

- `render_live_line` 原先对每个 Reasoning/Tool 行统一调用 `clip_display_cells`；文本未超终端 cell 宽时，其 fit 分支仍复制一份 `String`。
- 现先计算可见 cell 宽，未超宽则直接移动 `LiveLine.text` 进入 `Span<'static>`；只有超宽行继续走原省略裁切。Answer 仍经独立 Markdown 投影，工具摘要、详情、rail、颜色与窄端预算不变。
- `full_tui_frame_survives_narrow_cjk_and_escape_text`、`clipped_live_answer_uses_actual_fence_context` 与 fmt/diff 检查通过；未改模型内容、键位、输入、provider 或 scrollback。

## LiveLine 借用视图边界（2026-08-02）

- CodeGraph 审计确认 `visible_lines` 原先为每个 Answer/Reasoning/Tool 可见行复制底层 String，随后 `render_live_line` 才决定是否需要裁切。
- `LiveLine<'a>` 现借用 `LiveTranscript`/`ToolBlock` 已净化文本；Answer/Reasoning/Tool 尾部、焦点详情与 fence context 仅搬运引用。渲染层遇超宽行才生成 owned 裁切文本，动态 marker、badge 与 Markdown spans 仍可独立拥有。
- 关键窄帧、Answer anchor/fence、Live 上限与 fmt 检查通过；未改模型内容、工具折叠、键位、输入、provider、安全或 native scrollback。

## Live 多帧压力验收（2026-08-02）

- 既有 TestBackend 回归覆盖多宽度单帧与通道切换，但未证明长任务连续刷新稳定。
- 新增 `live_frame_pressure_stays_bounded_and_stable`：构造 20 轮真实 Reasoning→Tool→Answer，叠加 Ctrl+R 检视与焦点详情展开，在 96/32/12/8 列连续绘制 32/16 帧；每帧 buffer 尺寸固定、无 ANSI 残留、通道 badge 仍可见。
- 这是确定性渲染验收，不使用 wall-clock 阈值或伪造 PTY 性能结论；未改生产状态、键位、工具协议、输入、provider 或 scrollback。

## Responsive modal chrome（2026-08-02）

- CodeGraph 与窄帧回归确认：Panel/Tool History 原先虽不越界崩溃，但标题、查询、列表与操作提示未按 cell 宽度/可用行数分级，低高帧可能丢失可发现键位。
- `modal_rect` 统一模态边界；`panel_title`/`panel_hint`/`panel_query` 按宽度降级，列表项与 Tool History marker 显式按 cell 宽裁切；低高优先保留列表与 `Esc`，有余量再显示查询、详情与 `Enter`。
- `responsive_panel_chrome_keeps_actions_visible_in_narrow_frames` 覆盖 18×8、12×6、8×4 的普通 Panel 与 Tool History；确认 `Esc` 不丢、18 列保留 `Enter`、查询不越界、无 ANSI 残留。未改工具折叠、详情内容、Answer/Reasoning、键义、provider 或 native scrollback。

## Responsive Live slot budget（2026-08-02）

- CodeGraph 审计确认主 Live 四槽原以 `Min(output)+Length(chrome)+Length(input)+Length(status)` 直接布局；高输入或 4–7 行终端时固定总高可超过终端高，Answer 槽位无明确合同。
- `responsive_live_layout` 现集中按垂直优先级分配 `[output, chrome, input, status]`：输出保留 floor，输入保留最小可用槽；终端高不足 6 行时底栏让位，不改变 Answer/Reasoning/Tool 内容或键义；输入区不足 3 行时不放置虚假光标。
- `responsive_live_layout_preserves_output_and_input_under_vertical_pressure` 覆盖 1–14 行槽位连续性，以及 24×4/5/7 实际 Answer、Input、Status 可见性；workspace tests、clippy、build、fmt/diff 均通过。真实 PTY 帧延迟仍未宣称，native scrollback 与审批/取消语义不变。

## Reasoning shortcut truthfulness（2026-08-02）

- CodeGraph 对照确认：`LiveTranscript::toggle_reasoning` 已在无实际 `LiveBlock::Reasoning` 时 no-op，但 input chrome 宽屏仍显示 `Ctrl+R reasoning`，窄端又完全隐藏实际入口，提示与内容状态不完全一致。
- `LiveTranscript::has_reasoning` 现作为唯一呈现事实输入；`input_chrome` 无 reasoning 时省略 `Ctrl+R`，有 reasoning 时宽端显示完整动作，18 列保留 `Ctrl+R`，14–17 列降为 `^R`，展开态保持 `Ctrl+R collapse`。
- `reasoning_hint_tracks_actual_content_at_narrow_widths` 与既有 no-op 回归覆盖无/有/展开三态；workspace tests、clippy、build、fmt/diff 均通过。未生成推理、未改变键路、Answer/Tool 内容或 scrollback。

## Focused tool chrome chip（2026-08-02）

- Alt+↑/↓ 可锁定旧工具且 Ctrl+O 已作用于该焦点，但顶栏原只显示实际通道 badge，用户仍需从 Live 行确认详情目标。
- `LiveTranscript::focused_tool_summary` 现只读已净化的焦点 `ToolBlock` 摘要；`top_chrome` 在有 live 工具且宽度 ≥48 cells 时显示有界 `◈` chip，摘要最多占用固定预算，窄端直接让位给既有 chrome/status。
- `top_chrome_identifies_focused_tool_without_overrunning_width` 覆盖旧焦点与 64/48/32 列边界；workspace tests、clippy、build、fmt/diff 均通过。未改变 Ctrl+O、Alt+↑/↓、工具详情、Answer/Reasoning、输入、审批、安全或 scrollback 语义；依据仍唯一采用手动导入源 `8862d715-ffad-4288-b7eb-173260e2dcff`。

## Answer reasoning visibility chip（2026-08-02）

- Answer 活跃后顶栏原只显示 `[ANSWER]`；真实 reasoning 虽仍由暗行与输入提示表达，但缺少跨宽度的全局可见性标记。
- `reasoning_visibility_chip` 只在存在真实 `LiveBlock::Reasoning`、当前通道为 Answer、且无 live 工具时投影 `THINKING`；展开态显示 `Ctrl+R collapse`，工具焦点 chip 保持优先，48 列以下让位。
- `top_chrome_surfaces_reasoning_visibility_without_tools` 覆盖 96/64/48/32 列及展开态；workspace 108 TUI tests、clippy、build、fmt/diff 均通过。未生成隐藏推理、未改 Ctrl+R/工具折叠/Answer 内容/输入/审批/安全或 scrollback；依据仍唯一采用手动导入源 `8862d715-ffad-4288-b7eb-173260e2dcff`。

## Focused tool budget preserves reasoning（2026-08-02）

- Answer 到达后若焦点工具仍存在，旧预算会把折叠工具摘要与 `[Ctrl+O details]` 提示各占一行，4 行视口中唯一真实 reasoning 行因此消失。
- 默认视图现把折叠焦点工具压成一行摘要，并为实际 reasoning 保留一行；工具仍可由 Ctrl+O 展开，显式展开工具与 Ctrl+R 检视路径不让位。
- `focused_collapsed_tool_preserves_reasoning_row_in_default_view` 覆盖 Answer+Reasoning+焦点工具组合；workspace 109 TUI tests、clippy、build、fmt/diff 均通过。工具摘要、详情上限、键义、输入、审批、安全与 scrollback 语义不变；依据仍唯一采用手动导入源 `8862d715-ffad-4288-b7eb-173260e2dcff`。

## NLM 冷循环与活跃工具详情局部滚动（2026-08-02）

- 用户恢复 NLM 访问后，冷闸以 `user_requested` 通过；认证经专用 Chrome/CDP 刷新并以 `nlm login --check`、NotebookLM 列表验证。当前仅向既有对话询问下一步，未查询进行中深研状态。
- NLM 首选“活跃工具块局部详情滚动”；其响应 `sources_used=[]`、`citations={}`，故只作候选，不替代 CodeGraph/测试证据。NotebookLM note list 仅有 1 条 `Active` 笔记，无 `completed/expired` 条目，未做远端删除。
- 随后强制限定手动源 `8862d715-ffad-4288-b7eb-173260e2dcff` 重问；NLM 仍返回 `sources_used=[]`、`citations={}`，列出代码高亮、动态高度缓存、KKP 三候选。外部解析依赖、未测缓存收益与 Windows KKP 均继续证据门控，未直接实施。
- 本地回核确认 expanded live `ToolBlock` 原只显示最新详情尾部，旧行虽保存在有界块/Tool History，却无 live 回溯入口。已实现最小单焦点偏移：`Alt+PageUp` 向旧详情滚动，`Alt+PageDown` 回到新尾部；仅展开且详情多行时拦截，摘要始终保留，折叠复位，静态 `commit_lines`/native scrollback 不变。
- `focused_live_tool_details_scroll_within_bounded_view` 覆盖 20 行详情、5 行 live 视口、旧/新尾部切换；`tool_detail_scroll_action` 与输入 chrome 提示覆盖快捷键和浮窗优先级。`cargo test -p agent --offline --quiet -- --test-threads=1`：99 + 110 tests 通过；NLM 输出已存 `.iteration/notebooklm-response-20260802.json` 并通过 `notebook_gate.py validate-output`。
- KKP、可见代码高亮与真实 PTY 搜索/复制仍属候选/证据门控项；不以 NLM 引文、未经测量性能或远端深研状态作本仓库事实。

## Explicit render demand boundary（2026-08-02）

- 手动导入报告指出：状态变化与动画需求应分别驱动刷新；本地 CodeGraph 回核确认 `dirty` 与 token wake 的 256 chunk 批量排空原已存在，真实缺口是 `should_draw` 直接把 `Ui.busy` 当作渲染条件。
- `should_draw` 现只消费 `dirty` 与显式 `animation_due`；主循环 tick 在 busy、无 pending task/modal 时登记动画帧，绘制后清除两类需求。spinner 节奏、事件触发、Answer/Reasoning、工具、输入、审批、安全与 native scrollback 语义不变。
- `draw_only_when_dirty_or_animation` 与 workspace tests（99/110 等）、clippy、build、fmt/diff、ridgecode smoke 均通过；未将报告中的 CPU/单核收益写成 RidgeCode 测量事实。

## Tool History 详情命中定位（2026-08-02）

- NLM 建议“折叠内容可检索且可导航”；CodeGraph 回核确认基础已存在：`tool_history_panel` 将详情放入 `PanelRow.value`，`panel_filter` 已按 key/value 过滤，但列表只显示摘要，命中详情仍需手动 Enter 才可见。
- `Panel::retype` 现对 Tool History 优先选中首个详情命中并自动展开；`detail_match_scroll` 按换行、cell 宽与详情可见行数计算有界偏移，使命中行进入视口。无命中、摘要命中、空查询与手动折叠语义不伪造工具状态，静态 scrollback 不变。
- `tool_history_search_opens_and_positions_detail` 加上 40×12 TestBackend 验收；workspace tests（99/112 等）、clippy、build、fmt/diff、ridgecode smoke 通过。未引入语法高亮依赖，未把报告性能数字当本地事实。
- 对 NLM 的窄端 telemetry 候选做了确定性审计：40/32/18 列仍保留底部 `ctx`、token 数值与 `tok` 单位，故不重复改 `top_chrome`；可见行语法高亮仍受依赖与性能证据门控。

## ReRelease 稳定基线与归档验收（2026-08-02）

- 本轮 TUI 切片完成稳定基线复核：workspace tests（agent 99、TUI 113 及其余 crate）、workspace build、clippy -D warnings、fmt check 均 exit 0；release profile 亦已构建成功。
- scripts/dist.ps1 已实际生成 dist/ridgecode-x86_64-pc-windows-msvc.zip 与 SHA-256 文件；归档内容核验为 ridgecode.exe、README.md、install.ps1，README 已随包进入。
- release 二进制 --help、--version、无 key 离线 demo 均 exit 0。GitHub 远端发布尚未执行；需先把稳定基线与 README 纳入明确提交，再创建版本标签。

## Visible-only code token roles（2026-08-02）

- NLM 建议内容级代码语义增强；CodeGraph 回核确认现有 Live Answer 只在 fenced code 中统一使用 Muted，语言 badge 不等于内容高亮。未引入 tree-sitter/syntect 或其他依赖。
- `code_line_spans` 仅处理进入 `live_markdown_line` 的可见 fenced 行，有限识别通用关键字、类型、字面量、字符串、数字与注释；未知 token 保持 Muted，跨行语法不猜测。Answer/Reasoning 文本、围栏状态、静态 scrollback、工具、键位与布局不变。
- `live_code_tokens_are_semantic_and_visible_only` 覆盖 ANSI16 角色与宽度裁切；workspace tests（99/113 等）、clippy、build、fmt/diff、ridgecode smoke 通过。该切片不宣称报告中的性能数字，PTY/native scrollback 仍只待物理证据。
## v0.5.0 ReRelease 发布核验（2026-08-02）

- 用户以“执行”批准 `REQ-20260802-02`；需求 intake、Active/Pending 结构与状态快照校验通过。`iteration_gate` 仍报告既有流程文件、`samples/config.json` 与 `test_codegraph.ps1` 越界；这些路径未纳入发布提交。
- 版本锁定后重新通过 `cargo fmt --all -- --check`、`cargo test --workspace --locked --offline --quiet -- --test-threads=1`（agent 99、TUI 113 及其余 workspace 套件）、`cargo clippy --workspace --all-targets --locked --offline --quiet -- -D warnings` 与 `cargo build --workspace --locked --offline --quiet`。
- `scripts/dist.ps1` 生成 `dist/ridgecode-x86_64-pc-windows-msvc.zip`；归档字节核验包含 `ridgecode.exe`、`README.md`、`install.ps1`，且归档 README 与仓库 README 字节一致。SHA-256：`63e6bb0d9dffefbaf162513677bb5d78d7fa03ee259c6c94e03c402b0f867b1d`。
- 发布二进制 `--version` 输出 `ridgecode 0.5.0`，`--help` 与无密钥离线 demo 均 exit 0。GitHub main/tag/Release 尚待提交、推送与 Actions 归档核验。
## Answer 尾部增量围栏缓存实验（2026-08-02）
- NLM 既有对话重试仍返回 `UNAUTHENTICATED`；`refresh_auth` 判定磁盘凭据 `stale`，`nlm login --check` 亦因 `network_error` 失败。故本轮不采纳未取得的 NLM 计划，不查询用户禁用的研究状态；理论依据仍唯一为手动源 `8862d715-ffad-4288-b7eb-173260e2dcff`。
- 依据 CodeGraph/只读审计，落地最小可逆性能实验：`LiveBlock::Answer` 改为维护有界文本与 fenced-code 起点缓存；`LiveTranscript::visible_lines` 仅反向取得视口 `max_rows`，并按缓存奇偶恢复 `fence_before`。正常追加只处理末行与新增片段，超过 32K 上限才重建缓存；Answer/Reasoning、工具折叠、键义、ANSI、输入、审批、取消与 native scrollback 不变。
- 新增 `answer_fence_cache_handles_split_markers_and_closing_fence`、`answer_tail_ranges_only_keep_requested_viewport_rows`；fmt、workspace tests（agent 99、TUI 115 及其余套件）、clippy `-D warnings`、workspace build、`git diff --check` 均通过。
- 当前仍缺真实 Windows PTY/native scrollback 的物理证据；本实验不宣称 wall-clock 或行业性能数字，下一闸为实际终端验证。
## 异常 ANSI 行边界恢复切片（2026-08-02）
- 既有 NotebookLM 对话查询与当前 notebook note 列表均因实际凭据过期失败（`UNAUTHENTICATED`）；未调用用户禁用的 `research_status`，理论依据仍唯一为手动源 `8862d715-ffad-4288-b7eb-173260e2dcff`。
- `sanitize_display_text` 的 CSI/OSC 跳过器现于缺失终止符时在换行恢复，把换行交还外层净化器，避免异常模型输出吞掉后续 Answer/Reasoning 行；完整 ANSI 序列语义不变。
- 新增 `malformed_escape_sequences_recover_at_line_boundaries`，workspace tests（agent 99、TUI 116 及其余套件）、fmt、clippy `-D warnings`、workspace build、`git diff --check` 均通过。

## TUI 事件分流与 Reasoning 截断提示（2026-08-02）

- 终端事件现先经 `terminal_event_action` 分流：粘贴独立净化并原子插入，Key 才进入去重/输入状态机，Resize、Focus、Mouse 等事件仅触发重绘，不会误落入输入编辑。
- `LiveLine` 增加纯渲染元数据 `continuation_before`；Reasoning 尾部因视口或预算裁剪时，首条可见行复用一格 `┊` rail 表明前方仍有内容，不新增占位行，不改变 Answer/Tool 顺序与 Ctrl+R 语义。
- `terminal_event_router_separates_paste_and_resize`、`reasoning_tail_marks_hidden_prefix_without_extra_row` 与长 Reasoning 窄预算回归通过；当前 TUI 二进制测试 118 项通过。NLM 状态查询仍按用户要求停止，理论依据仍唯一为手动导入源 `8862d715-ffad-4288-b7eb-173260e2dcff`；真实 Windows PTY/native scrollback 仍待物理验收。

## 静态历史语义 rail（2026-08-02）

- CodeGraph 回核发现：Live Answer/Reasoning/Tool 已有 rail 与 marker，但 `Viewport::Inline` 静态历史仅保留颜色和文本，工具摘要/详情缺少可视类型连续性。
- `flush_commits` 现仅在展示层为静态 Reasoning 加 `┊`、为 Tool summary 加 `◈`、详情加 `┆`；原始文本、Markdown、折叠状态、`insert_before` 原生 scrollback 与 Tool History 均不变。
- `reasoning_commit_renders_in_inline_scrollback`、静态工具折叠/展开回归与 workspace 质量闸覆盖这些 rail；未宣称真实 Windows PTY 搜索/复制或帧延迟证据。

## 窄宽工具快捷键可发现性（2026-08-02）

- 审计发现：15 列以下 live 工具态只显示队列深度，`Ctrl+O` 折叠/展开入口被裁掉；这使“工具默认折叠、按需展开”在窄终端不可发现。
- `input_chrome` 现于 14–17 列保留紧凑 `^O`，18 列同时保留 `^O` 与真实 Reasoning 的 `^R`；idle 的 live 工具与 Tool History 同样保留入口。仅改变提示文本，不改变键路、工具摘要/详情上限、状态或布局槽位。
- `input_chrome_exposes_submit_or_queue_mode` 增加 15/18 列工具、Reasoning、History 组合回归；workspace tests（agent 99、TUI 118 及其余套件）、fmt、clippy `-D warnings`、workspace build 均通过。

## ReRelease 与 NLM 边界复核（2026-08-02）

- 当前主分支为 `022f421`，已推送 `origin/main`；v0.5.0 tag 未重写。Windows Release 资产已按当前包替换，远端 ZIP 摘要与本地一致：`sha256:8782dfd833e86b827a09803364c86a3c93070e955d2c9a94743a8fb84479629a`。
- 当前归档 `ridgecode-x86_64-pc-windows-msvc.zip` 含 `ridgecode.exe`、`README.md`、`install.ps1`；README 与仓库版本一致。该包供用户实际验证本轮窄宽快捷键。
- 本轮向既有 NLM 对话、限定手动导入源的计划查询返回 `UNAUTHENTICATED`；未查询用户禁用的深研状态，未删除或修改远端 note，理论依据仍唯一为手动源 `8862d715-ffad-4288-b7eb-173260e2dcff`。

## ConPTY 与真实 Windows 控制台验收边界（2026-08-02）

- 初版探针曾过早关闭 `CreatePseudoConsole` 句柄、并错误传递 HPCON 指针，导致 `0xC0000142`；按 Microsoft ConPTY 生命周期/属性契约修正后，`ridgecode` 进程退出码为 `0`，`ResizePseudoConsole(18×8)` 成功，故早期错误不归因于生产代码。
- 修正版 ConPTY 探针仍把 `cmd.exe /c echo PTY_OK` 与 `ridgecode --version` 文本落到宿主输出，故不把它当作 TUI 证据；原因是探针标准句柄映射未闭合，生产代码未改。
- 另以独立可见 Windows PowerShell 控制台启动真实 `ridgecode`：屏幕读取确认 `inline mode`、启动 banner、`RidgeCode ready`、输入框与状态栏；注入 `/help` 并按 Enter 后，帮助命令完整呈现；窗口由 `120×50` 缩至 `48×12` 后进程仍存活、内容有界且无越界；恢复尺寸后以 `/exit` 退出。故真实终端帧、输入、窄窗重绘、退出已取证。
- 尚未以人工鼠标/键盘完成终端原生搜索/复制验收，也未证明 `native scrollback` 的跨终端行为；因此不宣称该项已闭环。其余证据为 TestBackend/纯逻辑回归；若发布新 ReRelease，应把搜索/复制作为用户实际验证项并保留回滚路径。

## 工具摘要显示语言收口（2026-08-02）
- CodeGraph 审计发现 `tui::eventfmt::summarize_event` 的工具投影仍混用中文标签，与 iter-36「用户可见串全英文」契约不一致。
- 本轮仅翻译显示层标签：`Read`、`Write`、`Edit`、`Search`、`Batch edit`、完成/截断/trace 提示；工具名、参数、详情数据、颜色、折叠边界与错误判定均未改。
- `summarize_event_overviews_tools` 及 workspace 全量质量闸通过：测试 99/118 等套件全绿，`clippy -D warnings`、`cargo build`、`git diff --check` 均通过。
- 发布链路一并修正：`scripts/dist.ps1` 现在捕获 Cargo 正常 stderr，仅按真实退出码判定失败；最新归档含 `ridgecode.exe`、`README.md`、`install.ps1`，SHA-256 为 `9a2846cba8aef66b453f36e88443b711134a6f4dc54c3b6111612a44fafd56fa`。
- NLM 已用隔离 Chrome/CDP 写入 `default` profile；`nlm login --check` 通过（20 notebooks）。既有会话查询已由 `UNAUTHENTICATED` 变为 `RESOURCE_EXHAUSTED`，判定为配额/限流；未查询研究状态，未修改远端 note，下一步仍以手动导入源 `8862d715-ffad-4288-b7eb-173260e2dcff` 为理论依据。

## Tool History 长行命中定位（2026-08-02）
- CodeGraph 回核发现 `detail_match_scroll` 只按逻辑行定位；详情长行在窄面板换行后，命中词可能仍落在不可见的后半段。
- `detail_match_scroll` 现按既有 `char_cells`/`wrapped_rows` 口径计算命中所在物理行，再居中滚动；仅改 Tool History 呈现定位，不改搜索、折叠状态、键位、工具数据或静态 scrollback。
- 回归覆盖长 ASCII 行与 CJK 双宽字符；workspace tests（99/118 等套件）、`clippy -D warnings`、`cargo fmt --check`、`cargo build` 均通过。

## TUI 重试提示显示语言收口（2026-08-02）
- 重试中的 transient failure、不可重试停止与达到上限提示改为英文，统一 TUI 用户可见语义。
- 仅改提示文本；重试分类、`MAX_RETRIES`、错误传播与 provider 行为均未改变。事件分类所用的错误关键词仍保留原逻辑。
- `cargo test --workspace --locked --offline`、`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`、`cargo build --workspace --locked --offline`、fmt 与 `git diff --check` 均通过。

## v0.5.0 ReRelease 当前资产（2026-08-02）
- `3213a09` 已推送 `origin/main`；Windows 包按该提交重建并覆盖 GitHub Release `v0.5.0`。
- `ridgecode-x86_64-pc-windows-msvc.zip` SHA-256 为 `ee1f12dc30237d100c3d175a558f54309a86b2b86f12f252d14fb1f61ed38db5`，远端 digest、大小 `3009866` 与本地一致；`.zip.sha256` 旁车文件亦已同步。
- 归档含 `ridgecode.exe`、`README.md`、`install.ps1`；README 随包发布，覆盖安装、配置、TUI、命令与故障排查用法。
- Release 二进制 `--version`、`--help` 与无参数离线 smoke 均 exit 0；smoke 仅报告本机未安装 `codegraph-mcp`，NotebookLM MCP 仍成功连接。

## Live Answer/Reasoning Follow/Inspect（2026-08-02）
- `LiveTranscript` 现默认跟随最新尾部；`Alt+PageUp` 向较早内容检视，`Alt+PageDown` 向较新内容移动，`Alt+End` 回到 Follow。检视状态在顶栏显示，工具详情滚动仍优先消费同组快捷键。
- 检视采用有界偏移（最多 512 行），不复制完整 transcript；追加同一 Answer 时保留检视位置，新 Answer、清空流与 splash 重置为 Follow。
- Tool History 长详情支持 `Alt+PageUp/PageDown` 绕搜索命中手动微调；Enter/方向键/普通 Page 键仍负责面板选择与翻页。
- `4aa40f8` 已推送 `origin/main`；agent 99、TUI 120 及 workspace 测试、`clippy -D warnings`、fmt、diff 检查均通过。
- ReRelease `v0.5.0` 已覆盖：`ridgecode-x86_64-pc-windows-msvc.zip` 大小 `3011518`，SHA-256 为 `f14ab5c378f70f342103367e84dd13032066ef554677ada0aa5b8085260dbb6b`；归档含 `ridgecode.exe`、`README.md`、`install.ps1`，旁车 SHA 文件已同步。
- 本轮不查询用户指定停止查询的 NLM 深研状态；理论依据仍唯一为手动导入源 `8862d715-ffad-4288-b7eb-173260e2dcff`。真实 Windows 原生 scrollback 搜索/复制仍待用户实机验收。

## Windows Terminal 实机验收清单（2026-08-02）
- README 新增发布包验收步骤：inline 历史、Answer/工具的原生选择复制、英文/CJK/emoji 搜索、Live 检视快捷键、`48×12` 窄窗与 `/exit`。
- `8fb4223` 已推送 `origin/main`；README 本地与远端 blob 一致。ReRelease `v0.5.0` 已重新覆盖，ZIP 大小 `3012000`，SHA-256 为 `0e41668b08b670b809444e18035165102410da645ea08cb5788119197a32e877`，归档仍含 `ridgecode.exe`、`README.md`、`install.ps1`。
- 清单是可复现实验步骤，不冒充已取得的物理证据；原生 scrollback 搜索/复制仍须用户在实际终端执行并记录终端版本、尺寸与失败文本。

## 隔离 Release TUI 启动取证（2026-08-02）
- 在独立 Ridge pane 启动 `target/release/ridgecode.exe`，实际捕获 `RidgeCode ready`、inline 输入框、`Zai · glm-4.6 · ctx 0% · 0 tok` 状态栏；启动日志确认 NotebookLM MCP 连接，已知 `codegraph-mcp` 缺失警告仍存在。
- 该次桥接输入未形成可靠的 `/help` 提交事件，故不把它算作命令交互、原生搜索或复制证据；测试进程已按已核验路径精确停止。真实终端原生 scrollback 仍待人工验收。

## 静态 Answer 代码语义色一致性（2026-08-02）
- `md_line_spans` 现复用既有无依赖 `code_line_spans`：Live 可见 fenced code 与已落入 inline 原生历史的 Answer 均对关键字、类型、字符串、数字、字面量、注释做 bounded ANSI16 语义色；未知 token 与跨行语法仍保持 Muted/不猜测。
- 文本、围栏状态、折叠、工具协议、键位与 `insert_before` scrollback 语义不变；新增静态历史颜色回归，agent 99、TUI 121 及 workspace 测试、clippy、fmt、diff 检查通过。
- `c01d526` 已推送 `origin/main`；v0.5.0 ReRelease 已覆盖，ZIP 大小 `3011947`，SHA-256 为 `b8bbb398e6e6bcf1f22b276c6b7862c17b7ff39c02584ac3bbdf761a2107eaed`，归档含 `ridgecode.exe`、`README.md`、`install.ps1`。

## Open Vision 七切片落地（2026-08-11）

- `REQ-20260810-OPEN-VISION-01` 已批准并保持不变；七类方向登记于 `docs/OPEN-VISION-SLICES.md`。
- `crates/agent/src/open_vision.rs` 提供密钥绑定安全审计、治理元数据、Unicode 能力探测/安全回退、图谱推理投影、离线 store-and-forward/federated outbox、有界 Web/PWA 事件流/接管、推理树哈希链离线审计；所有边界均有容量、身份、父节点或载荷校验。
- `communication.rs` 将治理元数据随 `AgentEnvelope` 传输，并在 exchange 前执行 `authorize_governance`；既有 HMAC、时钟窗口、nonce replay、只读能力协商仍为硬门。
- 本轮统一 `scripts/quality-gate.ps1` exit 0：workspace tests（agent lib 151、ridgecode 357、其余套件全绿）、fmt、clippy `-D warnings`、build、`cargo llvm-cov --fail-under-lines 80` 与本机 Sonar quality gate 全通过；Sonar API 复核 coverage `85.2%`、new coverage `80.0%`、bugs/vulnerabilities/code_smells/issues `0`、duplication `0.9%`。无密钥 `ridgecode --version` 与离线 demo smoke exit 0；requirements/preflight/iteration gate 均通过。
