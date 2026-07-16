# 全局工作日志(append-only)

跨迭代的长期记忆。开工前读末尾 5–10 条;完成大块工作后追加一条。最新在最上面。

---

## 2026-07-16 · token 节约之路 iter-4 + 愿景收束:状态快照编译器(Durable State)

- **做了什么**:第一阶段 Runtime State **真正补全** —— `AgentState` 加 `modified_files: BTreeSet<String>`(有序稳态,利缓存)+ `last_error: Option<String>`;`durable_updates` 在 act 节点确定性回填(写类工具成功→记文件清错;工具错误→置 last_error);`durable_state_block` 编事实块注入 messages **末尾**(role=system,首部 system prompt 冻结利缓存),仅有事实时注入,体量 O(去重文件数),不随步数膨胀。
- **对抗评审驳回**:①NotebookLM 荐 `HashSet` → 改 `BTreeSet`(缓存要确定性有序);②驳回 `environment_context`/`cd` 追踪(ridgecode 每次 `run_shell` 全新 `sh -c`,cd 不跨调用持久,追踪是错的)。
- **验收**:`cargo test --workspace`=**全绿**(+3 测),clippy/fmt 干净。`durable_state_block_stays_bounded_over_steps`:改同两文件 50 步事实块字符恒定(O(1))。
- **🎉 愿景收束**:NotebookLM 终审「**是**」—— 内核侧 token 节约愿景**已完成**。4 判据全绿:①历史有界(iter-1/2)②静态底噪极小(iter-3)③输出端 Lean(iter-3)④事实驱动(iter-4)。余项(RAG/squeez/AST(syn)/tiktoken)归**外置 MCP**,动态工具加载/模型路由**附条件推迟**。存证见 `docs/iterations/VISION-token-runtime-state-COMPLETE.md`。

## 2026-07-16 · token 节约之路 iter-3:静态底噪清理(极简 Schema + Lean-output)

- **做了什么**:转向「静态底噪」(工具 Schema + system prompt 每轮都发,是 token 起步价)。①审计 `builtin_tool_specs`,裁 `web_search`(去 GFW/选引擎机制)、`todo_write`(去「像 Claude Code」+ schema 重复的 status 枚举)、`apply_edits`(去尾冗)——只精简文案,不改 name/schema/语义;②`BASE_SYSTEM` 加 Lean-output 指令(简洁作答 + 只出最小 diff,不整文件重写)。
- **验收**:`cargo test --workspace`=**全绿**(+2 测:`tool_descriptions_stay_terse` 每 desc <120 字守不回潮、`base_system_has_lean_output_directive`),clippy/fmt 干净。
- **下一步**:iter-4(状态快照编译器)—— 本轮紧接完成,依据即 iter-3 报告的开放问题 + NotebookLM 指导,未单列 contract。

## 2026-07-16 · token 节约之路 iter-2:加权字符压缩触发器

- **做了什么**:iter-1 的自动压缩触发判据由「history 条数(24)」改为「按内容体量的加权字符估算」——新增 `est_tokens`(CJK≈1 tok/字、ASCII≈1 tok/4 字符,口径同 `bin/token-count.mjs`,不引 tiktoken),`to_messages` 触发器改为 `history 各条 est_tokens 之和 > AUTO_COMPACT_TOKENS(6000)`。一条万字日志 ≫ 二十条短问答,条数触发会漏。
- **诚实边界**:加权触发改善「多条中等消息」判准;「少数超大单条消息」下 `compact_history` 按条数裁减不动,需单条内容截断(外置 squeez 域)。测试如实只验多条重消息触发。
- **验收**:`cargo test --workspace`=**92 全绿**(+1 净),clippy/fmt 干净。新测:`est_tokens_weights_cjk_heavier_than_ascii`(4:1)、`to_messages_auto_compacts_when_history_heavy`(重触发/轻不误伤)。
- **NotebookLM 予「剩余纯内核项」全表 + 愿景收敛**(经对抗评审):内核待办 ≈ iter-3(极简 Schema 审计 + Lean-output 指令)+ iter-4(状态快照编译器/Durable State);动态工具加载**推迟**(现仅 ~9 工具,YAGNI)、输出人格注入**判为已被 CLAUDE.md 注入覆盖**、置信度路由**推迟**(SwapProvider/FastContext 已具骨架);RAG/squeez/AST(syn)/tiktoken 归**外置走 MCP**。
- **下一步**:`docs/iterations/CONTRACT-token-iter-3.md`。

## 2026-07-16 · token 节约之路 iter-1:Runtime State 首刀 —— 自动上下文压缩

- **轨道**:新开「token 节约之路」,由 NotebookLM 笔记本「token节省之道」驱动;主源《rust agent开发下一步的token 节约之路》(为 langgraph-rust 量身,四阶段路线图,荐先做第一阶段 Runtime State)。
- **做了什么**:`to_messages` 增**自动压缩** —— history 超阈值(24 条)发 LLM 前先 `compact_history` 成有界快照(原任务+摘要+近 8 条),把 O(n) 历史收敛为 O(1);此前压缩仅 `/compact` 手动。顺手给 `compact_history` 加 **API 正确性护栏**:裁窗口首端悬空 `role=tool`(防 OpenAI 端点 400),手动/自动两路皆受益。**复用既有 `compact_history`,零新依赖、未改图/reducer**。
- **验收**:`cargo test --workspace`=**91 全绿**(+2 新测),clippy/fmt 干净。
- **对抗评审(驳回 NotebookLM 部分建议)**:①驳回 `tiktoken-rs` 依赖(内核走 std/按量估算即可);②**回退**了初版「把 squeez 终端去噪重实现进内核」——squeez 是外置可装工具,应走 MCP/hook,不塞内核(用户明确约束:外置工具不进内核);③RAG 采纳「按需检索」思想但驳回「向量库编进内核」(Qdrant/LanceDB 走 MCP)。
- **下一步**:见 `docs/iterations/CONTRACT-token-iter-2.md`。NotebookLM 荐 P0=压缩触发器改「加权字符数」(替代粗放的条数);P1 动态工具加载被本地评审**推迟**(现仅 ~9 工具,YAGNI);P2 状态快照编译器**推迟**(需先加 durable 字段,是独立大迭代)。
- **报告/指导归档**:`docs/iterations/2026-07-16-iteration-token-runtime-state.md`(已上传笔记本);指导见 CONTRACT。
- **熔断/驳回记录**:回退 squeez 内核集成 1 次(违「内核精简」);驳回 NotebookLM 建议 3 项(tiktoken 依赖、内核向量库、过早的动态工具加载)。

