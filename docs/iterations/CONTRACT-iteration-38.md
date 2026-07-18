# CONTRACT · iteration-38 —— 真正接入内置供应商:交互式登录页 + 连接校验

> maker = 用户需求「下一个迭代要真正接入内置供应商,并且可以登录」→ 澄清为「交互登录页 + 连接校验」。checker = 本文正确性门禁。价值门禁不适用(用户明确需求)。

## 缺口(代码索引核实 + 隔离实测)

- iter-37 登录**机制无坑**(隔离实测:config 得档+顶层身份+key_env、auth.json 存 key、无明文进 config、能据此启动)。但:
  - **盲存不校验**:`login`/`/login` 存 key 后不验证 key 是否真能连通,错 key 也「成功」,用户不知接没接上 —— 不是「真正接入」。
  - **登录不引导**:仅 `login <id> <key>` / `/login <id> <key>` 一行式,无「选家 → 输入 key → 验证 → 激活」的交互流。

## 目标

1. **连接校验**:登录时打 `{base_url}/models` 鉴权 GET(`provider::models::fetch_models`,15s 超时)验证 key 真连通。`get_json` 非 2xx 返 Err(已核),故**错 key/坏端点 → 明确失败,不落盘**;连通 → 存 + 激活 + 报 `✓ connected (N models)`。CLI `login` 默认校验,`--no-verify` 可跳过(离线配)。
2. **交互登录页(TUI)**:`/login`(无参)开供应商选单 Panel(14 家 preset,可搜索)→ ↑↓ 选一家 → Enter 进就地输入 key(**掩码显示**)→ Enter 校验中… → 连通则写 auth+config、热切、`✓ connected · now active`、关页;失败则红字错误、留在输入态可重试。保留 `/login <id> <key>` 快捷路径(同样校验)。

## 设计(最小面,复用现成件)

- **校验核**(`main.rs`):`verify_key_via(&dyn HttpClient, kind, base_url, key) -> Result<usize,String>`(纯经 `fetch_models`,Ok→模型数、Err→原因;**HttpClient 接缝可离线测**)+ `verify_provider_key(kind, base_url, key)`(真 `ReqwestClient` + 15s `timeout` 包壳)。
- **共享落地核**(`tui.rs`):`async login_apply_verified(preset, key, meta, swap, ui)` —— 校验 → 成功走 `tui_login`(写 auth+config)+ `swap.swap` 热切 + note ✓ + 关页;失败 note ✗。CLI `run_login`(改 `async`,`main` 用 `.await`)与 TUI 两路共用校验语义。
- **登录页**(`tui.rs`):`PanelKind::Login`(派生 `PartialEq`)+ `login_panel()`(rows=14 preset,key=id、value=`label · model`);`panel_enter` 加 `(Login,None)` → 起编辑(editing=Some(""),标题改「Enter API key for <id>」);主环 `Enter` 分支:`Login`+`editing.is_some()` → `login_apply_verified(...).await`(唯一异步分支,余仍 `panel_enter`);`draw_panel` 对 `Login`+editing **掩码**输入(`•×len`)。`/login` 无参开页,`SLASH_COMMANDS`/`/help` 已含。
- **不改**:引擎、preset 表 / auth 库 / apply_login(iter-37 已测)、其它 Panel、排队/越狱。
- **ponytail**:校验期 UI 短暂阻塞在 `.await`(有效 key 通常 <2s;15s 仅超时上限)—— 不引入后台校验任务通道(YAGNI),留注释;需要非阻塞再升级。

## 边界(不做)

- 用户自定义命令 / Skills 显示为命令 / Hook 系统 —— **顺延 iter-39**(用户已选:Prompt 模板命令 + Skills-as-命令为一轮,四个内置 Hook 为一轮)。
- OAuth 订阅登录 —— 仍只留接缝。
- 校验用 `/models`:个别不支持 `/models` 的端点会误判失败 → 用 `--no-verify` 兜底(文档写明)。

## 确定性验收信号

门禁 `cargo test --workspace && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` 全 **exit 0**。新增测试:
- `verify_key_via_maps_result`(async,stub HttpClient 零网络):`{"data":[{"id":"m1"},{"id":"m2"}]}` → `Ok(2)`;stub 返 Err → `Err`。
- `login_panel_lists_all_presets`:`login_panel().rows.len()==PROVIDER_PRESETS.len()`,每行 key 是合法 preset id,`kind==PanelKind::Login`。
- 既有测试(apply_login/auth/preset/slash_popup 等)保持绿。

## 停机

单轮;同一 contract 连续 2 轮验收不过 → 停下报告。收尾:回写 `docs/ARCHITECTURE.md`、写迭代报告、提交带 `iter-38`、rebuild+install、替换 NLM 架构来源。
