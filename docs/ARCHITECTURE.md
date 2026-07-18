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
- **`invoke_best_of`**(iter-24,Best-of-N 投机分支):`Arc<Self>` + JoinSet 并发 N 份初始状态各跑一遍图;失败分支丢弃,调用方评分器 `Fn(&S)->i64` 择优、平分低索引确定性胜;空/全败 → `GraphError::NoWinner`。**边界**:分支间无副作用隔离(并发真实写会互踩),真实 agent 接入需先做每分支工作区隔离;agent 侧已备确定性评分器 `branch_score`(approved 压倒一切、同侪省 token 胜),未接 CLI 主流程。
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

### 2.7 分支工作区隔离(`workspace.rs`,iter-25)

BoN 真实接入的物理前提(引擎零感知):`Workspace`(GitWorktree / ShadowCopy)。`create_isolated` —— git 仓库先试 `git worktree add --detach`(best-effort),败/非 git 回落影子拷贝(跳过 `.git`/`target`/`.ridge`/`node_modules`);`merge_winner(main, branch, modified_files)` 胜者整文搬运合回(自动建嵌套目录,**全有或全无**回滚);`cleanup` best-effort 清分支。**未接 CLI**:分支 cwd 贯穿 `execute_tool_call` 的接线是下一刀(见 CONTRACT-iteration-25 边界)。

### 2.8 Skills 与项目规则

`SKILL.md` 声明式技能:`RIDGE_SKILLS_DIR` env > config `skills_dir` > `~/.ridge/skills`。cwd 的 `CLAUDE.md`/`AGENTS.md` 经 `load_project_rules` 注入 system prompt。`@file` 引用注入正文(MENTION_CAP=20000 截断)。

## 3. provider:LLM 抽象

- `Role/Message`(tool_calls + tool_call_id 归一化)、`ToolSpec`(name/description/JSON Schema)、`ToolCall`(arguments 已解析成 Value)、`Completion{text, tool_calls, usage}`、`CompletionRequest`。
- **`LlmProvider`** trait:`complete` + `complete_streaming`(SSE 逐 token 回调,默认回落到 complete —— 不支持流式的 provider 零改动)。实现:`AnthropicProvider` / `OpenAiProvider`(HTTP,`HttpClient` 传输接缝可离线测,`StreamAcc` 累积 SSE)/ `ScriptedProvider`(离线按序吐预设 Completion,demo/测试用)。
- **`SwapProvider`**:`Mutex<Arc<dyn LlmProvider>>` 热切换 —— TUI `/model` `/provider use` 换芯不重建图;锁只持到 clone,不跨 await。
- web 工具:`web_search`(GFW 探测自动换引擎、无 key 多引擎 fallback)+ `fetch_url`(抓正文),`WebFetch` 接缝可离线测。
- **`provider::models`**(iter-29):`fetch_models(HttpClient, kind, base_url, key)` 向 `{base_url}/models` 发鉴权 GET(`HttpClient::get_json` —— 对称 `post_json`,默认 Err 仅 `ReqwestClient` 真实现)→ `parse_model_list`(**纯函数**,兼容 OpenAI/OpenRouter/Anthropic 的 `{data:[..]}` + 顶层数组,坏/空 → 空列表;context 多路探测含嵌套 `top_provider.context_length`)→ `Vec<ModelInfo{id, context: Option<u64>}>`。供 TUI `/models` 列实时模型 + 上下文大小。

## 4. tools:std-only 真实工具

`read_file` / `read_file_range`(区间读)/ `edit_file`(唯一匹配替换,0 处或多处即报错引导)/ `apply_edits`(多文件原子批量:全体校验唯一匹配 → 落盘,单写失败回滚已写)/ `write_file` / `search`(递归 + glob,SEARCH_CAP 截断提示)/ 跨平台 shell / `is_dangerous_command` 灾难命令硬拦截(任何模式下不可绕过)。
**写沙箱 jail**(`execute_tool_call` 里 write/edit/apply_edits 路径守卫):`jail = jail_guard(allow_jailbreak(), path)`,`jail_guard` 纯函数 —— 关(默认)时钳在**进程 cwd 子树**,越狱 → `BLOCKED`。**地址越狱开关**(iter-34)= 进程级 `AtomicBool`(`set_allow_jailbreak`/`allow_jailbreak`,启动读 `config.allow_jailbreak`、TUI `/jailbreak [on|off]` 实时切):开则 `jail_guard` 放行 cwd 外写,**但只放宽这一条** —— 危险命令拦截、受保护路径(tests/.git)守卫、只读模式不受影响;开启时 TUI 顶栏红底 `⚠越狱` 徽标警示。默认关,持久化经 `/config set allow_jailbreak true`。

## 5. mcp:客户端协议层