## 2026-07-15 · 多 provider 交互式管理 + /cost + 状态行 + /models 误伤修复

- **遗留修复**:REPL 斜杠命令用 `starts_with("/model")` 会把 `/models`(想列模型)误当「切到模型 s」。改成精确匹配或带空格参数(`input=="/model" || starts_with("/model ")`),`/config` 同修。
- **多 provider(用户要求「支持多 provider 且可交互式添加」)**:config.json 新增 `providers` 数组(命名档案:`kind`+`model`+`base_url`+`key_env`)。REPL `/provider list|add|use`:list 列档案(★=当前,⚠=key_env 未设);add 交互式加并落盘(纯变换 `agent::config_add_provider` 同名 upsert、保留其余键,可单测);use **热切换**到档案(复用 SwapProvider,不重建图)。**密钥永不进 config** —— 档案只存 `key_env`(env 变量名,默认 RIDGE_API_KEY),`use` 前 env 未设则拒绝切换。
- **`/cost`**:REPL 手动累计每轮 `out.total_tokens` → 展示会话累计 tokens + 轮数 + 单任务预算(单 AgentState 只记单任务,故跨轮手动累计)。
- **状态行(回应「about statusline?」)**:每个提示符前打一行灰色 `provider·model · Nk tok · 目录名`,`is_terminal` 门控(管道/重定向不打)。为此给 `Color` 加 `BrightBlack`(ANSI 90)。
- **验收**:单测 `config_add_provider_appends_and_upserts`;REPL 实测 add→list(★/⚠)→use(key 未设拒绝、已设切换)→/cost 全通过。`cargo test`=**84 全绿**,clippy/fmt 干净。README/samples 同步。

## 2026-07-15 · /model 热切换(像 Claude 的 /model,不重建图)

- **REPL 内即时换模型**:`/model <name>` 当场切换本会话模型,**无需重启、不重建 agent 图**。做法:新增 `provider::SwapProvider` —— 一个把底层 `Arc<dyn LlmProvider>` 藏在 `Mutex` 后的装饰器,`complete`/`complete_streaming` 委托给当前底层,`swap()` 换芯。图只见到这一个 `Arc<dyn LlmProvider>`,所以换芯即换模型,`mcp`/`skills` 不必重建。
- `/model`(无参)仍看当前 provider/model/base_url;`/model <name>` 只换 model(沿用当前 provider/base_url/key),**本会话生效、不落盘**(持久化仍走 `/config set model`)。抽 `make_provider` 给启动装配与热切换共用。
- **护栏**:持锁只到 clone、不跨 await 持 std Mutex;密钥只从 `RIDGE_API_KEY` env 取、绝不打印。
- **验收**:provider 单测 `swap_provider_hot_switches_inner`(两个 ScriptedProvider,swap 前后 complete 走不同底层);REPL 实测 `/model`→`/model glm-4.6`→`/model` 显示即时更新。`cargo test`=**83 全绿**,clippy/fmt 干净。

## 2026-07-15 · 配置改 JSON + 交互中 /config 持久化(用户要求)

- **配置格式 TOML→JSON**:用户要求用 JSON 做配置文件。`Config::parse` 改 `serde_json`;路径 `~/.ridge/config.toml`→`config.json`(env `RIDGE_CONFIG` 仍覆盖)。删 `toml`/`toml_edit` 依赖(JSON 无注释、`serde_json` 已在,零新增依赖)。样例 `samples/config.toml`→`config.json`(注释挪进 samples/README 的键表)。
- **REPL 内 `/config`**:无参→看文件路径+当前生效值+可设键;`/config set <key> <value>`→持久化标量键(白名单 provider/model/base_url/budget_tokens/skills_dir/skip_danger,类型归一)到 config.json,**保留其余键(如 mcp)**,改完**重启生效**。密钥仍拒写(只走 `RIDGE_API_KEY` env)。纯文本变换 `agent::config_set` 抽在 lib(可单测),写盘在 main。三种配置方式并存:REPL `/config set`、直接编辑文件、env 覆盖。
- **实测**:`/config` 显示、`/config set model/budget_tokens` 写盘、未知键(api_key)被拒、mcp 数组一路保留、重启读回 —— 全通过。`cargo test`=**82 全绿**,clippy/fmt 干净。README/samples/anysearch 文档同步 JSON。

## 2026-07-15 · Iteration 12 续³:顶层 README 刷新(front door 反映完成态)

- **README 重写**:旧版还叫 `langgraph-rs`/「编码 agent」/32 测试/里程碑停在 M5,严重滞后。新版:标题 **RidgeCode**、北极星(模块化跨领域框架、加 SKILL.md/MCP 不改源码)、**能力全表**(REPL 流式/spinner、批量编辑、权限门+diff+skip-danger、web 研究闭环、@file/--resume/Ctrl-C/todo_write、config.toml/多 MCP/Skills)、快速开始(REPL/一次性/--resume/加技能)、引擎用法、已知限制(沙箱/rmcp)。测试数 32→**81**。校验所有文档链接可达。
- 纯 docs,无源码改动。

## 2026-07-15 · Iteration 12 续²:官方样例技能 +3(含非编程域)

- **CONTRACT-12 P1 样例技能落地**:`samples/skills` 补 triage(分诊排优先级)、summarize(摘要保真去水)、translate(中英互译信达雅)。**summarize/translate 是非编程域** —— 实证「换 `SKILL.md` 就换领域,不改一行 Rust」的框架北极星。单测守住 5 个样例都能被 `load_skills` 解析。`cargo test`=**81 全绿**。提交 `58875e0`。

