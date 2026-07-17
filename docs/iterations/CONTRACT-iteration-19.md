# CONTRACT —— Iteration 19:巨型工具输出确定性截断(context_rot 根因修法)

- **开工时间戳**: 2026-07-17
- **依据**: `docs/iterations/2026-07-17-notebooklm-guidance-18.md` —— NotebookLM 荐「截断代理」+ 对抗评审:**采纳其确定性内核版为 P0**(证据最硬 [claw-tsaver 实测省 99.1%]、context_rot 根因、契合 token 北极星);**驳回**其 Saga 自动回滚(误套分布式语境 + 数据丢失陷阱 + 违 preserve-mistakes)。
- **里程碑**: iter-18 给 context_rot 立**终态诊断标签**;本轮在**产出接缝**处做根因修法 —— 巨型工具观察入 history 前确定性截断,既止住上下文膨胀,又零丢数据(存盘可重取)。

## 目标(End State)

任一工具观察(`read_file`/`search`/`run_shell`/MCP 结果)超字符上限时,入 `history` 前被**确定性截断**为 head+tail 预览 + 截断标记(注明字符数 + 提示可 `read_file` 区间重取)。上下文不再被单条巨型输出撑爆(补齐 `compact_history` 只压「多条旧消息」、压不掉「单条近消息」的缺口),而文件真相源物理无损、可再读。

## 任务与验收信号(离线可测、内核适配、无计时抖动)

| 优先级 | 任务 | 确定性验收信号 |
|---|---|---|
| **P0** | **确定性截断** `bound_observation(obs) -> String`:超 `OBS_CHAR_CAP`(如 8000)则保留 head(前 N)+ tail(后 M)+ 中缝截断标记;接进 act 循环 `obs` 定稿处(所有工具路径汇流的单一接缝)。截断标记**不含** `error`/`failed`/`exit`/`BLOCKED`/`permission` 等判据词(免污染 verify/durable 信号) | 单测:①超上限输入 → 输出 ≤ 上限+标记长度、含 head 与 tail 片段、含「截断」标记;②未超上限输入 → 原样返回(逐字节相等);③`"exit 0: " + 巨型正文` 截断后 `tool_output_ok` 仍 true(head 保 `exit 0:` 前缀);④截断标记不含判据词(`is_error_observation` 不被标记误触发) |
| **P1**(顺带,廉价则做) | 截断与 stall/durable 兼容性回归:截断后相同输入仍判 stall;截断后错误输出(`exit 7:...`)`tool_output_failed` 仍 true | 单测:截断后 stall 检测、tool_output_failed 各断言通过 |

## 押后(经对抗评审,不在本轮)

- **Saga 自动回滚(NotebookLM P0)**:**驳回** —— 分布式语境误套单用户本地 CLI;自动 `git checkout .` 毁用户未提交改动 + 失败现场(违 preserve-mistakes);重造 git;合理内核(记录改动文件)已由 durable state/manifest 实现。
- **自动 signal 抽取器**:需 LLM 摘要 pass(内容非确定性、难单测),且「自动改写 harness」维持 iter-15 驳回。俟本轮后议。
- **WASM/真沙箱**:重量级 + 需环境决策 + WASM 跑不了原生 shell;归 MCP(sandbox-exec)符内核铁律。
- **LLM 摘要式截断**:外置可装能力,走 MCP(如 claw-tsaver),不进内核。
- **可配保护路径**:小优化,除非顺带不单独立项。

## 边界

不破坏现有 62(agent)+ 全工作区测试 + clippy/fmt 净;截断是**纯函数**(输入 obs 字符串 → 输出 bounded 字符串),无 IO、无全局态、可单测;**零丢数据**(截断只影响入 history 的副本,磁盘文件不动,`read_file` offset/limit 可重取细节);上限取值须足够宽(head+tail 合计 ≥ 典型工具输出)使**仅病态巨型输出**被截,常规输出零影响;截断标记用 CJK 措辞避开 verify/durable 判据词。截断与 `compact_history`(压多条旧消息)正交叠加,同属**内核上下文卫生**,非外置摘要能力。

## 交付状态

> ✅ **已交付(2026-07-17)**。`bound_observation`(纯函数,`OBS_CHAR_CAP=8000`,head+tail 各 4000 + 中缝截断标记)接进 act 循环 `obs` 定稿处(所有工具路径单一汇流接缝)。截断标记用 CJK 措辞避开 verify/durable 判据词。`cargo test --workspace` 全绿(agent lib 62→**63**,+1:`bound_observation_truncates_giant_but_preserves_signals` 验有界/保 head-tail/成功失败信号存活/确定性),clippy `-D warnings`、fmt 净。内核对「上下文膨胀」现有**三层确定性防御**:单条巨输出即时截断(本轮,根因,零丢数据可 read_file 重取)+ 多条历史累积压缩(`compact_history`,已有)+ 压不动残留终态判 `context_rot`(iter-18,后备)。见 `docs/LOG.md` iter-19 条。
