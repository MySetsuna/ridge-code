# RidgeCode · Requirements Specification

> NotebookLM 的唯一已批准需求源。Pending 仅存本地 `PENDING-REQUIREMENTS.md`。

- 需求版本:`v0.2.0`

## 正式需求 (Active Requirements)

### REQ-20260801-01 · TUI 视觉、交互与展示层迭代

- 批准依据:`批准`
- 状态:`ACTIVE`
- 版本:`v0.2.0`
- 行为:`RidgeCode TUI 以清晰、科技感强且终端主题友好的界面呈现实际收到且允许展示的模型输出与最终回答；Answer 与实际 reasoning_content 分层呈现，不伪造隐藏推理；模型回复、调查结论、等待/执行状态与当前 Agent 活动须持续可见，长内容先展示摘要并可通过显式入口进入全屏详情，以滚动查看完整源文本；文件读取与工具输出默认合并为可折叠摘要，展开时保留完整有效细节，详情按终端宽度换行并由视口控制可见行数；支持有界、无外部解析依赖的行级 Markdown 展示（bold、code、header）与 ANSI 16 色语义角色；关键交互可发现、可操作，长任务期间保持响应并允许用户取消当前执行后接管输入；底部状态区明确区分输入 token 与输出 token；忙碌时待提交消息粘性显示于输入框上方，Enter 追加至队尾，Ctrl+Enter 立即推至队首且不打断当前模型回合。`
- 边界:`范围限于 crates/agent/src/tui 及其直接交互/渲染依赖、展示状态模型、布局、事件处理、渲染与测试，以及为展示完整实际观察而增加的展示事件保留接缝；不改变模型上下文的有界压缩、langgraph 核心语义，不把具体 LLM/MCP SDK 耦合进 TUI，不牺牲安全门、确定性验证、跨平台终端兼容或数据完整性，不升级无关依赖；保留 maker/checker、权限门、危险命令拦截、输入排队/取消、事件驱动主环、Viewport::Inline、insert_before 原生 scrollback 与 ANSI 16 色主题适配方向；不覆盖 samples/config.json 与 test_codegraph.ps1。工具摘要须保留，工具详情默认折叠；Markdown 仅作展示层解析，完整详情仅由用户主动打开并在有界视口中浏览。`
- 验收:`通过 cargo fmt --all --check、cargo test --workspace、cargo clippy --workspace --all-targets -- -D warnings；TUI 纯逻辑测试覆盖 Markdown span、语义色角色、折叠/展开、状态迁移、窄终端布局、CJK/emoji 宽度、长行物理换行、完整回答与完整多行 Diff 滚动浏览、Ctrl+R/Ctrl+O、输入/输出 token 分栏、Agent 活动状态、取消后接管与待提交队列预览/队首推送；证明静态提交文本无 ANSI 逃逸残留、文件读取/工具详情默认折叠且展开可访问全部行、模型上下文仍有界、LiveTranscript 64 块上限有效；完成真实终端或可复现渲染验收，证明长输出按宽度换行不刷屏、模型回复/调查结论/等待/当前活动可见、摘要可展开为全屏详情并滚动至尾部、工具默认收起且可展开、待提交消息不遮挡当前输入，Enter/Ctrl+Enter 不打断当前回合，Ctrl-C 可交还输入控制，且单帧刷新无明显卡顿；NotebookLM 建议经 CodeGraph、当前代码与测试核验后方可落地。`
- 追踪:`REQ → crates/agent/src/tui/*.rs 的状态/事件/渲染符号 → crates/agent/src/tui/tests.rs 与 workspace 质量闸；NLM 建议证据存于 .iteration/notebooklm-response.json；Note 清理以本轮迭代 ID、闭环证据与本地 archive 记录为准。

### REQ-20260802-02 · ReRelease 稳定包与 GitHub README

- 批准依据:`执行`
- 状态:`ACTIVE`
- 版本:`v0.2.0`
- 行为:稳定 TUI 基线通过质量门后，发布 v0.5.0 供实际验证；版本归档必须包含可运行的 RidgeCode 二进制、完整用法 README 与对应安装脚本，并将同一 README 更新至 GitHub 主分支。
- 边界:发布范围包含 README、Cargo 版本元数据、当前稳定 TUI/provider 源码及必要项目文档；排除 .iteration/、dist/、samples/config.json、本地测试脚本与 Pending 审批文件；不查询或修改 NotebookLM 进行中深研状态，不重写远端历史，不改变已验证的 TUI、安全门、provider 与 MCP 语义。
- 验收:提交前通过 cargo fmt --all -- --check、cargo test --workspace --locked、cargo clippy --workspace --all-targets --locked -- -D warnings、cargo build --workspace --locked；本机归档列出 README；推送后确认 origin/main、v0.5.0 标签、GitHub Release 资产与 README 可读且内容一致。
- 追踪:REQ → README/Cargo 版本与 crates/agent/src/tui、crates/provider/src 稳定代码 → workspace 质量门与 dist 归档 → GitHub main/tag/Release。

### REQ-20260803-01 · goal 长时间任务生命周期

- 批准依据:`批准`
- 状态:`ACTIVE`
- 版本:`v0.3.0`
- 行为:`提供可持久化的 goal 生命周期：创建、查看状态、继续/恢复、推进与完成/阻塞收敛；长时间运行保存目标、阶段、证据、失败原因与下一步，重启后可恢复，不以模型自述替代确定性验收。NotebookLM 仅提供经验证的架构建议。`
- 边界:`范围限于 ridgecode CLI/TUI 的 goal 命令入口、goal 状态模型与本地持久化、长任务恢复/收敛规则及测试；NLM 非运行时必需依赖，不上传原始日志、密钥或未批准需求；不改变 langgraph 核心并发语义、权限门、危险命令拦截或 provider/MCP SDK 边界；不引入无界后台任务。`
- 验收:`补充 goal 状态机、持久化原子写、重启恢复、完成/阻塞判定、取消与重复执行保护测试；通过 cargo fmt --all --check、cargo test --workspace --locked、cargo clippy --workspace --all-targets --locked -- -D warnings、cargo build --workspace --locked；完成一次本机 CLI/TUI smoke。`
- 追踪:`REQ → goal 状态/命令/持久化符号 → agent 测试与 workspace 质量门 → 经核验的 .iteration/notebooklm-response.json；不得将需求草案直接写入 NotebookLM 需求源。`

### REQ-20260803-02 · MCP/tool 链路与模型供应商身份修复

- 批准依据:`批准`
- 状态:`ACTIVE`
- 版本:`v0.3.0`
- 行为:`RidgeCode 明确发现并启动已配置的 MCP stdio server，完成 initialize、tools/list、tools/call 与结果回传；CodeGraph 使用实际可执行命令，初始化/列工具失败可观测；任一模型 function call 均与匹配的 function_call_output 成对进入下一次请求，异常/中断/恢复不发送悬空调用；TUI 与启动日志显示实际生效的供应商/模型，切换模型后持久化一致的身份、端点与模型。`
- 边界:`范围限于 crates/mcp、crates/agent 的 MCP 配置加载/握手/工具路由、Responses tool call/result 历史编排、会话恢复、provider/model 元信息解析与相关测试，以及用户级 ridge 配置的最小修正；不打印或提交 API key、OAuth token、原始会话内容；不修改用户无关配置，不改变 MCP 协议语义、权限门、危险命令拦截或 provider SDK 边界；保留用户工作区脏改动，兼容 Windows PATH 与 stdio。`
- 验收:`新增/修改握手、工具注册、调用结果、启动失败可观测、Responses 配对、恢复与 provider/model 一致性测试；通过 cargo fmt --all --check、cargo test --workspace --locked、cargo clippy --workspace --all-targets --locked -- -D warnings、cargo build --workspace --locked；本机 PATH 运行 /tools 或等价 smoke，确认 CodeGraph 工具可见且一次调用有结果。`
- 追踪:`REQ → resolve_configured_mcp/McpClient/Responses history/provider resolution → mcp/provider/agent tests → 本机配置与 PATH smoke；敏感值只留本地。`

### REQ-20260809-ROUTE-01 · Agent route 按任务特性选择 provider/model

- Approval evidence:`用户明确回复：批准`
- Status:`ACTIVE`
- Version:`v0.4.0`
- Behavior:`RidgeCode 从已加载且当前可用的 provider/model 配置构建可解释、可测试的 route 决策：根据任务难度、规模、类型与角色/派发方式选择或排序候选 provider/model；支持 subagent 与 agent teammate 派发路径；缺省、不可用或失败时确定性回退并可观测记录原因；不以模型自述替代路由结果。`
- Boundary:`范围含项目初始化所需 .iteration 状态、需求与状态快照、CodeGraph 查询、经批准的 NotebookLM/深度调研证据；crates/agent 的 route/dispatch 配置与决策模型、provider/model 能力元数据、subagent/teammate 派发接缝、确定性测试及必要 CLI/TUI 可观测性；仅改动证据证明必要的直接依赖。不得上传密钥、cookie、原始会话或未批准需求；不改变 langgraph 核心 BSP 语义、权限门、危险命令拦截、maker/checker 与 verify；不引入无界后台任务、具体第三方 SDK 耦合、无证据的 provider 重试/计费策略或无关 UI 重构；保留 provider trait/配置边界、MCP 协议语义、子 agent 只读约束及当前工作区脏改动。`
- Acceptance:`完成 request-intake、requirements gate、PROJECT-STATE 快照、context/iteration gate 与有界调研记录；形成经当前代码、CodeGraph、测试与真实配置核验的 route 设计与实现；测试覆盖任务特性分类/显式覆盖、候选过滤与排序、provider/model 不可用回退、subagent 与 teammate 派发、可观测选择原因及权限/只读边界；通过 cargo fmt --all --check、cargo test --workspace、cargo clippy --workspace --all-targets -- -D warnings、cargo build --workspace，并完成真实可用配置 route smoke，证明实际选择仅来自当前可用 providers/models。`
- Traceability:`REQ → provider/model 配置解析、route 决策、dispatch_agent/teammate 派发接缝 → crates/agent/provider/tests 与 workspace quality gates → 脱敏 smoke/调研证据；NotebookLM 报告仅作假设，闭环时清理或归档。

