# NotebookLM 指导归档 + 对抗评审(iter-24)

> conversation `68791fb7` 续。NLM 定夺:主刀 = **内核向 Best-of-N 分支探索**(引 `91397bf0` 投机分支/BoN+确定性验证器,论据真);P1 = BPM 区间粘贴、动态输入高度(引 `207981a6`,论据真,且后者原文即荐「纯函数 height_for 免虚拟终端可测」—— 与本仓验收铁律同源)。

## 对抗评审裁决

### ✅ 采纳(P0,收窄):引擎 Best-of-N 原语 `invoke_best_of`
- **来源真支撑** + 引擎有 BSP 底座。但**关键边界(NLM 未见)**:真实 agent 分支并发跑**副作用工具会撞同一工作区**(写文件/shell 互踩)—— 无 BranchFS/worktree 隔离前,BoN **只作引擎通用原语**(N 份初始状态并发 `invoke_with`、失败分支丢弃、按调用方评分器择优、平分低索引确定性胜出),**不接入 CLI 主流程**。agent 侧只落确定性评分器 `branch_score`(approved 压倒一切,同侪省 token 者胜)。工作区隔离(每分支 worktree)列后续迭代。
- **❌ 驳回 CowState(Arc 包字段)**:过早优化 —— N≤4、AgentState clone 廉价,量测前不动状态结构(ponytail)。
- **❌ 驳回 Pareto/GEPA 简化版选优**:一个 `Fn(&S) -> i64` 足矣;多目标待真需求。
- **❌ 驳回 NLM 验收信号「sleep 200ms 总耗 <300ms」**:计时断言,第二次踩线(iter-23 已驳同类)。改:语义断言 —— 择优正确、失败分支被弃、空输入归错(`NoWinner`)、全部确定性。

### ✅ 采纳(P1):Bracketed Paste(粘贴假死防护)
- `EnableBracketedPaste` **best-effort**(旧 Windows conhost 不支持,失败即静默退化逐字粘贴,不可让 TUI 起不来);select 环加 `Event::Paste` 臂整块注入;纯函数 `sanitize_paste`(CRLF/CR→LF、滤控制字符留 \n\t)可测。
- ❌ 驳回「stdin 注入 1 万字符 + 查 PushHistory 次数」验收:需真 PTY,环境断言;纯函数测替代。

### ✅ 采纳(P1):动态输入高度 `input_height`
- 来源公式 H=clamp(行数+边框, min, max),原文自荐纯函数化 —— 直接落 `input_height(content, width, min, max)`(字符数近似折行;CJK 宽度≈1 格的偏差留 ponytail)。
- ◻ CSI u(Shift+Enter)不做:需协议握手 + 终端探测,押后;多行内容本轮唯一入口是粘贴。

### 差集清单校正
NLM 称「真沙箱=仅 Landlock/Denylist」—— 本仓现实是 jail + denylist,**无 Landlock**;引用略夸,记录不改向。
