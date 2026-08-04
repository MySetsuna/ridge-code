# RidgeCode · Requirements Specification

> NotebookLM 的唯一已批准需求源。Pending 仅存本地 `PENDING-REQUIREMENTS.md`。

- 需求版本:`v0.2.0`

## 正式需求 (Active Requirements)

### REQ-20260801-01 · TUI 视觉、交互与展示层迭代

- 批准依据:`批准`
- 状态:`ACTIVE`
- 版本:`v0.2.0`
- 行为:`RidgeCode TUI 以清晰、科技感强且终端主题友好的界面呈现实际收到且允许展示的模型输出与最终回答；Answer 与实际 reasoning_content 分层呈现，不伪造隐藏推理；模型回复、调查结论、等待/执行状态与当前 Agent 活动须持续可见，长内容先展示摘要并可展开查看有界详情；文件读取与工具输出默认合并为可折叠摘要，展开时显示有界、按终端宽度换行的有效细节；支持有界、无外部解析依赖的行级 Markdown 展示（bold、code、header）与 ANSI 16 色语义角色；关键交互可发现、可操作，长任务期间保持响应并允许用户取消当前执行后接管输入；底部状态区明确区分输入 token 与输出 token；忙碌时待提交消息粘性显示于输入框上方，Enter 追加至队尾，Ctrl+Enter 立即推至队首且不打断当前模型回合。`
- 边界:`范围限于 crates/agent/src/tui 及其直接交互/渲染依赖、展示状态模型、布局、事件处理、渲染与测试，以及按迭代门进行的 NotebookLM 架构/业界方案调研与本轮 Note 清理。不改变 langgraph 核心语义，不把具体 LLM/MCP SDK 耦合进 TUI，不牺牲安全门、确定性验证、跨平台终端兼容或数据完整性，不升级无关依赖；保留 maker/checker、权限门、危险命令拦截、输入排队/取消、事件驱动主环、Viewport::Inline、insert_before 原生 scrollback 与 ANSI 16 色主题适配方向；不覆盖 samples/config.json 与 test_codegraph.ps1。工具摘要须保留，工具详情默认折叠且有界；Markdown 仅作展示层解析。`
- 验收:`通过 cargo fmt --all --check、cargo test --workspace、cargo clippy --workspace --all-targets -- -D warnings；TUI 纯逻辑测试覆盖 Markdown span、语义色角色、折叠/展开、状态迁移、窄终端布局、CJK/emoji 宽度、长行物理换行、Ctrl+R/Ctrl+O、输入/输出 token 分栏、Agent 活动状态、取消后接管与待提交队列预览/队首推送；证明静态提交文本无 ANSI 逃逸残留、文件读取/工具详情默认折叠且展开不越界、LiveTranscript 64 块上限有效；完成真实终端或可复现渲染验收，证明长输出按宽度换行不刷屏、模型回复/调查结论/等待/当前活动可见、摘要可展开、工具默认收起且可展开、待提交消息不遮挡当前输入，Enter/Ctrl+Enter 不打断当前回合，Ctrl-C 可交还输入控制，且单帧刷新无明显卡顿；NotebookLM 建议经 CodeGraph、当前代码与测试核验后方可落地。`
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
## 修订账本 (Revision Ledger)

关闭的 Pending、历史修订与审批证据写入 `docs/archive/events-YYYY-MM.jsonl`；本文件仅保留当前 Active 条款。
