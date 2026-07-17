# CONTRACT —— Iteration 20:自动 signal 抽取器(复利环产者的「发现/待办」侧)

- **开工时间戳**: 2026-07-17
- **依据**: `docs/iterations/2026-07-17-notebooklm-guidance-18.md` —— NotebookLM 荐「自动 signal 抽取器(hill-climbing Level 4)」;对抗评审采纳其**安全内核版**(喂已建 signals 复利环),**驳回**其暗含的「自动改写 harness」(单用户样本不足、改写无 checker,维持 iter-15 判定)。用户拍板「开工」。
- **里程碑**: iter-17 自动产者只记**失败**;本轮补另一半 —— run 收尾把执行轨迹提炼成**发现/摩擦/待办**信号,喂已通的产→消→解闭环,兑现 iter-16/17 留的「自动产者发现/待办侧」升级路径。

## 目标(End State)

一个 loop 跑完,其轨迹里的可复用知识(项目事实/踩坑/未竟)被**自动**提炼成结构化 signal 落 `.ridge/signals`,下个 loop 自动继承 —— 不再仅靠 run 中模型主动调 `signal_write`(常漏),也不再仅记失败。**opt-in**(env,默认关):尊重 token 北极星,不默认给每轮加一次 LLM 成本。

## 任务与验收信号(离线可测、内核适配、无计时抖动)

| 优先级 | 任务 | 确定性验收信号 |
|---|---|---|
| **P0** | **纯解析** `parse_extracted_signals(text)`:每行 `kind: body`(中英冒号皆认),kind ∈ {discovery,friction,todo},body 非空;NONE/不合规/项目符号容错忽略;上限 `MAX_EXTRACTED_SIGNALS=5` | 单测:混合输入(NONE + 合规 + 越集 kind + 空 body + 无冒号 + 第 6 条)→ 恰取前 5 条合规、kind 全在允许集 |
| **P0** | **抽取门** `run_has_substance` + `signal_extract_request`:无实质轨迹(未动工具且未改文件)→ 不抽(省 LLM 调用);有 act/改文件 → 构造有界请求(轨迹复用 `bound_observation`) | 单测:空 state → `run_has_substance` false 且 request None;含 `act:` 消息 → true 且 request Some |
| **P0** | **端到端** `extract_signals_from_run(provider, out, dir, source)`:调 provider 一次 → 解析 → `signal_create` 落盘(内容哈希幂等去重);best-effort(失败/无所得/无轨迹 → 空,不掀翻主流程) | 单测(`ScriptedProvider` 回 canned):落 N 条 open 信号、source 正确;同输入再抽 → id 一致、不新增(幂等) |
| **P1** | **接线 + 门控**:`signal_extract_enabled()`(env `RIDGE_EXTRACT_SIGNALS`,默认关)守;`run_once`/`headless` 建 app 前留一把 provider,收尾 `maybe_extract_signals` 复用 run 的 source id;`--help` 列 env | 编译通过 + `--help` 实证含 `RIDGE_EXTRACT_SIGNALS`;全工作区测试/clippy/fmt 净 |

## 押后(经对抗评审,不在本轮)

- **自动改写 harness/hill-climbing 改配置**:维持 iter-15/18 驳回 —— 单用户轨迹样本不足;改写自身无独立 checker(maker≠checker 在 harness 层未解)。本轮只做「喂已有 signals 环」的安全内核。
- **LLM 抽取质量调优 / few-shot / 结构化 JSON 输出**:先用最简 `kind: body` 行格式(确定性可解析);质量不足再迭代 prompt。
- **成功侧 durable 事实自动沉淀**(如把 `modified_files` 也编成 discovery):YAGNI,manifest 已留痕;有需求再做。
- **触发器编排多 loop(Webhook)/ 真沙箱**:另轨,见 guidance-18。

## 边界

不破坏现有 63(agent)+ 全工作区测试 + clippy/fmt 净;抽取是 **best-effort 旁路**,任何失败(provider 错/解析空/无轨迹)静默返空,**绝不影响主流程退出码**;**opt-in 默认关**,不默认加 token 成本(与 token 北极星一致);抽取的 provider 调用有界(轨迹经 `bound_observation` 截断);signal 经 `signal_create` 内容哈希**幂等去重**,反复同一发现不刷屏;密钥不入 signal;落盘走 cwd `.ridge/`(合 jail 边界)。

## 交付状态

> ✅ **已交付(2026-07-17)**。`parse_extracted_signals`(纯,可测)+ `run_has_substance`/`signal_extract_request`(抽取门)+ `extract_signals_from_run`(端到端,幂等)+ `signal_extract_enabled`(opt-in env)+ `run_once`/`headless` 接线(`maybe_extract_signals` 复用 run source id)。`cargo test --workspace` 全绿(agent lib 63→**66**,+3),clippy `-D warnings`、fmt 净;`--help` 实证含 `RIDGE_EXTRACT_SIGNALS`。复利环产者从「仅记失败」升为「失败 + 发现/摩擦/待办」双侧。见 `docs/LOG.md` iter-20 条。
