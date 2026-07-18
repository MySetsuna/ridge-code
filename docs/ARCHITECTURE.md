# RidgeCode 已落地架构详情

> 本文由 codegraph 全量读码生成,是**当前代码现实**的忠实快照(非愿景)。用途:①新人/新会话上手;②作为 NotebookLM 来源,供规划下一迭代时对照「现状」。行号会漂移,故只引 `文件 + 符号名`。

## 0. 总览

单二进制 `ridgecode`(住 `crates/agent`,package 名 `agent`)。Rust workspace,6 crate,edition 2021,v0.2.0:

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

- `Role/Message`(tool_calls + tool_call_id 归一化)、`ToolSpec`(name/description/JSON Schema)、`ToolCall`(arguments 已解析成 Value)、`Completion{text, tool_calls, usage}`、`CompletionRequest`。
- **`LlmProvider`** trait:`complete` + `complete_streaming`(SSE 逐 token 回调,默认回落到 complete —— 不支持流式的 provider 零改动)。实现:`AnthropicProvider`(`new` = `x-api-key` 档;**`new_oauth` = OAuth 订阅 bearer 档**,iter-43:`Authorization: Bearer` + `anthropic-beta` + system 首块注入 Claude Code 身份)/ `OpenAiProvider`(HTTP,`HttpClient` 传输接缝可离线测,`StreamAcc` 累积 SSE)/ `ScriptedProvider`(离线按序吐预设 Completion,demo/测试用)。
- **`SwapProvider`**:`Mutex<Arc<dyn LlmProvider>>` 热切换 —— TUI `/model` `/provider use` 换芯不重建图;锁只持到 clone,不跨 await。
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
