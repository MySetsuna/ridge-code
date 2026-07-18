# CONTRACT · iteration-36 —— 全局显示英化 + 启动动画美化 + 修 banner 折行撕裂

> maker = 用户需求「项目全局显示移除中文,都用英文;启动动画不够好看;终止动画后标识乱了」(合成一轮)。checker = 本文正确性门禁。

## 缺口(代码索引核实)

- **显示中文**:TUI(`tui.rs` 的 `ui.note`/命令帮助/Panel 标题提示/状态栏标签/审批弹窗/输入框标题/浮窗标题)与 CLI(`main.rs` `--help`/`eprintln`/`node_label`)大量中文串直接呈现给用户。
- **动画/标识**:SPLASH 是 48 列 ASCII 艺术字;`flush_commits` 提交走 `Wrap{trim:false}`,终端窄于 48 列时逐行折行 → banner 撕裂错位(用户所述「终止动画后标识乱了」)。banner 左对齐、单色、10 帧硬切,「不够好看」。

## 目标

1. **全局显示英化**:所有**用户可见**串(TUI 提示/帮助/Panel/状态栏/弹窗、CLI help/日志/phase 标签)改英文。**代码注释(不显示)保留中文**;`lib.rs` 的模型上下文串(BASE_SYSTEM/observation/BLOCKED 详情 = LLM 读,非「显示」)本轮不动(避免改 agent 行为 + 连锁破测)。
2. **banner 不折行**:按终端宽度守卫 —— 宽 ≥ banner 宽才显 ASCII 艺术字(且**居中**);窄则退化为紧凑单行标题。消灭折行撕裂。
3. **动画美化**:居中 + 语义色(Primary/青)+ 英文 tagline + 更平滑帧数。

## 设计(最小面)

- **i18n**:逐处把用户可见字符串字面量译英。集中在 `tui.rs`(欢迎语/中断/排队/审批/`run_command` 各命令文案/Panel 构造标题与提示/`(未设)`→`(unset)`/`draw` 内 " ready"/输入框标题/浮窗标题/审批文案/`⚠越狱`→`⚠JAILBREAK`/phase "推理中"→"reasoning")与 `main.rs`(`--help` 文案/`eprintln`/`node_label` 各标签/headless 提示)。
- **banner**:纯函数 `splash_block(width) -> Vec<String>`:`width >= SPLASH_W` → 居中 ASCII banner + tagline;否则单行紧凑标题。终帧提交与动画帧同口径(居中,尾部无多余空格致折行)。SPLASH_TICKS 提到更平滑。`splash_frame` 保留纯函数(列渐显),渲染时居中 + Primary 色。
- **不改**:引擎、交互页逻辑、排队、越狱语义、BoN。

## 边界(不做)

- `lib.rs` 模型侧串(系统提示/observation)—— 非显示,留中文。
- 真终端逐像素目测的「好看」主观项 —— 只保证确定性可测的核(banner 纯函数 + 不折行 + 居中数据)。
- 运行时语言切换(i18n 框架/locale)—— YAGNI,直接英文硬串。

## 确定性验收信号

门禁 `cargo test --workspace && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` 全 **exit 0**。新增/改:
- `panel_titles_are_english`:5 个 Panel 标题(`config/provider/tools/models/agent`)`has_cjk==false`。
- `splash_block_guards_width`:`splash_block(SPLASH_W)` 含 banner 行(艺术字字符)且每行 ≤ 传入宽;`splash_block(10)`(窄)退化单行且 `has_cjk==false`。
- `splash_reveals_monotonically`(既有)按新口径更新仍绿。
- 既有断言中文显示串的测试同步改英(如有)。

## 停机

单轮;连续 2 轮验收不过 → 报告。价值门禁不适用(用户明确需求)。收尾替换 NLM 架构来源。