## 2026-07-15 · Iteration 12 续:斜杠命令 /tools /model

- **CONTRACT-12 P2 斜杠命令落地**:`/tools` 列所有可用工具(内置 + MCP)、`/model` 显示当前 `provider/model/base_url`。抽 `resolve_model_info`(env>config>默认)给装配与 `/model` 共用 → 显示即真在用;`McpTools::tool_names()`;`/help` 补新命令 + 提示 `@path`/Ctrl-C。
- **GLM 实测**:`/tools`→9 个工具(run_shell…todo_write)、`/model`→`openai·glm-4.5-air·bigmodel url`。
- `cargo test`=**81 全绿**,clippy/fmt 干净。提交 `d50eaeb`。
- (发布打磨轨可继续:更多样例技能、标准存储库;框架加固沙箱/rmcp 仍待用户决策。)

## 2026-07-15 · Iteration 11 完成 + Iteration 12 起步(发布打磨)

- **🎉 CONTRACT-11 全部 6 项完成 —— Claude Code 核心用户体验全套达成**:多文件批量原子编辑、token 逐字流式、`--resume` 崩溃恢复、`@file` 引用、Ctrl-C 中断、`todo_write` TODO 清单可视化(GLM 实测多步任务 `[ ]→[~]→[x]` 实时渲染)。叠加此前:彩色 REPL/spinner、权限门+diff+skip-danger、精准+批量编辑、web 研究闭环、MCP/Skills 插件化、config.toml、/compact、trace 审计。写 `2026-07-15-iteration-11.md` 上传 NotebookLM。
- **NotebookLM Iter12 指导 + 对抗评审**:NBLM 荐转「硬化+发布」(P0 Docker 沙箱 / P1 标准存储库 / P2 rmcp / P3 发布打磨)。**驳回 Docker 沙箱当 P0**(gVisor 仅 Linux、当前 Windows、重量级平台相关、**无法离线/自主验收**;危险命令拦截+权限门+diff 已顶 80%)、**再驳 rmcp**(自写已连真实 server,可选升级)。→ 转**安全可自主的发布打磨轨**。归档 guidance-11 + CONTRACT-12。
- **Iteration 12 P0 落地(发布打磨)**:`--help/--version`(读 CARGO_PKG_VERSION)+ **官方样例** `samples/`(skills/researcher + skills/rust-fixer + config.toml 带注释 + README)—— 让「加 SKILL.md/MCP 配置不改源码」有开箱即用范例。单测守住样例可解析。`cargo test`=**81 全绿**。提交 `c1fccda`。
- **交付状态:用户目标(媲美 Claude Code 全部用户体验)已达成。** 剩余沙箱/rmcp/子智能体并行 = 需用户环境/决策的框架轨,标已知限制。

## 2026-07-15 · Iteration 11 续⁴:Ctrl-C 中断当前任务

- **CONTRACT-11 P2 Ctrl-C 落地**:REPL 里 `run_streamed` 与 `tokio::signal::ctrl_c()` `select!` 竞速 —— 任务跑一半按 Ctrl-C → 取消该任务、清 token sender、回提示符(像 Claude Code),不再杀整个会话。tokio 加 `signal` feature。
- **回归验证**:REPL 两轮 piped 均正常完成 + `/exit`(证明 select! 未破坏正常流程)。中断路径为竞速取消(简单;SIGINT 难离线/管道实测,正常路径已验证)。已知:tokio 装 handler 后提示符处 Ctrl-C 被吞,用 `/exit`/Ctrl-D 退(与 Claude Code 拦 Ctrl-C 一致)。
- `cargo test --workspace` = **79 全绿**,clippy/fmt 干净。提交 `685ea97`。
- **CONTRACT-11 仅剩 TODO 清单可视化**(最后一项,拟加 `todo_write` 工具 + REPL 渲染)。

## 2026-07-14 · Iteration 11 续³:@file 上下文引用

- **CONTRACT-11 P2 `@file` 落地**:`expand_mentions`(纯函数)—— 输入里 `@path` 引用**存在的**文件 → 正文注入进消息(模型直接看到、不必自己 read_file);不存在的 `@xxx` 原样留;同路径只注一次、截断 20000 字防爆。REPL + 一次性任务都接。
- **GLM 实测**:`@note.txt 里 RIDGE_SECRET_MARKER 的值` → 模型**不调工具、一步**答出「紫色大象在跳舞」(证明文件正文已注入)。
- `cargo test --workspace` = **79 全绿**(78→79),clippy/fmt 干净。提交 `57db13c`。
- CONTRACT-11 剩:TODO 清单可视化(需把 planner 接进 REPL)+ Ctrl-C 中断(需 tokio signal + 取消语义)—— 均较大/难离线测,下轮再评估。

## 2026-07-14 · Iteration 11 续²:LLM token 逐字流式(SSE)

- **CONTRACT-11 P1 token 流式落地**(最大块):provider 加 `complete_streaming`(默认回落 `complete` 整段 emit,**零风险降级**)、`HttpClient::post_json_stream`(reqwest `resp.chunk()` 增量读、切 SSE `data:` 行、默认报错让降级)、`openai::StreamAcc`+`accumulate_stream`(文本增量拼 + **分片工具调用按 index 拼装** + usage via `include_usage`)。**回调收 owned `String`** 避 async_trait+HRTB 的 `&str` 生命周期坑。
- **CLI 侧**:`TokenBus`(`Arc<Mutex<Option<Sender>>>`)+ `null_token_bus`;`build_llm_agent_full` 接总线,reason 节点 `complete_streaming` 边收边发;`run_streamed` 的 printer 一个 `select!` 里协调 spinner + 逐字 token + 节点事件,流式期间不转 spinner、末尾不重复整段打(抑制 `(final)` 双打)。
- **测试**:假流式 HttpClient(**非 mockito**)喂 SSE 帧 → 逐字拼接==全文、工具调用/usage 正确;降级路径也测。**GLM 实测**:多句问答逐字出、🤖 恰好 1 次(无重复)、approved、usage 到账。
- `cargo test --workspace` = **78 全绿**(76→78),clippy/fmt 干净。提交 `4d45452`。
- CONTRACT-11 剩:TODO 可视化、`@file` 引用、Ctrl-C 中断(均 P2)。

