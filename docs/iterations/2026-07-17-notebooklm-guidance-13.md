# NotebookLM 指导归档 + 对抗评审 —— 针对 Iteration 13(即 Iteration 14 计划)

- **时间戳**: 2026-07-17
- **来源**: NotebookLM「手搓agent」笔记本,基于《RidgeCode 迭代报告 Iteration-13》(source `d5f8413a`)+ loop-engineering / 多智能体协作 notes
- **conversation_id**: `68791fb7-659a-4ad6-a86c-beb7ac694781`

## NotebookLM 给的 Iteration 14 排序

| 优先级 | 任务 | NotebookLM 理由 | 它给的验收信号 |
|---|---|---|---|
| P0 | **子智能体并行编排** | 消延迟瓶颈;Rust `tokio::spawn`/`join_all` 多核并发是超越 Python 版核心优势 | trace 里两并发子 agent `start_time` 差 <10ms 且总耗时 < 串行和(sleep 500ms → 总 500~600ms) |
| P1 | **signals/ 复利机制** | 信号是跨 loop 复利最小单元;没有它每个 run 只是孤立记录 | Run A 产 `signals/xxx.md` 被 Run B 启动时显式加载引用 |
| P2 | **约束/合同守卫(Constraint Guard)** | 防「奖励黑客」(删失败测试伪造成功);把不可违反约束写进合同、verifier 硬校验 | 构造 agent 删 `tests/` 文件 → verifier 检测文件变化且未授权 → 停机返 `ConstraintBreach` |

**缺失的 HaltReason**(NotebookLM 荐补,均可确定性判定):`ConstraintBreach`(违禁操作/删测试/改保护路径)、`CircuitBroken`(下游 provider 连续失败达阈值,防无人值守耗尽预算)、`ContextRot`(压缩后仍逼近上下文极限,主动停防推理静默劣化)。

## 对抗评审(不全信 NotebookLM)—— **驳回它的 P0 排序**

- ❌ **驳回「子智能体并行编排」当 P0**,四点理由:
  1. **同一批来源自相矛盾**:source [1] 明说「coordination is deterministic, single-threaded, and traceable. That is precisely the point」——它在**为单线程编排背书**;[2] 承认「pure orchestration 的延迟代价对 research-style 工作 fine,因为时间本就被模型调用主导」。并行主要救**延迟**,而 CLI 单用户场景延迟非瓶颈。
  2. **它是性能优化,非能力/正确性缺口**。本项目已多次(正确地)判为「串行够用,并行是性能上限非刚需」。引擎层 BSP 超步**早已并发** fan-out(iter-01 的 `tokio::spawn` + 同步点 reduce);串行的只是 `dispatch_agent`/`run_planned` 这层应用编排。
  3. **它给的验收信号是 sleep 计时测试**(500ms→总<600ms)——计时断言在 CI 天生**易抖**,是反模式,违背「确定性验收」本旨。其自己也承认真正的证明在 reducer/BSP 正确性,而那**已实现且已测**。
  4. 多 agent「25× 复杂度、每条连接是竞态点」也是来源反复告警的(source [19/20]),不该在自主 loop 里为省延迟盲上。
- ✅ **升 P2「约束守卫 / ConstraintBreach」为真正的 P0**。它在「①价值 ②确定性可验 ③自主循环可做」三轴上都更强:
  - 直击**奖励黑客**(删测试转绿)——loop-engineering 头号失败模式;
  - **确定性验收无计时抖动**:构造删/清空 `tests/` 或改保护路径的调用 → 守卫拦截 → 停机 `ConstraintBreach`,纯文件系统断言,离线可测;
  - **确是真缺口**:现有 jail 只把写限在 cwd 子树、denylist 只拦危险 shell——**cwd 内 `rm tests/foo.rs` 当前是放行的**,伪造 CI 绿的经典攻击没被挡;
  - 与我刚落的护栏套件(iter-5/6 jail/denylist/read-only)+ HaltReason(iter-13)**同轨自然延展**。
- 📝 **signals/ 仍押后**:NotebookLM 自己的 P1 验收要「Run B 加载 Run A 的 signal」,那需要跨 run 装配的额外基建;当前无 signal 产者,先押后(YAGNI,已在 iter-13 留升级注释)。
- 📝 **`ContextRot` / `CircuitBroken` HaltReason**:确定性可判,列为 iter-14 的 P1 轻量补充(若廉价则随手做)。
- ✅ 引用干净:cost/loop-engineering/多智能体引用均对题,未见张冠李戴。

## 采纳后的 Iteration 14(见 CONTRACT-iteration-14)

**P0 = 约束守卫(ConstraintBreach)**:护栏套件补上「防奖励黑客」一环——保护路径(默认 `tests/` 与用户可配)禁删/禁清空,命中即停机返 `ConstraintBreach`,离线可测。**P1(可选)**= 补 `ContextRot`/`CircuitBroken` 两个确定性 HaltReason。**押后**:子智能体并行编排(性能上限、计时验收易抖、来源自相矛盾)、signals/ 复利(无产者,YAGNI)。
