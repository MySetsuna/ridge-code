# provider 注册表 + N 模型路由 + 原生 Anthropic — 设计文档

> 状态:待实现(2026-07-05)。
> 配套:provider 边界原则见 `PLAN.md §6`、`HANDOFF.md §5`;现状见 `CLAUDE.md`。

---

## 1. 背景与目标

现状:config 只有强/弱两段(`[strong]`/`[weak]`,或单 `[provider]`),各自能指向不同的 OpenAI 兼容端点——
即「跨供应商」已成立,但**模型是固定两档、只能用两个**。

本设计把 provider 从「两段内联」升级为**命名注册表**:声明任意 N 个命名 provider,再由角色/路由**按名引用**。
交付两个能力:①声明 N 个供应商/模型;②(可选)按子任务难度把 worker 路由到注册表里的**任意**模型(真正用上 >2 个模型)。
**Cost 仍按难度档位(强/弱 tier)记账**,eval 的「强模型 token 占比」语义不变。**旧配置零改动照跑。**

## 2. 范围

**做:**
- config 新增 `[[providers]]` 命名注册表(每项 `name` + `base_url` + `model` + `api_key`/`api_key_env` + 可选 `kind` / `max_tokens`)。
- config 新增 `[roles]`:`strong`/`weak` **按名**引用注册表里的 provider。
- config 新增可选 `[routing]`:`trivial`/`moderate`/`hard` **按名**把对应难度的 worker 覆盖到任意命名 provider。
- **原生 Anthropic provider**:`kind = "anthropic"` 走 Anthropic Messages API(`/messages`,`x-api-key` + `anthropic-version` 头,`tool_use`/`tool_result` 内容块归一化),让 strong=Claude 能直连原生、不必走兼容网关。
- `rc-core`:`Orchestrator::with_worker_models(map)` builder——按难度覆盖 worker provider;`work()` 命中则用覆盖,否则回落 strong/weak。
- **向后兼容**:`[strong]`/`[weak]`/`[provider]` 内联写法继续有效(注册表缺省时回落);`kind` 缺省 = `openai`(旧配置零改动)。
- 文档 + 示例 + 保持 build/test/clippy/fmt 全绿。

**不做(YAGNI):**
- Planner 直接给每个子任务点名模型(更细路由留待;本轮按「难度→模型」映射已够 N 模型)。
- 每个命名模型独立 USD 记账(Cost 仍强/弱两桶;定价与 tier 解耦已够 eval 用)。
- Anthropic 流式 / 扩展思考 / prompt caching(先非流式,与 OpenAI 实现对齐)。

## 3. 关键决策

| # | 决策 | 取舍理由 |
|---|---|---|
| 1 | provider 升级为**命名注册表** `[[providers]]` | 声明 N 个,角色/路由按名引用;是 N 模型的地基 |
| 2 | `[roles]` strong/weak **按名**引用 | 保留两档编排不变,只把「谁是强/弱」解耦成命名 |
| 3 | `[routing]` 按**难度**覆盖 worker 模型(可选) | 真正让 >2 个模型上场,且贴合现有 `Difficulty`,Planner 无需改 |
| 4 | **Cost 仍按难度 tier(强/弱)记账** | eval「强 token 占比」语义不变;命名模型是身份、tier 是成本分类,两者解耦 |
| 5 | `Orchestrator::new(strong,weak,..)` **签名不变** + `with_worker_models` builder | 加法式扩展;rc-eval/单测零改动,不破坏 M3 闭环 |
| 6 | 旧 `[strong]`/`[weak]`/`[provider]` 全兼容回落;`kind` 缺省 openai | 不逼用户改配置 |
| 7 | **新增 `AnthropicProvider`(第二个 `LlmProvider` 实现)** | 落地 PLAN §6「一个 Anthropic + 一个 OpenAI 兼容」;wire 归一化都在 provider 内部,上层 `rc-types` 内部表示不变 |
| 8 | Anthropic 非流式、`max_tokens` 可配(默认 8192) | 与现有 OpenAI 实现对齐;`max_tokens` 是 Anthropic 必填项 |

## 4. 配置形态

**新(推荐,多供应商):**
```toml
[[providers]]
name = "claude"
kind = "anthropic"                          # 原生 Anthropic Messages API
base_url = "https://api.anthropic.com/v1"
model = "claude-sonnet-4-6"
api_key_env = "ANTHROPIC_KEY"
# max_tokens = 8192                          # Anthropic 必填,缺省 8192

[[providers]]
name = "deepseek"                            # kind 缺省 = openai(兼容端点)
base_url = "https://api.deepseek.com/v1"
model = "deepseek-chat"
api_key_env = "DEEPSEEK_KEY"

[[providers]]
name = "qwen"
base_url = "https://dashscope.aliyuncs.com/compatible-mode/v1"
model = "qwen-plus"
api_key_env = "QWEN_KEY"

[roles]
strong = "claude"     # planner/reviewer/修复/基线 + 默认 hard worker
weak   = "deepseek"   # 默认 trivial/moderate worker

[routing]             # 可选:按难度把 worker 覆盖到任意命名 provider(用上第 3 个模型)
hard     = "claude"
moderate = "qwen"
trivial  = "deepseek"
```