## 2026-07-14 · Iteration 11 续:--resume 会话恢复(kill-9)

- **CONTRACT-11 P1 kill-9 恢复落地**:REPL 每轮把对话 `history` 落盘(`RIDGE_SESSION` 或 `~/.ridge/session.json`,serde_json);`--resume`/`--continue` 启动读回,`/reset` 也落盘清空。像 Claude Code 续接会话。选了**会话级 history 持久化**(用户可见价值 = 对话不丢),而非引擎级 mid-graph resume(那是另一码事,引擎 `FileCheckpointer` 已具备,列后续)。
- **GLM 实测(真·跨进程)**:进程1「记住幸运数字 42」→ 落盘退出;**全新进程 `--resume`** →「已恢复上次会话:2 条消息」→ 问幸运数字答 **42**。
- `cargo test --workspace` = **76 全绿**(75→76),clippy/fmt 干净。提交 `dd6dd9b`。
- CONTRACT-11 剩:token 逐字流式(provider SSE,假流式 HttpClient 测)、TODO 可视化、`@file`/Ctrl-C。

## 2026-07-14 · Iteration 11 起步:多文件原子批量编辑(apply_edits)

- **NotebookLM 定 Iteration 11 + 对抗评审**:计划 P0 多文件批量编辑、P1 token 流式/kill-9 resume、P2 TODO/@file。**揪出张冠李戴**:P0 引用 `[1]`/沙箱 `[22]` 指向无关营销页(Ruh.AI「AI Workforce」脏来源),忽略;**驳回 mockito 测 SSE**(本项目早弃用,改假流式 HttpClient)。归档 guidance-10 + CONTRACT-11。
- **P0 多文件批量编辑落地**:`tools::apply_edits`(跨文件多处、每文件读一次、同文件按序叠加、全体校验唯一匹配、**全过才落盘、写失败回滚**)+ `edits_diff`(汇总 `-/+`)。agent:`apply_edits` tool_spec + `parse_edits` + execute 分支 + `preview_call` 渲染**汇总 diff 一次确认**(非逐个 [y/N])。
- **GLM 实测**:`把两文件的 8080 改 9090` → 模型一次 `apply_edits`(2 处)→ `applied 2`、两文件原子改完、approved。
- `cargo test --workspace` = **75 全绿**(71→75),clippy/fmt 干净。提交 `00a3903`。
- CONTRACT-11 剩:token 逐字流式(假流式 HttpClient 测)、kill-9 `--resume`、TODO 可视化、`@file`/Ctrl-C。

## 2026-07-14 · Iteration 10 收尾:config.toml/多 MCP + AnySearch 调研(插件式扩展实证)

- **`~/.ridge/config.toml` + 多 MCP(CONTRACT-10 P2 完成)**:`Config`/`McpServerCfg`(serde+toml)、`Config::parse/load`(坏 TOML/缺文件→默认空,降级 env 不崩)。main:`load_config`(`RIDGE_CONFIG` 或 `~/.ridge/config.toml`)、`real_provider` env>config>默认、`resolve_configured_mcp` 起 config 多 `[[mcp]]` + 兼容旧 env 单 server、skills_dir/预算/skip_danger 从 config 读。**密钥不进 config,只走 `RIDGE_API_KEY` env**。GLM 实测:仅给 key,provider/model/base_url 全来自 config → 应答 approved。提交 `6a2403e`。
- **AnySearch 调研(用户问:能否作更优网页工具)**:AnySearch = **AI agent 搜索基建 MCP server**,结构化输出(title/url/context+JSON)、batch/extract/垂直域,匿名可用/免费 key。**关键**:它是 MCP server → RidgeCode 的 config.toml 多 MCP **零改源码**即接入。**live 实测通过**:config 里加 `[[mcp]] anysearch (cmd /c npx -y mcp-remote https://api.anysearch.com/mcp)` → `已接入 1 个 MCP server` → 模型可用 `anysearch__search/extract/batch_search/get_sub_domains`。**这实证了北极星「加能力=加 MCP 配置不改源码」**。结论:内置零依赖当默认,AnySearch 当可选结构化升级。写入 `docs/web-search-and-anysearch.md`。⚠ Windows 需 `cmd="cmd" args=["/c","npx",…]`(Rust Command 不解析 npx.cmd)。
- **顺手修 bug**:内置 web_search 实测混进 DuckDuckGo 广告位 `y.js` 脏链 → `parse_duckduckgo` 过滤 `/y.js`/`ad_domain=`。提交 `189c4f5`。
- `cargo test --workspace` = **71 全绿**,clippy/fmt 干净。**CONTRACT-10 全部完成。**

## 2026-07-14 · Iteration 10 续:rename ridgecode + RAG 闭环 + 搜索硬化

- **全量 rename `ridge` → `ridgecode`(用户选 B)**:Cargo `[[bin]]`(含 `ridge-eval`→`ridgecode-eval`)、release.yml 归档名、REPL 提示符 `ridgecode>`、日志前缀 `[ridgecode]`、CLI 用法/横幅、README + CLAUDE.md。**env 前缀仍 `RIDGE_*`**(不破坏现有配置)。提交 `0961e11`。
- **`fetch_url`(RAG 闭环的「读」)**:`provider::search` 加 `fetch_url` + `html_to_text`(删 script/style/head 块、块级标签转换行、去标签、压缩、截断 4000)+ `strip_blocks`(ASCII 大小写不敏感、字节布局不变索引安全)。agent 加 tool_spec + act 异步分支 + `fetch_url_obs`,归只读;BASE_SYSTEM 引导 `web_search 找链接 → fetch_url 读正文 → 据原文作答`。GLM 实测抓 example.com 正文作答。提交 `0df7a09`。
- **`detect_net` 多探针**:单探针 → 并发探 google + gstatic 的 generate_204(`tokio::join!` + 每探针 3s `timeout`),任一通即 International,更抗抖动、避免卡 15s。provider 加 tokio 常规依赖。提交 `c91640b`。
- **搜索结果按 URL 去重**(保序)。提交 `ec41364`。
- `cargo test --workspace` = **67 全绿**(55→67),clippy/fmt 干净。CONTRACT-10 剩:API key 搜索后端(**live 需用户给 Brave/Tavily key**,无 key 回落现有 HTML)、config.toml + 多 MCP。
- ⚠ 仓库有并行工具 mimocode 会留破损 WIP,动 agent crate 前先编译确认基线。⚠ 用户智谱 key 明文,提醒轮换。宿主进程 `ridge.exe` 勿杀(会话 host,PID 会变)。

