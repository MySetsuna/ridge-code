# NotebookLM 指导归档 + 对抗评审(iter-18 → iter-19)

> 笔记本「手搓agent」(52 源)。NotebookLM 是计划 **maker**,非裁判。以下每条经独立 **checker**:①核对引用来源是否真支撑;②第一性原理 + 当前代码现实;③采纳/驳回 + 理由。

## NotebookLM 原始建议(iter-19 排序)

- **P0 硬伤**:Saga/补偿模式回滚 —— 验证失败时自动 `git checkout .` / 快照恢复原始文件态,退出码非零。[来源 25-27]
- **P1 根因**:Token-saver 截断代理 —— 截断巨型工具输出(preview + handle),context_rot 的根因修法。[来源 11,12,14]
- **P2 扬长**:自动 signal 抽取器(hill-climbing Level 4)—— LLM 分析 trace 提炼「发现/避坑」更新技能库。[来源 15-18]
- **P3 安全**:WASM 轻量沙箱作 run_shell 默认隔离。[来源 1,4]
- 子问:护栏投资 —— 真沙箱最高 ROI、可配保护路径高 ROI、**内容级语义检测=过度设计**。

## 对抗评审裁决

### ❌ 驳回 P0「Saga 自动回滚」(非硬伤,是误判)
- **来源错套**:[25-27]=4c37e782 是 Databricks/MLflow **分布式多 agent 生产**语境(agent A/B/C 各带 execute/compensate、orchestrator 逆序走),Saga 源自分布式数据库事务。RidgeCode 是**单用户单 agent 本地文件系统 CLI**,语境不匹配。
- **违自身原则**:自动回滚**毁掉失败现场**,直接违背笔记本 [来源10] 强调的 loop-engineering「preserve mistakes」(iter-17 失败自动产者正建于此)。
- **数据丢失陷阱**:用户在自己的 git 仓里跑 RidgeCode,自动 `git checkout .` 会**连用户未提交的改动一起抹掉**——比「破坏性中间态」危害更大。
- **重造平台原生**:git 本就是用户的回滚层(ponytail:平台原生优先)。合理内核「失败时记录改了哪些文件」**已实现**(durable `modified_files` + manifest + 失败自动 signal,用户可据此 `git checkout` 选择性还原)。

### ✅ 采纳 P1「截断巨型工具输出」→ 定为 iter-19 P0(证据最硬 + 根因 + 契合北极星)
- **来源真支撑**:[11] claw-tsaver 实测 11507→104 token(省 99.1%);[12] context rot = transcript 膨胀致注意力涣散,对策是主动 compact/prune/externalize;[14] Ralph 净上下文。证据强且一致。
- **根因**:现 `context_rot` 仅**终态贴标签**;巨型 `read_file`/`search`/`run_shell` 输出直入 history 撑爆上下文才是根因。截断在**产出接缝**处修根因。
- **守内核铁律**:只做**确定性截断**(head+tail 预览 + 截断标记 + 存盘可 `read_file` 区间重取 = **零丢数据**,文件仍是真相源)。LLM 摘要版属外置能力 → 归 MCP。这与内核已有 `compact_history`/`est_tokens` 同族(上下文卫生),非 RAG/tiktoken 外挂。

### ✅ 确认「内容级语义检测 = 过度设计」(独立佐证 iter-18 的 YAGNI)
- 改断言使恒真属奖励黑客,应由**独立 checker 跑真测**拦(maker≠checker [来源8,9]),非引擎层文本语义分析。NotebookLM 的 checker 与我方判定一致。

### ⏸ 推迟「自动 signal 抽取器」(俟截断后,非本轮)
- 安全内核(从 trace 提炼 signal 喂已建复利环)有效,但需 **LLM 摘要 pass**:内容非确定性、难单测其**内容**(只能测「产出了文件」),工作量大。
- NotebookLM 暗含的「自动改写 harness config」维持 **iter-15 驳回**:单用户轨迹样本不足;改写本身无 checker(谁验证改写?harness 层 maker≠checker 未解)。
- 结论:截断落地、复利环跑出真实 signal 后再议其自动产者的**发现/待办侧**。

### ⏸ 推迟「WASM/真沙箱」(维持 iter-15,归 MCP)
- 重量级、需环境/工具链决策、违单二进制。WASM **跑不了原生 shell/`cargo test`/`git`**([来源1] 自承 WASM 生态兼容差)——用 WASM 隔离 `run_shell` 是范畴错误;真隔离要 Docker/gVisor(需装 Docker)。
- NotebookLM 自身排序矛盾(§1 称沙箱「最高 ROI」,总表列 P3)。词法护栏是单二进制下的务实中档;真沙箱走 MCP(sandbox-exec server)符铁律。

### ◻ 可配保护路径(小优化,可选,非本轮 P0)
- 低成本、确定性;可扩 `is_protected_path` 读 config/env(默认 `tests`/`.git`)保护 `.env`/config。但奖励黑客核心已被默认覆盖,扩展属推测需求 → 除非顺带,不单独立项。

## iter-19 结论
- **P0**:巨型工具输出**确定性截断**(context_rot 根因修法,零丢数据,契合 token 北极星)。`context_rot` 标签保留作残留(多消息累积)的后备。
- **押后**:Saga 回滚(驳回)、自动 signal 抽取(需 LLM pass)、真沙箱(归 MCP)、可配保护路径(可选)。
