# RidgeCode · Requirements Specification

> NotebookLM 的唯一已批准需求源。Pending 仅存本地 `PENDING-REQUIREMENTS.md`。

- 需求版本:`v0.2.0`

## 正式需求 (Active Requirements)

### REQ-20260801-01 · TUI 视觉、交互与展示层迭代

- 批准依据:`批准`
- 状态:`ACTIVE`
- 版本:`v0.2.0`
- 行为:`RidgeCode TUI 以清晰、科技感强且终端主题友好的界面呈现实际收到且允许展示的模型输出与最终回答；Answer 与实际 reasoning_content 分层呈现，不伪造隐藏推理；工具调用默认折叠并可按需展开；支持有界、无外部解析依赖的行级 Markdown 展示（bold、code、header）与 ANSI 16 色语义角色；关键交互可发现、可操作，长任务期间保持响应。`
- 边界:`范围限于 crates/agent/src/tui 及其直接交互/渲染依赖、展示状态模型、布局、事件处理、渲染与测试，以及按迭代门进行的 NotebookLM 架构/业界方案调研与本轮 Note 清理。不改变 langgraph 核心语义，不把具体 LLM/MCP SDK 耦合进 TUI，不牺牲安全门、确定性验证、跨平台终端兼容或数据完整性，不升级无关依赖；保留 maker/checker、权限门、危险命令拦截、输入排队/取消、事件驱动主环、Viewport::Inline、insert_before 原生 scrollback 与 ANSI 16 色主题适配方向；不覆盖 samples/config.json 与 test_codegraph.ps1。工具摘要须保留，工具详情默认折叠且有界；Markdown 仅作展示层解析。`
- 验收:`通过 cargo fmt --all --check、cargo test --workspace、cargo clippy --workspace --all-targets -- -D warnings；TUI 纯逻辑测试覆盖 Markdown span、语义色角色、折叠/展开、状态迁移、窄终端布局、CJK/emoji 宽度与关键键位；证明静态提交文本无 ANSI 逃逸残留、工具详情不越界、LiveTranscript 64 块上限有效；完成真实终端或可复现渲染验收，证明长输出不刷屏、工具默认收起且可展开、输入与取消不阻塞，且单帧刷新无明显卡顿；NotebookLM 建议经 CodeGraph、当前代码与测试核验后方可落地。`
- 追踪:`REQ → crates/agent/src/tui/*.rs 的状态/事件/渲染符号 → crates/agent/src/tui/tests.rs 与 workspace 质量闸；NLM 建议证据存于 .iteration/notebooklm-response.json；Note 清理以本轮迭代 ID、闭环证据与本地 archive 记录为准。

### REQ-20260802-02 · ReRelease 稳定包与 GitHub README

- 批准依据:`执行`
- 状态:`ACTIVE`
- 版本:`v0.2.0`
- 行为:稳定 TUI 基线通过质量门后，发布 v0.5.0 供实际验证；版本归档必须包含可运行的 RidgeCode 二进制、完整用法 README 与对应安装脚本，并将同一 README 更新至 GitHub 主分支。
- 边界:发布范围包含 README、Cargo 版本元数据、当前稳定 TUI/provider 源码及必要项目文档；排除 .iteration/、dist/、samples/config.json、本地测试脚本与 Pending 审批文件；不查询或修改 NotebookLM 进行中深研状态，不重写远端历史，不改变已验证的 TUI、安全门、provider 与 MCP 语义。
- 验收:提交前通过 cargo fmt --all -- --check、cargo test --workspace --locked、cargo clippy --workspace --all-targets --locked -- -D warnings、cargo build --workspace --locked；本机归档列出 README；推送后确认 origin/main、v0.5.0 标签、GitHub Release 资产与 README 可读且内容一致。
- 追踪:REQ → README/Cargo 版本与 crates/agent/src/tui、crates/provider/src 稳定代码 → workspace 质量门与 dist 归档 → GitHub main/tag/Release。
## 修订账本 (Revision Ledger)

关闭的 Pending、历史修订与审批证据写入 `docs/archive/events-YYYY-MM.jsonl`；本文件仅保留当前 Active 条款。