## 2026-07-14 · Iteration 10:富文本 REPL(彩色实时输出 + spinner + skip-danger)

- **用户 steer**:输出要直接在 shell 里直观彩色呈现(而非翻 trace.json)、要 skip-danger 模式、等待时有 loading 动画;并澄清 web_search 不必用 Brave/Tavily(内置无 key 方案即可)、本轮先不做多模态;产物暂定名 **RidgeCode**。
- **接手并行工具 mimocode 的 WIP**:仓库里发现未提交的 `rich_output.rs`(彩色/媒体/表格/formatter)+ lib/main 改动,但漏写 `mod rich_output;` → **整个 crate 编译不过**;MediaType 缺 PartialEq、format_progress 测试自相矛盾。按用户「接手并完善」补齐修好。
- **实时彩色输出**:`run_streamed` 重写 —— 等待转 spinner(braille,`is_terminal` 门控,非 TTY 不刷屏),每超步把新推理/工具调用/结果/校验彩色流式打到终端(reason 青、act 黄、final 加粗白带 🤖、verify 绿/红)。`print_report` 彩色状态行。
- **skip-danger 模式**:`--yolo`/`--skip-permissions`/`--dangerously-skip-permissions` 或 env `RIDGE_SKIP_PERMISSIONS=1` → AutoApprove、不再 [y/N](灾难命令仍硬拦截),红横幅提示。
- **品牌**:REPL 横幅改 `RidgeCode`(二进制命令暂仍 `ridge`,全量 rename 待确认拼写)。
- **GLM 实测**:①一次性 "解释 Rust 所有权" → 🤖 加粗答案 + 绿 verify + 青 stats;②`--yolo` 管道 "创建 hello.txt" → 红横幅 + 彩色工具链 + 文件自动创建、未卡 [y/N]。`cargo test --workspace` = **62 全绿**(55→62),clippy/fmt 干净。提交 `32c8684`。
- ⚠ 用户智谱 key 明文在对话里,再次提醒轮换。⚠ 仓库有并行工具 mimocode 会留破损 WIP,动 agent crate 前先编译确认基线。

## 2026-07-14 · Iteration 09:web_search —— 探网络(GFW)自动换搜索引擎

- **用户 steer 插队**:用户要一个 web search 工具,要能**检查网络环境、判断是不是在 GFW 内、据此用不同搜索引擎**,并「继续迭代直到工业级」。据「用户方向 > 计划」,插到原 CONTRACT-09(多文件批量编辑)前面。
- **能力**:给 agent 补上查外部实时信息的手。全放 `provider::search`(复用 reqwest,**零新依赖**)。`WebFetch` GET 接缝(假抓取器不联网可测)。`detect_net`:GET Google `generate_204` 探针 —— 通→`International`(直连/带 VPN)、不通→`Restricted`(墙内)。`web_search`:直连→**DuckDuckGo**(解 `uddg` 跳转拿真实 URL);受限→**Bing 中国版 cn.bing.com**(墙内可达、静态 HTML 好解析,DuckDuckGo/Google 墙内不可达)。HTML 解析纯 std(`strip_tags`/`urlencode`/`percent_decode`),不引 scraper/regex。
- **接入 agent**:`web_search` tool_spec + act 节点异步分支 + `web_search_obs`(懒探测一次、缓存网络环境、排版标题/链接/摘要);归**只读工具**不打扰权限门。
- **GLM + 真实网络实测(零改代码)**:`ridge "web_search 查 Rust 官网"` → 探测=**直连国际** → 引擎=**duckduckgo** → 返回 `rust-lang.org`/`github.com/rust-lang/rust`/`doc.rust-lang.org` 等真实链接(uddg 已解码)、approved。trace 记录「网络:直连国际·引擎:duckduckgo」决策透明。
- `cargo test --workspace` = **55 全绿**(50→55,provider +4 / agent +1),clippy/fmt 干净。提交 `9ee63c6`。
- **NotebookLM Iter10 指导 + 对抗评审**:采纳 `fetch_url`(RAG 闭环,**重排为 P0**,可 live 实测)、API key 搜索后端(env-first + 无 key 回落 HTML)、探测更稳(多探针+TTL);**驳回**「确定性引用审计」当 DoD 硬门槛(引用正确性不可确定性机检),并揪出其引用 `[10]` 是**无关的 awesome-MCP-servers 清单**(张冠李戴)。config.toml 降为 P2(key 可先走 env)。归档 guidance-09 + CONTRACT-10。
- ⚠ 用户智谱 key 明文在对话里,再次提醒轮换;**搜索 API 后端的 live 实测需用户提供 Brave/Tavily key**。

## 2026-07-14 · Iteration 08:驾驭工程 + 用户交互(补上「像 Claude Code」最缺两块)

