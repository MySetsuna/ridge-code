# CONTRACT · iteration-37 —— 内置主流供应商 preset 表 + `login` 一键接入 + auth.json 密钥库

> maker = 用户需求「业界顶级 + 中国顶级 + 知名聚合供应商优先内置;用户只需输入 API Key 或 `ridgecode login` 接入」。checker = 本文正确性门禁。价值门禁不适用(用户明确需求)。

## 缺口(代码索引核实)

- **无内置供应商清单**:接一个新 provider 要用户手敲 `/provider add <name> <kind> <model> <base_url> [key_env]`(`parse_provider_add`),记不住 base_url/kind,门槛高。opencode 式「选供应商→填 key 即用」缺失。
- **密钥无持久库**:key 只有两条路——`RIDGE_API_KEY`/`key_env` 环境变量(需用户自己设 env,非「在工具里输入」),或 config.json 顶层/档案内联 `api_key`(而工具写 config 受 `skip_serializing` 铁律禁止写明文)。**「输入 key 即接入且下次仍在」无落点**。
- **多密钥并存缺失**:登录 deepseek + kimi 各一把 key 并自由 `/model` 切换,现在只能靠多个 env 变量,工具不管理。

## 目标

1. **内置 preset 表**(编进二进制,纯数据):世界顶级 `openai / anthropic / gemini / grok`;中国顶级 `glm / kimi / deepseek / qwen / hunyuan / minimax`;聚合 `openrouter / siliconflow / together / groq`。每条 = `id, label, kind(openai|anthropic), base_url, default_model, key_env`。
2. **`~/.ridge/auth.json` 密钥库**:`login` 把 key 写这里(**不进 config.json**,铁律保持),按 `key_env` 名索引;权限尽量收紧(unix 0600)。key 解析新增一档:内联 `api_key` > env[key_env] > **auth.json[key_env]**。
3. **`ridgecode login` 子命令**:
   - `login --list` → 打印 preset 表(id / label / model / base_url)。
   - `login <preset> [KEY] [--model M] [--name N] [--default]` → 据 preset 造 `ProviderProfile` 写入 config `providers[]`(经既有 `config_add_provider`,**不含 key**)+ key 存 auth.json。缺 KEY 参数则从 stdin 读一行(避免留在 shell 历史/argv)。`--default` 额外把顶层 `provider/model/base_url/key_env` 指向该 preset,使下次启动即以它为默认。
4. **TUI `/login <preset> [key]`**:复用同一纯核,交互内即可接入;`/login` 无参列出 preset。
5. **接缝(不落地)**:auth.json 值为字符串(API key);未来 OAuth 档可扩成对象 `{type:"oauth",...}`,`login` dispatch 预留 preset-specific 分支。本轮不实现任何 OAuth。

## 设计(最小面)

- **lib.rs 纯核(全部可单测)**:
  - `struct ProviderPreset { id, label, kind, base_url, default_model, key_env }`(`&'static str`)。
  - `PROVIDER_PRESETS: &[ProviderPreset]`(14 条)、`preset_by_id(id)->Option<&'static ProviderPreset>`、`preset_to_profile(&preset, name:Option<&str>, model:Option<&str>)->ProviderProfile`(api_key=None,key_env=preset.key_env)。
  - 密钥库纯函数:`auth_parse(text)->BTreeMap<String,String>`(坏/空→空表)、`auth_upsert(text, key_env, key)->String`(保留余键,同名覆盖,pretty JSON)、`auth_get(text, key_env)->Option<String>`。
  - `resolve_key_env(name, auth:&BTreeMap)->Option<String>` = env[name].非空 或 auth[name];`ProviderProfile::resolve_key_with(&auth)` = 内联 api_key > `resolve_key_env(key_env, auth)`。
  - `apply_login(config_text, &preset, name:Option, model:Option, make_default:bool)->Result<String,String>`:纯字符串变换 —— 加/覆盖 `providers[]` 档 +(make_default 时)set 顶层 `provider/model/base_url/key_env`。**产物绝不含 key**。
  - Config 顶层新增可选 `key_env: Option<String>`(与档案对称;供 `--default` 从 auth 取顶层 key)。
- **main.rs**:`run_login(args)`(在 `handle_meta_flags` 后、主流程前 dispatch;读写 config+auth,打印后续指引,key 绝不回显);`auth_path()`=`~/.ridge/auth.json`;`load_auth()`/`save_auth`(0600);`real_provider` 顶层新增第 3 档 `cfg.key_env`→auth 解析,`providers[]` 迭代改 auth-aware;`build_agents` 同。
- **tui.rs**:`/login` 命令走同一纯核 + `load_auth`;`switch_provider`/`current_api_key` 改 auth-aware(缺 env 时回落 auth.json)。
- **不改**:引擎、maker/checker、排队/越狱、交互页框架(Provider 页可后续补 preset 区,本轮不做)。

## 边界(不做)

- **OAuth 订阅登录**(Claude Pro/Max、ChatGPT 订阅等):只留接缝,不实现;逐家 ToS/device-flow 待后续专轮。
- Provider 交互页的 preset 选单 UI 美化 —— 后续。
- 运行时探活/model 列表拉取(已有 `/models` 抓取,不在本轮扩)。
- preset 的 `default_model` 字符串精确性非门禁项(base_url/kind/key_env 才是接入正确性关键;model 用户可随时改)。

## 确定性验收信号

门禁 `cargo test --workspace && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` 全 **exit 0**。新增测试:

- `provider_presets_wellformed`:每条 id/label/base_url/default_model/key_env 非空;`kind ∈ {openai,anthropic}`;id 唯一;`base_url` 以 `https://` 起;条数 ≥ 14 且**含** `openai/anthropic/gemini/grok/glm/kimi/deepseek/qwen/openrouter/siliconflow/groq` 全部 id。
- `preset_by_id_roundtrip`:`preset_by_id("deepseek")` base_url 含 `deepseek.com`;`preset_by_id("nope")==None`。
- `preset_to_profile_maps_fields`:deepseek preset→profile:kind/base_url/model/key_env 对齐,`api_key==None`,name 默认=id、可被 override。
- `auth_store_roundtrip`:`auth_upsert("{}","DEEPSEEK_API_KEY","sk-x")` 产物 `auth_get==Some("sk-x")`;再 upsert 另一 key 保留前者;坏文本从空起;**产物是合法 JSON**。
- `resolve_key_precedence_with_auth`:内联 api_key 优先;无内联时 env[key_env] 优先于 auth[key_env];皆无→None(不触碰全局 env:用显式表参数)。
- `apply_login_writes_profile_no_key`:`apply_login("{}", deepseek, None, None, true)` 产物:`providers[]` 含 deepseek 档(kind/base_url/model/key_env 对);顶层 `provider/model/base_url/key_env` = deepseek;**全文不含任何 key 字面量**;是合法 JSON;`make_default=false` 时顶层不被改。

## 预算与停机

单轮;同一 contract 连续 2 轮验收不过 → 停下报告。收尾:回写 `docs/ARCHITECTURE.md`(preset 表 + auth.json 密钥库 + 新 key 解析档)、写迭代报告、提交带 `iter-37`、替换 NLM 架构来源。