`McpTransport` trait(`request` + `notify`,JSON-RPC 信封内部处理)→ `StdioTransport::spawn` 子进程 / 闭包 FnTransport(测试)。`McpClient`:`initialize` 握手(initialize 请求 + `notifications/initialized` 通知,合 MCP 规范)、`list_tools`、`call_tool`(拼 content text 块)、`namespaced` = `server__tool`。`McpError`(Transport/Rpc/BadResponse)。
agent 侧 `resolve_mcp`:多 client 各自握手 + 列工具 → 归一化 `McpTools{specs, router}`;**降级不崩** —— 单 server 失败跳过。config 支持多 `mcp` 声明 + 兼容旧 env `RIDGE_MCP_CMD`。

## 6. 入口与交互(`main.rs` / `tui.rs`)

- **`main`** 分流:有 `RIDGE_API_KEY`(或 config provider)→ 真实路径;无 → 离线脚本 demo。
- **TUI**(TTY,ratatui):**主屏内联 REPL**(iter-26)—— `Viewport::Inline(LIVE_HEIGHT=14)`,不占备用屏;历史行经 `Ui.commits` 队列 → `flush_commits` → `Terminal::insert_before` **静态提交**进终端原生 scrollback(原生滚动/选取/搜索保留),入历史即永不重绘;Live 视口五槽定长布局(iter-31):顶状态行(provider·model·**ctx%**·tokens·todo·spinner;busy 时徽标转暖色)+ 流式尾巴(`stream_tail` 尾 K 行)+ **忙碌粘条**(仅 busy 有高度:`fmt_busy_bar` = phase·读秒·token·tok/s·todo d/n)+ 动态高度输入框 + **自定义底栏**(`config.status_bar` 模板,`render_status_template` 替 `{provider}{model}{ctx}{tokens}{cwd}`,空则 `DEFAULT_STATUS_BAR`);审批模态覆视口、↑↓ 滚动接 Paragraph 偏移;TODO 变更快照历史化(无侧边栏)。`flush_commits` 每块前置空白行分栏(iter-31 需求 5)。计时/计量纯函数化:`token_rate`/`ctx_percent`/`fmt_busy_bar` 全收数值入参,时钟采样(`task_started: Instant`)与 token 累计(`ui.stream_tokens` 经 `est_tokens`)留主环,测试零 wall-clock;ctx% 分母 `meta.ctx_window`(默认 200K,`/models` 命中当前模型即缓存真实窗口)、分子 history `est_tokens` 和。**视觉与反馈**(iter-28)—— 语义化色角色层 `Role`→`role_color`(ANSI 16 具名色,零 RGB 硬编码,尊重终端主题);`md_line_spans` 行级 md 轻渲染**只在静态提交时染**(围栏/块内/标题/行内 code/bold,未闭合按字面);`fold_lines(20)` 呈现层折叠(留头 + `+N` 尾标);审批 detail 按 `+`/`-` DiffAdd/DiffDel 着色;`splash_frame` 启动 banner 列渐显(tick 驱动,纯函数帧序列;iter-36:`SPLASH_TICKS=14` 更平滑、`indent`+`splash_pad` 居中动画);**落定 banner `splash_block(width)`**(iter-36 修「标识乱了」):宽 ≥ `SPLASH_W(48)` → 居中艺术字(逐行 `trim_end` → 每行 ≤ width,`flush_commits` 的 Wrap 不再折行撕裂)+ 英文 tagline,窄 → 紧凑单行 `◆ RidgeCode`;busy 流尾青色 `█` 呼吸游标。**显示语言(iter-36)**:所有用户可见串(TUI 提示/命令/Panel/状态栏/弹窗、CLI help/日志/phase)一律**英文**;代码注释与 `lib.rs` 模型侧串(system prompt/observation)保留中文(非显示)。**高级输入**(iter-27)—— CSI u best-effort(`DISAMBIGUATE_ESCAPE_CODES`,Drop 时 Pop);`InputState{buffer,cursor,history,hist_idx,draft}` 多行编辑状态机:光标插删/Left/Right/Home/End/多行列钳位/CJK 安全,**首逻辑行 Up=历史召回**(draft 存还),Shift/Alt+Enter/Ctrl+J 换行,真光标 `set_cursor_position` **按 wcwidth 显示列**(iter-30,`cursor_display_col`/`char_cells`/`str_cells`;CJK/emoji 占 2 格,`cursor` 仍字符序,只渲染换算用显示宽 —— 修中文输入光标偏左根因;`wrapped_rows`/浮窗宽同口径);**补全浮窗**:`Popup{items,selected,anchor}` 纯文本补全 —— 打 `/`/`@` **即弹随打随滤**(iter-35:`Insert`/`Backspace` 后重算 `build_popup`,余字符 →None 自然关;Tab 仍显式触发),行首 `/` 补命令(`SLASH_COMMANDS`)、`@` 补路径(单层 read_dir ≤20 有界),Enter 经 `apply_completion` 写回缓冲。**交互页 Panel**(iter-35,取代 iter-32 的 ModelPick 浮窗特例):`Ui.panel: Option<Panel{kind,title,query,rows,view,sel,editing}>`,模态居中覆视口 = 搜索框(随打随滤 `panel_filter` 纯函数)+ 过滤列表(选中高亮)+ 选中动作。五命令开页:`/config`(就地编辑:↑↓ 选键→Enter 编辑→`persist_config`+`apply_config_live` live 应用→刷新)、`/provider`(Enter `switch_provider` 切档)、`/tools`(只读)、`/models`\|`/model pick`(Enter `swap_model`+缓存 ctx_window)、`/agent`(只读);`panel_action` 纯路由,`panel_enter` 分派(先 clone 选中项释放借用再 mut)。键位模态优先级 = 审批 > **Panel** > 浮窗 > 输入。`swap_model`/`switch_provider` 单点收敛热切换(密钥 `current_api_key` env 优先/回落 config 内联),文本命令与页共用。**输入排队**(iter-33)—— busy 时 Enter → `InputAction::Queue` 入 `Ui.queued: VecDeque`(空闲 → `Submit`);起任务/跑命令逻辑收敛为**主环顶唯一提交点**(消费 `pending_submit`,键入提交与 `done` 后 `queued.pop_front()` 接跑共用),消除重复 spawn 码;中断 `clear` 队列(abort=取消全部待跑);忙碌粘条 `⏳N` 示待跑深度。**事件驱动主环**(iter-23)—— 阻塞读线程转发键盘事件入 tokio mpsc,`tokio::select!` 六路复用,dirty 标记空闲零重绘;纯决策函数 `approval_action` / `should_draw` / `wrapped_rows`(`input_height`+`commit_height` 共用)。`TuiApprover`(请求 tokio unbounded,应答 std sync_channel);**Bracketed Paste**(iter-24,best-effort + `sanitize_paste`,粘贴并入 InputState 光标事务)与**动态输入高度**(3..=8 行);斜杠命令 `/help /cost /model[ <name>| pick] /models /provider[list|use|add] /agent /config[ set] /jailbreak[ on|off] /reset /compact /tools /exit` **只在 TUI**(多数列表/配置类开交互 Panel,见上)(`run_command` 为 `async` —— `/models` 内 `fetch_models` 裹 15s `timeout` 防挂);每轮 `save_session` 落盘。
- **headless**(非 TTY:管道/CI/重定向):逐行 stdin 当任务串行跑,跨行携带 history,恒 `AutoApprove`(危险命令仍硬拦截)。
- **`run_once`**(CLI 带任务):一次性;`--every <dur>` = 时间触发器(常驻,每轮重载信号、单轮出错不掀翻循环)。
- CLI:`--cwd` / `--yolo`(skip_danger)/ `--resume`(kill-9 恢复会话)/ `--read-only` / `--every`。
- 每轮落 `.ridge/runs/<id>/`(manifest.json + trace.json)审计;`trace_and_report` 打终态 + `halt_reason`。