- **用户 steer 重排**:用户直接指出「离 Claude Code 差距主要在**驾驭工程**和**用户交互**」→ 据「用户方向 > NotebookLM 计划」,本轮把原 CONTRACT-08(config.toml + 多 MCP)顺延 Iter09,改做工程能力 + 交互体验。
- **驾驭工程(工具集 3→6)**:`tools` 加 `edit_file`(唯一匹配替换,0/多处报错,Claude Code Edit 语义)、`read_file_range`(分段读大文件)、`search`(递归 glob + 子串,跳 target/.git,**Windows 无 grep 的可移植找代码接缝**,上限 200 行)。`agent` 加 3 个 tool_spec + `execute_tool_call` 分支;`read_file` 支持 offset/limit;BASE_SYSTEM 引导「改文件优先 edit_file、先 search/读再改」。
- **用户交互(权限门 diff 预览)**:`preview_call` —— edit_file 渲染 `-/+` diff、write_file 给规模、run_shell 给命令原文;用户**看着改动批准**而非盲批 JSON。search/read_file 归只读、不打扰审批。
- **GLM 实测(零改代码)**:`ridge "改 config.rs 的 port 8080→9090"` → 模型自主 **search×2 → read_file×2 → edit_file×1、零 write_file**,单行精准改、其余不动、approved。trace 佐证工具链。
- `cargo test --workspace` = **50 项全绿**(42→50,tools +5 / agent +3),clippy/fmt 干净。提交 `e7def41`。
- **NotebookLM Iter09 指导 + 对抗评审**:采纳多文件批量编辑(P0)、config.toml+多 MCP(P1)、TODO 可视化/Skills 匹配;**再次驳回 rmcp 当硬门槛**(自写 stdio 已连真实 server,兼容性来自协议非 SDK);**下调流式输出**为体验项(NBLM 自家 Iter05 note 就评「低」,前后不一致)。归档 guidance-08 + CONTRACT-09。
- ⚠ 用户智谱 key 明文在对话里,再次提醒轮换。

## 2026-07-14 · Iteration 07:方向转向 —— 模块化通用框架 + Skills 知识层

- **新方向(读了 NotebookLM Studio 13 篇 notes 得出)**:Ridge 从「编码 CLI」升级为**模块化、跨领域可扩展的通用 agent 框架** —— 加新能力 = 加一个 MCP 配置 或 一个 SKILL.md,不改 Rust 源码。四层解耦:内核(引擎)+ 协议(MCP)+ 知识(Skills)+ 协作(多智能体)+ 安全(权限门/拦截,沙箱待做)。写进 `docs/DIRECTION.md` + CLAUDE.md 北极星。
- **知识层落地(Skills 系统)**:`Skill` + `load_skills`(扫 `~/.ridge/skills/*/SKILL.md`,解析 frontmatter+正文)+ `build_system_prompt`(注入 system)。CLI `RIDGE_SKILLS_DIR` / 默认 `~/.ridge/skills`;`build_llm_agent_full` 全装配(MCP + 权限门 + Skills)。
- **GLM 实测(非编程任务)**:放一个 haiku SKILL.md → `ridge "写首关于 Rust 的诗"` → 模型按 skill 写出严格俳句 + 签名「— ridge」→ approved。**加 SKILL.md 就会做新事,零改代码。**
- `cargo test --workspace` = **42 项全绿**,clippy/fmt 干净。
- ⚠ 用户智谱 key 明文在对话里,提醒轮换。

## 2026-07-14 · Iteration 06:MCP 接入真实 server + trace + /compact + 通用 verify

- **DoD① 达成(真实第三方 MCP server)**:`mcp` 加 `notify`(补 `notifications/initialized` 握手);`examples/connect.rs` 连真实 `notebooklm-mcp.exe` → **握手 + tools/list 拿到 39 个真实工具**。CLI 加 `RIDGE_MCP_CMD`/`RIDGE_MCP_NAME` 接入。ridge(GLM)一次性任务实测:调 `nlm__notebook_list` → 路由到真实 notebooklm server → trace.json 里有真实笔记本数据(`手搓agent`/`source_count`)。
- **DoD⑥ trace.json**:每轮写审计(task/approved/steps/tokens/人读轨迹/多轮 history)。
- **DoD② /compact**:`compact_history` 保留首条+摘要+最近 N;REPL `/compact` 命令。
- **实测发现 + 修复**:MCP 信息类任务(列笔记本)无 `exit 0`/`passed` 信号 → verify 空转到上限烧 85k token。修:`verify_ok` —— 模型 finish 且**无失败信号**即接受(编码任务仍严卡 exit 0),让 ridge 对开放式任务也通用(像 Claude Code)。
- `cargo test --workspace` = **40 项全绿**,clippy/fmt 干净。
- ⚠ 用户的智谱 key 明文在对话里,提醒轮换。

## 2026-07-14 · Iteration 06 起步:安全硬门槛 —— 危险命令拦截

- NotebookLM 给 Iteration 06 计划 + 6 条 Definition of Done。**对抗评审**:驳回它把 rmcp 当 P0(DoD① 要的是「能调真实 MCP server」不等于「必须 rmcp」);采纳它列的硬门槛「危险命令拦截」为最高优先(便宜、离线可测)。归档 guidance-05。
- **P0 危险命令拦截**:`tools::is_dangerous_command`(denylist:`rm -rf /`/`mkfs`/fork 炸弹/`dd of=/dev/`/`format c:` 等,归一化大小写+空白防绕过)+ `execute_tool_call` 强制拦截——**即使用户批准也拒绝**。日常命令(`cargo build`/`rm -rf target/debug`)不误伤。
- DoD 现状:REPL/多轮/权限门/role=tool/危险拦截/serde 检查点 = ✅;待补:trace.json 审计、/compact、真实 MCP server 验证。
- `cargo test --workspace` = **39 项全绿**(agent 16 + tools 4 + …),clippy/fmt 干净。

## 2026-07-14 · Iteration 05:变成「像 Claude Code 的 CLI」(REPL + 流式 + 权限门)