### REQ-20260810-A2A-01 · Agent-to-agent 多协议通信重构

- Approval evidence:`用户明确指令：自动通过审批，直接按nlm迭代流程走，启动深度研究，然后直接开发，不卡再需求审批。`
- Status:`ACTIVE`
- Version:`v0.5.0`
- Behavior:`RidgeCode 支持有界、可审计的 agent-to-agent 协作：主 agent 可通过明确的通信信封与协议适配接缝向一个或多个角色 agent 发送任务、上下文/能力约束与取消信号，并接收带有发送方、关联 ID、状态、结果或错误的结构化响应；同一协作语义可落在至少两种传输协议上，协议差异不得泄漏进 agent 业务状态机；保留只读子 agent、安全门、maker/checker、确定性验证与并发/步数上限。
- Boundary:`范围限于 crates/agent 的 agent 协作协议/消息模型、dispatch 与 teammate 接缝、传输适配及必要的 crates/mcp/provider 直接边界与测试；优先复用现有 trait、MCP/stdio 与本地调用能力，不耦合具体第三方 agent SDK，不改变 langgraph BSP 语义、工具权限、危险命令拦截、provider trait 或 MCP JSON-RPC 语义；不得引入无界后台任务、隐式跨会话共享、未审计的远程执行或敏感信息上传。
- Acceptance:`完成 request-intake、NotebookLM 深度研究假设核验与 CodeGraph 设计审计；定义版本化消息信封、关联/取消/错误语义与至少两种传输协议的可插拔适配；测试覆盖握手/能力协商、请求响应关联、并发隔离、超时/取消、传输失败、只读权限与 maker/checker 边界；提供可启动的跨进程 stdio peer（serve/call），可选 HMAC/时间窗/nonce replay 防护；通过 cargo fmt --all -- --check、cargo test --workspace --locked、cargo clippy --workspace --all-targets --locked -- -D warnings、cargo build --workspace --locked，并完成本机无密钥 smoke，证明两种协议均可完成一次 agent-to-agent 任务闭环。
- Traceability:`REQ → agent communication envelope/transport/dispatch symbols → protocol adapters and deterministic tests → local two-protocol smoke → workspace quality gates; NotebookLM output remains hypothesis and is archived only after current-code verification.`

### REQ-20260810-OPEN-VISION-01 · 开放愿景落地与质量治理

- Approval evidence:`用户批准 PENDING-REQ-20260810-OPEN-VISION-01`
- Status:`ACTIVE`
- Version:`v0.1.0`
- Behavior: RidgeCode 将 Open Vision Note 中七类方向转为可独立验收的实现切片：高级 agent 通信安全与隐私边界（签名/认证/重放/审计，TEE 仅在可验证边界内）；自主权等级与治理元数据；原生 grapheme/Unicode 能力探测与安全回退；图谱支撑推理接缝；离线 store-and-forward/federated A2A 传输；有界 Web/PWA 事件流与接管；推理树离线审计。每个切片保持现有权限门、只读子 agent、maker/checker、确定性 verify、BSP 与 MCP/provider 边界；仓库同时提供可重复覆盖率采集、Sonar 扫描与不可绕过的质量阈值。
- Boundary: 仅限当前 Rust workspace、必要的 scripts/.github 质量配置与明确的本地/CI 适配；不上传密钥、cookie、原始敏感日志；不引入无界后台任务、未审计远程执行、隐式跨会话共享或未经批准的第三方 agent SDK；开放愿景方向须分片提交，不改变既有安全不变量。
- Acceptance: 七类方向分别登记为可追踪实现切片并给出本地证据；每个已实现切片有正向、失败、边界与回归测试，覆盖率较当前基线可量化提升；接入 SonarScanner/SonarCloud 或等价 Sonar 分析，固定配置、排除规则、覆盖率报告路径与质量阈值，扫描失败即失败；统一质量阀必须包含 cargo fmt --all -- --check、cargo test --workspace --locked、cargo clippy --workspace --all-targets --locked -- -D warnings、cargo build --workspace --locked、cargo llvm-cov --workspace --all-features --lcov --fail-under-lines <approved threshold>、git diff --check 与 Sonar quality gate；阀失败须修复代码/测试/架构后重跑，不得降阈值、跳过扫描、排除失败目录或伪造结果；完成无密钥 smoke、状态/需求/迭代门禁、提交推送。
- Traceability: Open Vision Note → REQ → per-slice design/code/tests → coverage artifact + Sonar report → deterministic quality gate → local smoke → archive evidence；NotebookLM 仅作候选方向来源，代码、测试、扫描与运行证据为事实依据。

### REQ-20260811-COMMANDS-01 · Commands 业务完整性与 provider/model 两阶段选择

- Approval evidence:`用户明确回复：批准这个需求`
- Status:`ACTIVE`
- Version:`v0.1.0`
- Behavior:`建立可审计的 command inventory，使已实现业务均有可发现的命令、帮助文本、路由和取消/返回语义；恢复 /login，保证其在命令面板、帮助与路由中可发现且能进入既有登录流程。/model 进入 provider/model 选择流程时，展示当前配置中所有可用 provider 的模型，保留 provider 身份、模型身份与稳定排序，支持面板内搜索/过滤模型，空结果、加载失败与取消均有明确状态；模型列表不混入 effort。选定 provider/model 后，明确转入独立 effort 选择界面；effort 只作用于刚选中的模型，支持上下/回车/取消并持久化一致状态，不得在未选 model 前要求选择 effort。既有 /provider、/config、/tools、/agent、/compact、/help、/goal 等命令逐项核对，修复丢失、重复、不可达、状态错配与键盘行为不一致，并保持命令面板与直接命令入口一致。`
- Boundary:`仅改动 RidgeCode commands 的声明/帮助/路由、TUI command palette 与 provider/model/effort 选择状态、必要配置/catalog 适配、相关单元测试、确定性离线 PTY E2E 与使用文档；不改变 langgraph BSP、安全门、危险命令拦截、provider/MCP 协议语义或无关视觉重构；测试 fixture 不联网、不使用真实密钥。`
- Acceptance:`命令 inventory/路由/帮助覆盖全部公开 commands，覆盖 /login、/model、provider/model 搜索、两阶段 model→effort、取消/返回与错误状态；模型聚合与搜索覆盖多 provider、重复模型、空结果、加载失败、稳定排序与敏感信息不泄漏；PTY E2E 使用离线 fixture 证明 /help 能发现 /login，/login 可达，/model 展示多 provider 模型并可输入搜索，选 model 后才出现 effort 面板，Enter/Backspace/上下/取消均改变正确状态且无不可见 modal；通过 cargo fmt --all -- --check、cargo test --workspace --locked、cargo clippy --workspace --all-targets --locked -- -D warnings、cargo build --workspace --locked、git diff --check 与现有质量阀。`
- Traceability:`REQ → commands inventory/command router → provider/model catalog aggregation and TUI selection state → agent unit tests + deterministic PTY E2E → workspace quality gates.

