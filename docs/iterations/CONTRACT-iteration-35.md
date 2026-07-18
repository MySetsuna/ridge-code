# CONTRACT · iteration-35 —— 交互页框架:斜杠即弹 + 五命令搓成可搜索 Panel(配置页就地编辑)

> maker = 用户需求(斜杠自动补全、各命令开交互页、页内搜索框、配置页),已定「全套 + 配置页就地编辑」。checker = 本文正确性门禁。

## 缺口(代码索引核实)

- **斜杠不自动弹**:补全浮窗只 `Tab`(`PopupOpen`)触发;`Insert(c)` 先关浮窗,打 `/` 反关列表。
- **命令只打文字**:`/config`/`/provider`/`/tools`/`/models`/`/agent` 全 `ui.note` 打一坨,无导航/搜索/就地操作。
- iter-32 `PopupKind::ModelPick`/`build_model_popup` 是「选中即动作」特例,本轮被通用 **Panel** 取代 → 删(现状不留两套模型选择器)。

## 目标

1. **斜杠/@ 即弹随打随滤**:`Insert`/`Backspace` 后重算 `build_popup` —— 打 `/` 现命令表、`/mo` 滤 `/model*`,`@` 同理。
2. **五命令开可搜索 Panel**(模态覆视口居中):标题 + 搜索框(随打随滤)+ 滚动列表(高亮选中)+ 提示 + 选中动作。
3. **配置页就地编辑**:↑↓ 选键 → Enter 进编辑 → 输新值 → Enter 落盘(`persist_config`)+ **live 应用**(provider/model/base_url 重 swap、status_bar 换底栏、allow_jailbreak 切开关)→ Esc 取消。
4. 各页动作:Config=就地编辑;Models=Enter `swap_model`+缓存 ctx_window;Provider=Enter 切档;Tools/Agent=只读浏览+搜索。

## 设计(最小面 / 复用优先)

- **删** iter-32 popup 特例:`PopupKind`/`ModelPick`/`build_model_popup`/`Popup.kind`/`Popup.picks` 及 PopupApply 分支、相关测试 → `Popup` 回纯文本补全,`PopupApply` 回 `apply_completion` 单路。
- **`Panel`**:`{kind: PanelKind{Config|Provider|Tools|Models|Agent}, title, query, rows: Vec<PanelRow{key,value,ctx}>, view: Vec<usize>, sel, editing: Option<String>}`。`Ui` 加 `panel: Option<Panel>`。
- **纯函数** `panel_filter(rows, query) -> Vec<usize>`(key+value 不分大小写子串;空 query 全含)。Panel 方法 `retype`/`move_up`/`move_down`/`selected`。构造 `config_panel`/`provider_panel`/`tools_panel`/`models_panel`/`agent_panel`。
- **渲染** `draw_panel`:居中框 + `🔍 query`(编辑时 `✎ 新值`)+ 过滤列表(选中高亮)+ 提示行。draw 里 panel 开时抑制输入光标。
- **键路由**:模态优先级 = 审批 > **Panel** > 浮窗 > 输入;纯路由 `panel_action(key) -> PanelAction`;主环 `ui.panel.is_some()` 时处理并 `continue`;编辑态字符入编辑缓冲,浏览态入 query。Enter 动作分派(需 `swap`/`meta`/持久化)在主环。
- **复用抽取**:`switch_provider(name, meta, swap, ui)`(`/provider use` 与 Provider 页共用)、`apply_config_live(key, val, meta, swap, ui)`(配置页编辑后 live 应用)。
- **不改**:逻辑光标/流式/审批模态/状态双栏/输入排队/越狱开关/BoN。

## 边界(不做)

- Panel 分组/多列表格美化/排序 —— 单列 key + 右值。
- provider 交互式**创建**(`/provider add` 仍走命令)—— 后续。
- 非 live 配置键(budget_tokens/skills_dir/skip_danger)编辑后仅持久化,下次启动生效(note 明示)。

## 确定性验收信号

门禁 `cargo test --workspace && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` 全 **exit 0**。新增测试:
- `panel_filter_substring_case_insensitive`:命中 key/value、大小写无关、空 query 全含、无命中空。
- `panel_nav_and_retype_clamp`:过滤后 sel 不越界;move_up/down 到边界钳位。
- `config_panel_lists_all_config_keys`:行 key 集 == `CONFIG_KEYS`。
- `models_panel_selects_current`:current 命中 → sel 落其 view 位次。
- `panel_action_routes_keys`:Up/Down/Enter/Esc/Char/Backspace 纯路由正确。
- `slash_popup_live`(守):`build_popup` 于 `"/"` 全表、`"/mo"` 滤 `/model*`。

## 停机

单轮;连续 2 轮验收不过 → 报告。价值门禁不适用(用户明确需求)。收尾替换 NLM 架构来源(iter-33/34/35 一并)。