- **P0a** provider 剥 `<think>` 标签(实测 GLM 会漏 `</think>` 进 content)。
- **P0b** 多轮 `role=tool` 正确回灌:`AgentState.history: Vec<Message>`,reason 推 assistant(带 tool_calls)、act 推 tool_result,`to_messages = [system]+history`。
- **P1** 交互式 REPL(`ridge` 无参进对话循环,跨轮携带 history,`/exit` `/reset` `/help`)。
- **P2** REPL 实时流式进度(接引擎 `StreamEvent`,`· reason#1 · act#2 …` 边跑边显)。
- **P3** 权限门 `Approver` trait(`AutoApprove`/`AutoDeny`/REPL 的 `StdinApprover`),有副作用工具执行前 `[y/N]`。
- **对抗评审**:拆分「流式」(只做引擎事件流,LLM token 流延后)、沙箱延后(权限门先),都写进 guidance-04。
- **GLM 真实 REPL 实测**:`ridge>` → 任务 → `run_shell {"cmd":"cargo build"}` 前 `[y/N]` 确认 → `· reason/act/verify` 流式 → `verify PASS, approved=true, tokens=861` → `/exit`。**用起来已经像 Claude Code**。
- `cargo test --workspace` = **37 项全绿**(agent 15 + provider 14 + …),clippy/fmt 干净。
- backlog(交付前):沙箱、LLM token 流、`~/.ridge/config.toml`、`/compact`、TUI、rmcp、子任务并行。

## 2026-07-13 · Eval harness(评测基础设施 / verification-first)

- 新增 `crates/eval`:`run_eval(cases)` 批量跑 agent,按确定性闸判 pass,聚合成功率 + token 成本;`EvalReport::pass_rate()`。bin `ridge-eval` 打印每 case PASS/FAIL + 总成功率。
- case 的 provider 可离线(CI 确定性)或真实模型(量真实成功率/成本)。
- 测试:2 case(一过一卡)→ pass_rate=0.5。`ridge-eval` demo:2/3 passed 67%。
- 工作区现 **6 crate**,`cargo test --workspace` = **34 项全绿**。

## 2026-07-13 · M5 完整:plan-and-execute 编排器(orchestrator-workers)

- `run_planned(planner, worker, task)`:planner(强模型)拆子任务,worker 逐个执行(build_llm_agent),聚合成 `PlanReport`(每个子任务的 approved/steps/tokens + 整体)。成本杠杆:planner≠worker。
- 串行执行(子任务常有依赖);独立子任务可 tokio::spawn 并行(引擎已支持),先要正确性。
- 测试:planner 拆 2 子任务 → worker 逐个到 approved → 整体通过。
- `cargo test --workspace` = **33 项全绿**。M5 从「分解」升级为「规划+执行」完整闭环。

## 2026-07-13 · ridge CLI:接真实 provider,成为可运行工具

- 重写 `crates/agent/src/main.rs`:解析任务 + `--cwd`;按环境变量(`RIDGE_API_KEY`/`RIDGE_PROVIDER`/`RIDGE_MODEL`/`RIDGE_BASE_URL`)装配真实 Anthropic/OpenAI provider → `build_llm_agent` 用结构化 tool_call 驱动真实 shell/文件工具;无 key 时降级跑离线脚本 demo。密钥绝不打印。
- demo 路径实测通过(approved=true steps=3,打印 checkpoint)。有 key 即真实 LLM 在 `--cwd` 目标项目里干活。
- 至此 ridge 从「demo」变成**可运行的编码 agent 工具**。

## 2026-07-13 · M5 起步:规划器(目标→子任务)

- `plan(provider, task)`:让 provider 把目标拆成有序子任务(JSON 数组),`parse_subtasks` 容忍模型包裹的解释文字(取首 `[` 到末 `]`);解析失败/出错**降级**为单个子任务(绝不返回空)。
- 子任务可交给 `build_llm_agent` 逐个执行;独立子任务靠引擎 fan-out 并行(引擎已支持)。
- 测试:JSON 数组正常解析 + 不可解析降级。
- `cargo test --workspace` = **32 项全绿**。至此 M1–M5 核心全部落地(M3 耐用执行/M5 规划为起步版)。

## 2026-07-13 · M4:独立模型 checker(maker≠checker 强形式)

- `build_llm_agent_reviewed(provider, mcp, reviewer)`:确定性 verify 通过后,再让一个**独立的** reviewer 模型看轨迹复核有没有作弊(删/跳测试、伪造输出),打回则 approved=false 回 reason。用**不同的** provider,别让写代码的模型自审。
- `build_core` 统一装配,verify 节点按有无 reviewer 分支;`review_request` 给 reviewer 铺 system(角色)+ user(任务+轨迹)。
- 测试:确定性闸通过但 reviewer REJECT(发现删测试)→ 最终不批准;reviewer APPROVE → 批准。
- `cargo test --workspace` = **30 项全绿**,clippy/fmt 干净。里程碑 M4 达成。

## 2026-07-13 · M2 接线:MCP 工具接进 agent

- agent 依赖 mcp;新增 `resolve_mcp(clients)`(各 initialize+list_tools,归一化成 ToolSpec + 命名空间路由表,降级不崩)+ `build_llm_agent_with(provider, McpTools)`。
- reason 把 内置 + MCP 工具一起 offer 给 LLM;act 按 `<server>__<tool>` 命名空间路由到对应 MCP 客户端(async),否则走内置工具。
- mcp 加 `FnTransport`(闭包充当传输,免 async-trait 造假服务器)。
- 端到端离线测:LLM 发 `ci__check` → act 路由到假 MCP 服务器 → 返回 `tests: passed` → verify approved。
- `cargo test --workspace` = **28 项全绿**,clippy/fmt 干净。**M2 从独立 crate 变成 agent 真能用的能力**。

## 2026-07-13 · M2 起步:最小 MCP 客户端(crates/mcp)

- 新增 `crates/mcp`:MCP = JSON-RPC 2.0。`McpClient` 做 initialize / tools/list / tools/call + `<server>__<tool>` 命名空间;`McpTransport` trait 把传输与协议解耦;`StdioTransport`(tokio 子进程,按 id 关联、跳通知)是生产传输。
- 离线测:`FakeTransport` 校验握手/列举/调用 + RPC 错误映射。协议核心 100% 离线可测。
- **对抗评审**:NotebookLM 荐官方 `rmcp` SDK,但其 stdio 传输离线无法单测、是重依赖 → 本轮先落可测的协议核心 + 最小 stdio 传输;要上生产把 `StdioTransport` 换 rmcp 即可(`McpTransport` 不变)。留待:把 MCP 工具并入 agent 的 `builtin_tool_specs` + 在 act 里按 `server__tool` 路由(async)。
- `cargo test --workspace` = **27 项全绿**,clippy/fmt 干净。工作区现 **5 crate**。

