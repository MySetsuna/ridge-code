# Ridge Agent 迭代报告 —— Iteration 15(补硬伤 + 扬长方向研判)

- **时间戳**: 2026-07-17
- **项目**: RidgeCode(二进制 `ridgecode`)
- **主题**: 补齐核心二进制的**硬限硬伤**(用户明确「补全其余所有硬伤」)+ 用 NotebookLM 深研研判「扬长/差异化」方向真伪
- **状态**: ✅ 补硬伤三刀落地 + 全绿;扬长方向据证据研判完毕

---

## 1. 补硬伤(核心二进制,已落地)

诚实厘定:核心二进制的真硬伤是「回合上限过低」「验证器可被奖励黑客骗过」;而 LSP/代码智能属 **MCP 生态**(加能力=加 MCP,非改源码),非核心硬伤。

| 硬伤 | 修法 | 验收 |
|---|---|---|
| **回合上限 `MAX_STEPS=8` 过低**,真实多文件任务动辄十数次工具调用即被腰斩 | 提到 **30**(≈60 超步,稳在引擎默认 100 超步下,零改引擎)。定位为**后备护栏**,主力停机仍是 `approved`/预算/无进展 | runaway 测试自随常量至 30 并停机 |
| **验证器脆弱/可奖励黑客**:`tool_output_ok` 用 `contains("exit 0")`,失败命令 `exit 7: ...` 正文含 "exit 0" 文本即被误判成功 | 收紧为**行首前缀** `starts_with("exit 0:")`(harness 产出、模型无法伪造行首前缀)。既修正确性 bug 又堵奖励黑客 | 新测 `tool_output_ok_requires_exit0_prefix_not_substring` |
| **子agent步数 `SUBAGENT_MAX_STEPS=8`** 对真实仓库侦察偏紧 | 提到 **15**(恒只读,低风险;为自身循环、不耦合引擎超步) | 既有子agent测试全绿 |

**质量闸**:`cargo test --workspace` = 全绿(agent 53→**54**,+1 测);clippy `-D warnings`、fmt 均净。

**未做(诚实)**:更深的验证器硬化(verify 按**工具身份**而非输出子串门控,防 `read_file` 读到含 "passed" 的文件伪造成功;或 verify 独立跑测试)属 feature 级,列后续。LSP=MCP 生态,非核心码。

## 2. 扬长方向研判(NotebookLM 深研 + 现有 51 源,区分真价值 vs 炒作)

语境:RidgeCode 是**单二进制、单用户** Rust 框架(非多租 SaaS)。三候选裁决:

| 方向 | 证据强度 | 单用户适配 | 裁决 |
|---|---|---|---|
| **A. 时间旅行/分支·多路线并行(best-of-N + 验证器择优)** | 中 | 中 | **有余力再做**:引擎有快照底座,但单用户 CLI 下多属「token 税翻倍的炫技」,价值主在崩溃恢复(已实现)。除非极复杂逻辑分支(如多路径漏洞研究) |
| **B. 自改进 harness / hill-climbing(读 trace 自动优化)** | **弱/炒作** | 低 | **炒作/押后**:需多轨迹样本量方成立;单用户低频交互跑不起来,缺样本驱动 |
| **C. 多 loop 共享大脑(signals 复利)** | **强** | **高** | **唯一优选**:AI Jason 实战框架核心「系统的心脏」;恰建于已完成的标准存储库(.ridge/runs)之上;解决 agent「冷启动」——知识跨会话持久复利。非营销叙事(Anthropic/Google 工程师共识的系统化路径) |

**校正上轮误判**:iter-13 我判「signals 无产者故 YAGNI」——对**空目录**成立,但研判表明真价值在**建全「产者→消费者」闭环**(非空脚手架)。故 iter-13 留的升级注释现应兑现。

## 3. 下一步(见 CONTRACT-iteration-16-signals)

**扬长优选 = C 信号复利闭环**。确定性验收:①产者 loop 探测到事实 → 物理写 `.ridge/signals/<slug>.md`(带 frontmatter schema);②消费者 loop 启动时**自动扫描并注入**未决 signal 到上下文(非用户手输),trace 可见其为推理依据;③消费后 signal 置 `resolved`/归档,退出码 0。A 押后(性能增强)、B 判炒作。

## 4. 开放问题(请 NotebookLM 定夺)

1. signal 的**最小 schema** 该含哪些字段方能既复利又不臃肿(id/type/body/source/status 够否)?
2. 消费者「自动扫描注入 signal」与既有 Durable State 事实块注入如何共存不冲突?注入体量如何保持有界(防上下文膨胀)?
3. signal 存 `.ridge/signals/`(项目级、跨 run 共享)还是 `.ridge/runs/<id>/signals/`(run 级)?跨 loop 复利要前者,审计溯源要后者——能否 run 级产出 + 项目级索引二者兼得?