### REQ-20260812-GOAL-RUN-01 · Goal 与真实 Agent Run 绑定

- Approval evidence:`用户明确回复：批准。另外，sonar已经接通`
- Status:`ACTIVE`
- Version:`v0.6.0`
- Behavior:`在保留现有 ridgecode goal CLI 状态机与默认运行行为的前提下，增加显式 opt-in 的 goal-bound agent run。任务开始时写入 running/phase；确定性 approved 时写入 completed/evidence；StepLimit、timeout、cancel 或确定性失败时写入 blocked/failure_reason/next_step；resume 必须读取同一 goal 与最近一次有界 durable run facts，支持进程重启后继续，而不是只把 status 从 blocked 改回 active。`
- Boundary:`范围限于 crates/agent/src/goal.rs、main.rs 的 CLI/headless run 接缝、必要的 run manifest/恢复适配、goal/运行状态测试、确定性本机 smoke 与最小文档；goal 数据位于用户 cwd 下 `.ridge/goal.json`，写入必须原子且有界；默认无 goal 时无额外 I/O。`
- Non-goals:`不改变无 goal 的 CLI/TUI/headless 默认路径；不新增无界后台任务；不以模型自述判断完成；不改 langgraph BSP、provider/MCP/A2A 协议、安全门、危险命令拦截、maker-checker 或已有 Goal CLI 语义；不把 NotebookLM 输出写入运行时。`
- Acceptance:`补充 start/progress/approved/StepLimit/timeout/cancel/failure 与重复执行保护测试；补充进程重启后 load/resume round-trip，证明 evidence/failure_reason/next_step 与 durable facts 有界且无敏感字段；完成无 key CLI/headless fixture smoke；通过 `cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo build --workspace --locked`、`git diff --check`；Sonar quality gate 接通后必须纳入最终验证。`
- Traceability:`REQ → goal/run binding symbols → state transition and persistence tests → restart/terminal smoke → workspace quality gates + Sonar → archived evidence.`

