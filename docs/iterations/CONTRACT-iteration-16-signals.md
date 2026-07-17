# CONTRACT —— Iteration 16:信号复利闭环(扬长优选 C · 证据最硬)

- **开工时间戳**: 2026-07-17(待用户确认方向后执行)
- **依据**: `docs/iterations/2026-07-17-iteration-15-hardening.md` §2 研判 —— NotebookLM 深研 + 51 源裁决:**C 多 loop 共享大脑(signals 复利)= 证据强/适配高/唯一优选**;A 有余力再做、B 判炒作押后。
- **里程碑**: iter-13 立标准存储库(`.ridge/runs`)物理底座;本轮在其上建**「产者→消费者」信号闭环**,兑现 iter-13 留的升级路径,把 agent 从「孤立脚本」升为「跨会话/跨 loop 复利的系统」。

## 目标(End State)

一个 loop 探测到的事实(摩擦点/发现/待办),经结构化 `signal` 落盘;**下一个 loop 启动时自动继承**之,无需用户重述——解决 agent「每会话冷启动、重新学项目」的根本损耗。这是「共享大脑」的心脏,非空脚手架。

## 任务与验收信号(离线可测、内核适配、无计时抖动)

| 优先级 | 任务 | 确定性验收信号 |
|---|---|---|
| **P0** | **产者**:内置工具 `signal_write`(agent 探测到事实时调用),把 signal 落 `.ridge/signals/<slug>.md`,带 frontmatter 最小 schema:`id / type / status(open) / source(run_id) / body` | 单测:调用 → 文件物理生成、frontmatter 解析出 5 字段、status=open;slug 冲突不覆盖(追加序号) |
| **P0** | **消费者**:run 启动时扫 `.ridge/signals/` 取 `status=open`,编成**有界**信号块注入上下文(复用 iter-4 `durable_state_block` 的注入接缝与「末尾 role=system、体量有界」纪律),trace 可见其为推理依据 | 单测:N 个 open signal → 注入块含之且**字符有界**(超阈值截断,防上下文膨胀);无 open signal 则不注入 |
| **P1** | **闭环/消解**:signal 被处理后置 `status=resolved`(或移 `archive/`),避免下轮重复消费 | 单测:resolved signal 不再被消费者扫入;整个产→消→解流程退出码 0 |

## 设计约束(经开放问题预答)

- **位置**:`.ridge/signals/`(**项目级**,跨 run 共享 → 复利);溯源靠 signal 的 `source=run_id` 字段回指 `.ridge/runs/<id>/`。run 级产出 + 项目级索引二者兼得。
- **schema 最小**:`id/type/status/source/body` 五字段起步,不臃肿;不做 timeline/前端元数据等未被用到的字段(YAGNI)。
- **与 Durable State 共存**:signal 块与 `modified_files`/`last_error` 事实块**同接缝、分节注入**,各自有界,合计仍 O(去重项),不随步数膨胀。
- **不做**:自动 signal 抽取器(从 trace 用 LLM 提炼 signal)—— 先用显式 `signal_write` 工具(确定性、可单测);LLM 抽取属后续增强。触发器(Cron/Webhook)驱动多 loop 自动轮转 —— 另轨(需 daemon),本轮只做文件系统层的产→消→解。

## 押后(经证据研判,不在本轮)

- **A 时间旅行/分支·多路线并行**:证据中、单用户多属 token 税炫技;价值主在崩溃恢复(已实现)。有余力或遇「多路径探索」真需求再做。
- **B 自改进 harness/hill-climbing**:证据弱/炒作,单用户缺轨迹样本量,押后。
- **约束守卫 `ConstraintBreach`**(原 CONTRACT-14):仍是有效**安全**backlog,与 signals 正交;可在 signals 后接续,防奖励黑客删测试。

## 边界

不破坏现有 54(agent)+ 全工作区测试 + clippy/fmt 净;signal 落盘走 cwd `.ridge/`(与写操作 jail 边界一致);密钥不入 signal;注入块必须有界(硬性字符上限),防上下文膨胀反噬 token 节约成果。

## 交付状态

> ✅ **已交付(2026-07-17)**。产→消→解全闭环落地:`signal_write` 产者(内容哈希 id 幂等去重)+ `load_signal_block` 消费者(有界注入,复用 `durable_state_block` 接缝)+ `signal_resolve` 消解。`cargo test --workspace` 全绿(agent 54→58,+4 测),clippy/fmt 净。标准存储库不再只是「审计留痕」,而成**可被下一 loop 消费的活知识层**——研判中证据最硬、最契合本架构的差异化长板。P1 resolve 亦随 P0 一并落地。见 `docs/LOG.md` iter-16 条。
