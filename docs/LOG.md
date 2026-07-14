# 全局工作日志(append-only)

跨迭代的长期记忆。开工前读末尾 5–10 条;完成大块工作后追加一条。最新在最上面。

---

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