### REQ-20260812-AGENT-TUI-FIX-01 · Agent 驾驭闭环、跨终端 TUI 输入、goal 命令与 scrollback 修复

- Approval evidence:`批准`
- Status:`ACTIVE`
- Version:`v0.1.0`
- Behavior:`agent 对只读探索设置可观测、有界的阶段推进与停滞检测；连续探索无进展时能确定性转入编写/澄清/阻塞路径，不因重复读文件无限循环至退出；工具调用、阶段、停滞原因和下一步对用户可见。TUI 将不同终端产生的 Backspace、Enter、鼠标与功能键序列统一归一化为稳定事件，PowerShell、Windows 传统控制台、Windows Terminal、常见 ANSI/VT 终端及可复现 PTY 中编辑、提交、取消、面板导航和鼠标操作一致可用。`/goal '需求'` 解析为完整 goal 标题并持久化，空参数、引号/空白、重复运行与错误状态有明确提示，不破坏既有 `/goal` 子命令。已提交历史输出通过 `Terminal::insert_before` 写入终端原生 scrollback；Live 视口只保留当前活动帧，历史可由终端滚动、选取、搜索回溯，不以反复 ratatui 重绘替代 scrollback。`
- Boundary:`保留现有 langgraph BSP 语义、权限门、危险命令拦截、maker/checker、确定性 verify、MCP/provider/A2A 协议语义、安全边界、只读子 agent 约束、输入排队/取消语义、TUI Inline viewport 与终端主题；修复须兼容没有鼠标/VT 能力的终端并提供安全回退，不打印密钥、cookie 或原始敏感会话。goal 数据仍位于 cwd 下 `.ridge/goal.json`，写入须原子且有界；历史提交不得丢失、重复或被后续帧覆盖。`
- Acceptance:`新增 agent 阶段转移、重复探索、无进展、写入触发、StepLimit/timeout/cancel 与诊断测试，证明探索预算、进展信号和写入路径均有界且无重复读文件死循环；新增按原始字节/事件构造的 Backspace(0x08/0x7f)、Enter(0x0a/0x0d/CSI/VT)、功能键、鼠标序列和 release/press 过滤测试，并完成可复现 Windows/PTY smoke；新增 `/goal '需求'`、带空白/引号、空参数、重复执行、持久化 round-trip 测试；新增 scrollback/Live 回归，证明提交历史进入 `insert_before` 原生 scrollback，后续刷新不覆盖/重复历史，Live 仅承载当前帧且历史可滚动回溯；通过 cargo fmt --all --check、cargo test --workspace --locked、cargo clippy --workspace --all-targets --locked -- -D warnings、cargo build --workspace --locked、git diff --check，并运行现有 Sonar quality gate（如其入口已配置）。`
- Traceability:`PENDING-REQ-20260812-AGENT-TUI-FIX-01 → agent phase/stall/progress symbols、TUI raw input/event normalization、goal command parser/route、scrollback commit boundary → 单元/PTY/终端回归测试 → workspace quality gates、无密钥 runtime smoke 与 Sonar 证据。

### REQ-20260812-24H-AGENT-01 · 24 小时持续迭代驾驭闭环

- Approval evidence:`批准`
- Status:`ACTIVE`
- Version:`v0.1.0`
- Behavior:`RidgeCode 面向长达 24 小时的无人值守项目迭代，能从任务理解稳定进入计划、写入、验证与收敛；不得长期停留在思考/读文件阶段后退出。每轮必须有可观测阶段、心跳、进展事实、当前阻塞、下一动作与停止原因；写入失败、测试失败、provider/subagent 超时、进程重启、取消与上下文压缩后可恢复，不重复破坏性操作，不因单次失败丢失目标与已完成事实。任务完成须由确定性质量信号或明确阻塞收敛，不能只信模型自述。`
- Boundary:`范围含 crates/agent 的运行状态/阶段路由、长任务循环、subagent/teammate 派发与取消/超时/重试、goal 持久化与恢复、durable state/进展信号、TUI/CLI 可观测性、相关单元/集成/PTY smoke 与本地长跑 harness；必要的 scripts 与文档。不升级 tag 或 Cargo 版本；不改变 langgraph BSP 语义、权限门、危险命令拦截、maker/checker、确定性 verify、MCP/provider/A2A 协议边界；不引入无界后台任务、静默无限重试、自动 push/release、密钥/cookie 上传或未经批准的第三方 SDK。`
- Acceptance:`补充阶段转移/写入触发/重复探索/停滞/超时/取消/恢复/重启/上下文压缩/质量门测试；执行 bounded soak（至少覆盖多轮任务、provider/subagent 故障、进程重启与恢复）；通过 cargo fmt --all -- --check、cargo test --workspace --locked、cargo clippy --workspace --all-targets --locked -- -D warnings、cargo build --workspace --locked、git diff --check；本机 release 包安装后以真实终端完成至少一项不同维度复杂写入任务，并保存脱敏证据。`
- Traceability:`REQ → agent phase/route、dispatch/timeout/cancel、goal/signal/persistence、TUI activity/diagnostics → 单元/集成/PTY/soak tests → workspace quality gates → 本机 release smoke。`
## 修订账本 (Revision Ledger)

关闭的 Pending、历史修订与审批证据写入 `docs/archive/events-YYYY-MM.jsonl`；本文件仅保留当前 Active 条款。
