# CONTRACT · iteration-29 —— 多 Provider 管理(opencode 式核心)

> maker = 用户明确需求原文(2026-07-18);checker = 本文对抗评审(正确性门禁)。NLM 认证过期,本轮本地推进,复认证后回写 notes/来源。

## 背景与缺口(代码索引已核实)

现状**已有**:`ProviderProfile{name,kind,model,base_url,key_env,api_key}`、`Config.providers: Vec<ProviderProfile>`、`config_add_provider(text,profile)->Result<String>`(纯函数,增/同名覆盖,`api_key` 因 `skip_serializing` 永不落盘)、`SwapProvider` 热切换、`make_provider(kind,model,base_url,key)`、`/model [name]`、`/provider [list|use <name>]`、`HttpClient{post_json}`(带 header)、`WebFetch{get_text}`(无 header)。

**缺口**(用户 P0):① 无「实时抓某 provider 的模型列表 + context size」;② 无程序内自建 provider(`/provider` 提示「请去 config.json 手改」);③ 模型/provider 切换只有裸命令,无列表可视。

## 目标(P0)

1. **实时模型列表 + context size**:向当前 provider 的 `{base_url}/models` 发鉴权 GET,解析出模型 id 与(若端点提供)上下文窗口大小。命令 `/models` 列出。
2. **程序内自建 provider**:`/provider add <name> <kind> <model> <base_url> [key_env]` 接线既有 `config_add_provider` 并写盘。
3. **保留并打通切换**:`/provider use <name>`、`/model <name>` 既有能力不回退。

## 设计(注入式 = 可确定性测;网络走替身)

- `provider` crate 新增:
  - `pub struct ModelInfo { pub id: String, pub context: Option<u64> }`
  - `pub fn parse_model_list(v: &serde_json::Value) -> Vec<ModelInfo>` —— **纯函数**。兼容:OpenAI `{"data":[{"id":..}]}`、OpenRouter `{"data":[{"id":..,"context_length":N}]}`、Anthropic `{"data":[{"id":..}]}`(context 缺省 None)、以及顶层直接是数组。坏/空 JSON → `vec![]`(优雅降级,不 panic)。context 键容错:依次探 `context_length`/`context_window`/`max_context_length`/嵌套 `top_provider.context_length`。
  - `HttpClient` trait 补 `async fn get_json(&self, url, headers) -> Result<Value>`,**默认 `Err("unsupported")`**(既有测试替身零改动),仅 `ReqwestClient` 真实现(reqwest GET)。
  - `pub async fn fetch_models(http: &dyn HttpClient, kind, base_url, key) -> Result<Vec<ModelInfo>>` —— 按 kind 造鉴权 header(openai: `Authorization: Bearer <key>`;anthropic: `x-api-key`+`anthropic-version`),GET `{base_url}/models`,过 `parse_model_list`。
- `agent`(tui.rs):`run_command` 改 `async fn`(唯一调用点在 async 事件循环,`.await` 即可);新增分支:
  - `/models`:构 `ReqwestClient`,取 key(`meta` 的当前 provider 依 `Config`/env 解析),`fetch_models(...).await`(裹 `tokio::time::timeout` 防挂,超时/错 → `ui.note` 报错),列出 `id  (ctx N)` / `id`,标注当前 model。
  - `/provider add ...`:纯函数 `parse_provider_add(args) -> Result<ProviderProfile,String>` 解析定位参数 → `config_add_provider(read(config), &profile)` → 写盘 → `ui.note`。

## 边界(明确不做)

- **不做** 模型选择器浮窗(↑↓ 选实时模型)—— 留作 iter-30/后续快随;本轮先把数据/抓取/自建打通,浮窗是其上薄层。
- **不做** 付费聚合目录(models.dev 式静态库);context size **只取端点自报**,无则显示 `ctx ?` 并允许 config 覆盖(`ProviderProfile` 已可加字段,本轮不加)。
- **不改** 密钥存储策略:`api_key` 明文永不因 `/provider add` 落盘(既有 `skip_serializing` 保证);`/provider add` 只写 `key_env` 指向。
- **不动** 引擎层 / BoN / 其余 UI 需求(光标 wcwidth、状态双栏归 iter-30/31)。

## 确定性验收信号(编译器/测试/退出码可判定,零计时/网络)

门禁:`cargo test --workspace && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` 全 **exit 0**。新增测试:

1. `parse_model_list_openai`:OpenAI 形状 → id 有序、context=None。
2. `parse_model_list_openrouter_context`:含 `context_length` → context=Some(N);嵌套 `top_provider.context_length` 亦取到。
3. `parse_model_list_malformed_is_empty`:非对象/空串/缺 data → `vec![]`(不 panic)。
4. `fetch_models_via_stub_http`:注入 StubHttp 返回预设 models JSON → `fetch_models` 得预期 `Vec<ModelInfo>`(复用既有 `StubHttp`/`openai_provider_full_path_with_stub_http` 先例,零网络)。
5. `parse_provider_add_ok_and_bad`:合法定位参数 → 预期 `ProviderProfile`;缺参/未知 kind → `Err`。
6. `config_add_provider` 经 `parse_provider_add` 产物往返 → providers 数组含该档、`api_key` 键不出现(明文不落盘)。

## 预算与停机

单轮实现;同一 contract 连续 2 轮验收不过 → 停下报告。网络类 runtime 行为(真 `/models` 抓取)**不进测试**,仅纯函数 + 替身覆盖。
