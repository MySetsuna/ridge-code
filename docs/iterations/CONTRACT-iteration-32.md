# CONTRACT · iteration-32 —— 程序内模型选择器浮窗(收需求 1「in-UI 便捷」尾巴)

> maker = 用户需求原文第 1 条(「很方便的在程序界面内进行 provider 添加,模型切换」)。checker = 本文正确性门禁。

## 缺口(代码索引核实)

iter-29 已给 `/models`(实时列表含 ctx size)+ `/provider add`,但**切换模型仍须手打** `/model <name>`(要先记住 id)。既有 `Popup`(iter-27,↑↓/Tab/Enter/Esc + `apply_completion` 文本补全)只服务 `/`、`@` 文本补全,**无「选中即执行动作」变体**。且 `/model <name>` 的热切换只认 `env RIDGE_API_KEY`,**config.json 内联 `api_key` 用户无法切模型**(commit 6aa79be 放开内联启动后遗留的缺口)。

## 目标

`/model pick` 拉起模型选择器浮窗:复用 `/models` 抓取 → 既有 `Popup` ↑↓/Tab/Enter 选 → 选中即热切换模型 + 缓存该模型真实 `ctx_window`(顶栏/底栏 ctx% 分母转真值),无需手打 id。顺带修 config 内联 key 无法切模型的根因(共用切换路径)。

## 设计(最小面)

- `Popup` 加 `kind: PopupKind{Complete|ModelPick}` + `picks: Vec<ModelPick{id, ctx: Option<u64>}>`(仅 ModelPick 填,Complete 空)。既有 `build_popup` 两处构造补 `kind: Complete, picks: vec![]`。
- **纯函数** `build_model_popup(models: &[provider::models::ModelInfo], current: &str) -> Option<Popup>`:空列表 → None;items 显示 `"{id}  ·  ctx {fmt_ctx|?}"`,`picks` 平行携 id+ctx,`selected` 落在当前模型下标(无匹配 → 0)。
- **共用切换** `swap_model(swap, meta, model, ui)`:密钥经 `current_api_key()`(env 优先,回落 config 内联)→ `make_provider` → `swap.swap` + `meta.model=model` + note;无 key 则红字提示。`/model <name>` 文本命令改调它(根因修:内联 key 也能切)。
- `run_command` 加 `_ if input == "/model pick"` 臂(**置于** `starts_with("/model ")` 臂**之前**,免被当成切到名为 pick 的模型):`current_api_key` → `fetch_models` 裹 15s `timeout` → `ui.popup = build_model_popup(&list, &meta.model)`;空/超时/无 key 各红字。
- 主环 `PopupApply` 按 `kind` 分派:`Complete → apply_completion`;`ModelPick → {ctx→meta.ctx_window; swap_model(pick.id)}`。
- `/help`、`/model` 文案补 `/model pick`。

## 边界(不做)

- provider 交互式创建浮窗(`/provider add` 仍走命令)—— 下一轮;本轮只闭「模型切换」这一最高频动作。
- 浮窗内按 ctx/价格排序、分组 —— 超范围;当前按 id 排序(与列表一致)。
- `@` 递归目录补全 —— 后续轮。

## 确定性验收信号

门禁 `cargo test --workspace && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` 全 **exit 0**。新增测试:
- `build_model_popup_selects_current_and_formats`:三模型、current 命中中间 → `selected==1`、`kind==ModelPick`、`items[1]` 含 `ctx`、`picks` 与 items 等长且 `picks[1].id==current`。
- `build_model_popup_empty_is_none`。
- `build_model_popup_unknown_current_defaults_zero`:current 不在列表 → `selected==0`。

## 停机

单轮;连续 2 轮验收不过 → 报告。价值门禁不适用(用户明确需求)。