## 2026-07-13 · Iteration 03 P2(成本护栏 + 无进展检测 / 停机是设计的一半)

- provider `Completion` 加 `Usage`(prompt/completion tokens),两个 parse_response 从响应读用量。
- agent 加多层独立退出:`total_tokens`/`budget_tokens`(预算熔断)+ `stall`/`MAX_STALL`(无进展检测)。`must_stop` 汇总「回合上限 | 超预算 | 僵局」,shared 路由用它——scripted 路径两值恒 0,行为不变(对抗评审:预算放 agent 层,不进 langgraph 引擎)。
- 测试:预算耗尽 / 连续 MAX_STALL 轮输出不变 → 早于回合上限停机、approved=false。
- `cargo test --workspace` = **25 项全绿**,clippy/fmt 干净。

## 2026-07-13 · Iteration 03 P3(耐用执行 / M3 起步)

- `crates/langgraph` 新增 `FileCheckpointer`(每超步 append 一行 JSON,JSON Lines 版本日志)+ `CompiledGraph::resume(checkpoint)`(把主循环抽成 `run_loop`,invoke 从头 / resume 从快照共用)。`Checkpoint` 加条件 serde 派生。
- 测试:跑完落盘 → 全新 checkpointer 从磁盘读回超步 1 快照 → `resume` 续跑到同一终态(模拟崩溃后跨进程恢复,超步连续)。
- `cargo test --workspace` = **23 项全绿**,clippy/fmt 干净。提交前一条 aa881a6。
- 里程碑:M3(耐用执行)起步完成基础;bincode 落作后续优化。下一步候选:成本记账+预算熔断(agent 层)、无进展检测、或 M2(rmcp MCP 客户端)。

## 2026-07-13 · 工作流加对抗评审 + Iteration 03 P1(真实 HTTP provider)

- **工作流升级**:给 NotebookLM 驱动的循环加了**对抗评审**步骤(step 7)——不全信 NotebookLM(它是 maker 不是裁判,会张冠李戴引用、把概念放错层、过度设计)。关键决策要独立 checker + 高影响决策另起干净上下文当对抗评审员。写进全局 skill `notebooklm-iteration-loop` 与 `docs/WORKFLOW.md`。
  - 对抗评审实例(驳回):NotebookLM 建议把预算做成 `GraphError::BudgetExceeded`(app 层塞进通用引擎)+ 给「成本记账」引了不相关的 IoT 论文 → 驳回,预算归 agent 层。
- **Iter03 P1**:`crates/provider` 新增 `http::HttpClient` trait(分离传输与归一化)+ `ReqwestClient` + `OpenAiProvider`/`AnthropicProvider`(`build_request`→HTTP→`parse_response`)。测试:stub 传输走全链路(零网络)+ `mockito` 本地 server 校验 Authorization 头。首提交 9bd4464。
- 提交策略:用户授权直接提交 main(不走 PR)。

## 2026-07-13 · Iteration 02 P1 + Iteration 03 P0 完成

- **Iter02 P1**:新增 `crates/provider`(`LlmProvider` trait + Anthropic/OpenAI 工具调用**归一化**纯函数 + 离线 `ScriptedProvider`)。agent 新增 `build_llm_agent`:provider 吐结构化 tool_call → act 调真实 `tools` → verify 认 `exit 0` → approved,端到端离线可测。
- **闭环**:上传 Iter02 报告到 NotebookLM,取得 Iteration 03 指导(归档 `2026-07-13-notebooklm-guidance-02.md`)。核心:先做多轮 tool 结果回灌(离线可测),再接真实 HTTP(mockito),再成本熔断,最后 serde 落盘。
- **Iter03 P0**:`Message` 升级支持 assistant `tool_calls` 与 `role=tool` 结果;`openai::build_request` / `anthropic::build_request` 把统一历史铺成各自 wire(OpenAI role=tool + tool_call_id;Anthropic tool_use/tool_result 块 + 合并相邻同角色 + system 顶层)。纯函数,离线单测。
- 质量闸:`cargo test --workspace` = **19 项全绿**,clippy/fmt 干净。
- 下一步(Iter03 P1):真实 HTTP provider 客户端 —— 抽 `HttpClient` trait 分离传输与归一化,`mockito` 离线测。

## 2026-07-13 · Iteration 02 开工 + P0 完成

- 建 NotebookLM 驱动的迭代工作流(`docs/WORKFLOW.md` + `docs/iterations/`),沉淀为全局 skill `notebooklm-iteration-loop`。
- 上传 Iteration 01 报告到 NotebookLM,取得下一步指导:**先做物理闭环(P0 真实工具 → P1 真实 LLM)**。归档在 `docs/iterations/2026-07-13-notebooklm-guidance-01.md`。
- **P0 落地**:新增 `crates/tools`(真实 `read_file`/`write_file`/`run_shell`,跨平台),并把 `run_shell` 包成 agent 的 `shell_tool()`。工具层测试全绿。
- 决策:`run_shell` M1 阶段不做沙箱,只在受控命令上用;沙箱留到 harness 阶段。
- 下一步(Iteration 02 P1):`crates/provider` 的 `LlmProvider` trait + 真实实现,结构化 tool_call,把 `Brain` 换成真实模型。

## 2026-07-13 · Iteration 01(推倒重来)

- 删旧 ridge-code(rc-* 成本优化编码 agent),重建为 langgraph-rs 两层:`crates/langgraph`(手搓 Rust 版 LangGraph 引擎:GraphState+reducer、Pregel 超步+BSP、checkpoint 时间旅行、streaming、防跑飞)+ `crates/agent`(ReAct 循环 + maker-checker + 双保险停机,二进制 `ridge`)。
- 质量闸:`cargo test --workspace` 9 项全绿,clippy/fmt 干净。
- 旧码在 git 提交 `f0e65e6`,可 `git restore` 找回。
- 报告:`docs/REPORT-langgraph-rust.md`。