## 7. config(`~/.ridge/config.json`,env 覆盖)

providers 命名档(kind/model/base_url/**key_env** —— 密钥只走 env 绝不落盘)/ budget_tokens / skip_danger / 多 `mcp{name,cmd,args}` / skills_dir / **`status_bar`**(iter-31 自定义底栏模板,占位 `{provider}{model}{ctx}{tokens}{cwd}`,空用内置默认)。`RIDGE_CONFIG` 指路径;TUI `/config set` 可持久化回写;TUI `/provider add <name> <kind> <model> <base_url> [key_env]` 经 `parse_provider_add` + `config_add_provider` 增/同名覆盖档(`api_key` skip_serializing,明文永不落盘)。

## 8. 设计不变量(改码前须知)

1. **maker ≠ checker**:verify 独立、只认确定性信号。
2. **reducer 显式**:新状态字段必须进 Patch + apply,不得绕过。
3. **引擎零 LLM**:langgraph 不依赖 provider;预算/成本等 app 概念不进 GraphError。
4. **外置能力走 MCP/SKILL 不进内核**:squeez/RAG/AST/tiktoken 等一律外置。
5. **provider 边界**:第三方 SDK 包在 trait 后。
6. **有界注入**:一切注入块(观测/信号/事实/@file)皆有上限截断。
7. **危险命令拦截不可绕过**;sub-agent 恒只读。
8. **有序稳态**:注入块用 BTreeSet/排序,字节稳定利 prompt 缓存。
