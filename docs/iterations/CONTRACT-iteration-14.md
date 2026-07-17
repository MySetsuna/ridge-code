# CONTRACT —— Iteration 14:约束守卫(防奖励黑客,补齐护栏套件)

> ✅ **全部已交付**:P0 约束守卫(iter-17,`is_protected_path`+写/shell 臂 + `HaltReason::ConstraintBreach`);**iter-18 补齐**:①`edit_file`/`apply_edits` 臂约束守卫(`constraint_guard_edit`:受保护路径「非空替换为空」= 删测试代码 → 拦,闭合 iter-17 诚实边界的编辑清空缺口);②P1 `ContextRot`(压缩后仍超 2× 阈值 = 单条巨消息压不掉 → 诊断标签);③P1 `CircuitBroken`(连错达 `MAX_ERR_STREAK=5` 熔断,`must_stop` 早停,兜「错误每轮不同 stall 不触发」)。`cargo test --workspace` 全绿(agent lib 60→**62**)。**残留诚实边界**:词法/路径级守卫非真沙箱;改断言等**内容级语义**篡改仍未判(YAGNI:属深度问题);保护路径尚不可 config 配(默认 `tests`/`.git`)。详见 `docs/LOG.md` iter-18 条。

- **开工时间戳**: 2026-07-17(待下轮执行)
- **里程碑**: iter-13 落地标准存储库(`.ridge/runs/<id>/`)+ 显式停机原因(`HaltReason`);本轮补护栏套件缺的「防奖励黑客」一环
- **依据**: `docs/iterations/2026-07-17-notebooklm-guidance-13.md`(NotebookLM + 对抗评审:**驳回**其「子agent并行」P0,升「约束守卫」为 P0)

## 目标(End State)

让**奖励黑客**(reward hacking:删/清空失败测试以伪造 CI 绿——loop-engineering 头号失败模式)在自主循环里被确定性拦下。护栏套件(jail/denylist/read-only/HaltReason)补上「保护路径」这一环:现状 cwd 内 `rm tests/foo.rs` 仍放行,是伪造成功的经典缺口。

## 任务与验收信号(可自主做、离线可测、无计时抖动)

| 优先级 | 任务 | 确定性验收信号 | 状态 |
|---|---|---|---|
| **P0** | **约束守卫 `ConstraintBreach`**:保护路径(默认 `tests/`;可经 config/CLI 配)禁**删除/清空**。守卫接进写/shell 执行臂(复用 iter-5 `jail()` 的接入点),命中回 `BLOCKED (constraint)`;`HaltReason` 加 `ConstraintBreach` 变体,`halt_reason` 据此判定 | 单测:①构造删/清空 `tests/` 下文件的工具调用 → 守卫返 BLOCKED 且文件**物理无损**;②守卫命中的终态 `halt_reason` = `constraint_breach` 且 `!is_success()`;③保护路径外的正常写不被误伤 | ⬜ |
| **P1**(可选,廉价则做) | `ContextRot` HaltReason:`to_messages` 压缩后 `est_tokens` 仍 > 硬上限(如 2× `AUTO_COMPACT_TOKENS`)→ 判定 `context_rot` | 单测:构造压缩后仍超硬上限的 history → `halt_reason` = `context_rot` | ⬜ |
| **P1**(可选) | `CircuitBroken` HaltReason:连续 provider/工具失败达阈值(如 5)→ 熔断停机,防无人值守耗预算 | 单测:连续 N 次工具失败 → `halt_reason` = `circuit_broken`,停机早于回合上限 | ⬜ |

## 押后(经对抗评审,不在本轮)

- **子智能体并行编排**:性能上限非刚需;引擎 BSP 已并发 fan-out;来源自相矛盾(为单线程编排背书);NotebookLM 给的 sleep 计时验收易抖,违「确定性」本旨。仍作 backlog。
- **signals/ 复利机制**:当前无 signal 产者,建了即空脚手架(YAGNI);iter-13 已留升级注释。有产者再做。
- **重量级沙箱(Docker/gVisor)/ rmcp 替换**:需用户环境/技术选型决策,标已知限制。

## 边界

不破坏现有 53(agent)+ 全工作区测试 + clippy/fmt 干净;守卫是**词法/路径级**判定(纯函数、可单测),不引全局可变态;密钥不入任何留痕;保护路径默认 `tests/`,用户可配但不可关到「零保护」以外的危险默认。约束守卫是**深度防御**,与 jail(写限 cwd)、denylist(危险 shell)、read-only 正交叠加。

## 交付状态

> iter-13 起标准存储库 + 停机原因已就位;本轮补「防奖励黑客」后,轻量护栏套件对**无人值守自主循环**的四类经典失败(无进展/超预算/上下文腐烂/奖励黑客)将都有确定性停机信号。