**旧(仍支持):** `[strong]`+`[weak]`,或单 `[provider]`。

## 5. 解析逻辑(rc-cli)

1. 从 `[[providers]]` 建 `HashMap<name, ProviderConfig>`(无 name 的项告警跳过)。
2. **strong**:`[roles].strong` 有 → 查注册表(缺则报错指名);否则回落 `[strong]` → `[provider]`;都无 → 报错give 提示。
3. **weak**:`[roles].weak` 有 → 查注册表;否则回落 `[weak]` → `[provider]` → 复用 strong。
4. **worker 覆盖**:遍历 `[routing]` 的 trivial/moderate/hard,有名字则查注册表建 provider,塞进 `HashMap<Difficulty, Box<dyn LlmProvider>>`;非空则 `orch.with_worker_models(map)`。
5. 密钥解析 `resolve_api_key` 不变(段内 `api_key` → `api_key_env` → `RIDGE_API_KEY`);每个命名 provider 各自独立 key。

## 6. rc-core 改动

- `Difficulty` 加 `Hash`(作 HashMap 键)。
- `Orchestrator` 加字段 `worker_models: HashMap<Difficulty, Box<dyn LlmProvider>>`(`new` 默认空)。
- `pub fn with_worker_models(mut self, models) -> Self`。
- `work(&self, st, tier, cost)`:`worker_models.get(&st.difficulty)` 命中用它,否则 `provider_for(tier)`;**tier(成本档)仍 = route_tier(difficulty)**,与用哪个命名模型无关。
- 其余(planner/reviewer/repair/baseline 用 strong)不变。

## 6.5 原生 Anthropic wire 归一化(`rc-providers::anthropic`)

`AnthropicProvider` 实现同一个 `LlmProvider` trait,内部把 `rc-types` 内部表示 ↔ Anthropic Messages API 互转。归一化要点(OpenAI 与 Anthropic 的三处关键差异):

1. **system 是顶层参数**:内部 `Role::System` 消息**抽出**拼成请求体顶层 `system` 字符串,不进 `messages`。
2. **tool_result 是 user 消息里的块**:内部 `Role::Tool` 消息 → Anthropic `user` 消息,内容块 `{type:"tool_result", tool_use_id, content}`(OpenAI 是 role=tool + tool_call_id)。
3. **tool_use 用 input 对象**:内部 assistant 的 `tool_calls[].arguments`(JSON 字符串)→ Anthropic `{type:"tool_use", id, name, input}`(input 是 JSON 对象,需 parse);assistant 文本 + tool_use 同处一条消息的内容块数组。
4. **合并相邻同角色**:工具循环里一条 assistant(N 个 tool_use)后跟 N 条 tool_result → 会变成 N 条连续 user 消息;Anthropic 要求角色交替,故**把相邻同角色消息的内容块合并**成一条。
5. 请求:`POST {base_url}/messages`,头 `x-api-key` + `anthropic-version: 2023-06-01`;`max_tokens` 必填(默认 8192)。工具用 `input_schema`(不是 OpenAI 的 `parameters`)。
6. 响应:遍历 `content` 块,`text` → 文本、`tool_use` → `ToolCall{id,name,arguments=to_string(input)}`;`usage.{input_tokens,output_tokens}` → `Usage`。

wire 类型(Anthropic 私有)不外泄,纯翻译函数(system 抽取 / 消息合并 / 响应解析)离线单测。

## 7. 测试

- rc-core 现有单测保持绿(默认 worker_models 空 → 走原 strong/weak 路径,零回归)。
- 新增单测:`with_worker_models` 塞一个 Difficulty::Hard → StubProvider,`work` 一个 hard 子任务后,断言用的是覆盖 provider(Stub 脚本被消费)、且 Cost 仍记在 strong tier。
- Anthropic:纯翻译函数单测——system 抽取、tool_result → user 块 + 相邻合并、响应 tool_use → ToolCall。用构造的 `Message`/JSON,零联网。
- rc-cli 的解析是薄装配,靠手动/示例覆盖(与现有 config 解析一致,无单测)。

## 8. DoD

- 旧配置(`[strong]`/`[weak]`/`[provider]`)行为完全不变(零回归)。
- 新配置:`[[providers]]` + `[roles]` 能按名选强/弱;`[routing]` 能让第 3 个模型在对应难度上场,启动日志可见。
- `kind = "anthropic"` 能用原生 Anthropic API 跑工具循环(strong=Claude 直连,不必走兼容网关)。
- `cargo build/test/clippy(-D warnings)/fmt` 全绿。
