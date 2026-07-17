# NotebookLM 指导归档 + 对抗评审(iter-25)

> conversation `68791fb7` 续。NLM 定夺:主刀 = **分支工作区隔离**(git worktree 首选 + 影子拷贝回落),引 `91397bf0`(Claude Code 生产实践用 worktree 隔离并行 agent、BoN 需隔离沙箱才有可信验证信号)—— 引用真、方向对。

## 对抗评审裁决

### ✅ 采纳(P0,改层):工作区隔离模块 —— 但归 **agent 层**,不进引擎
- **❌ 驳回「隔离路径注入 `RunConfig.cwd`」**:RunConfig 是引擎运行参数,cwd 是 app/工具概念 —— 塞进去重蹈当年 `GraphError::BudgetExceeded` 覆辙(app 概念污染通用引擎)。隔离落 `crates/agent/src/workspace.rs` 新模块,引擎一字不动。
- **选型采纳**:`Workspace::GitWorktree`(main 是 git 仓库且 `git worktree add` 成功)+ `Workspace::ShadowCopy` 回落(递归拷贝,跳过 `.git`/`target`/`.ridge`/`node_modules`)。git 路径 **best-effort**(git 存在性属环境差异,失败即静默回落影子拷贝,不算错误)。

### ✅ 采纳(P0,改法):胜者合回 —— 整文件搬运 + 全有或全无回滚
- **❌ 驳回「用 `apply_edits` 合回」**:概念放错层 —— apply_edits 是**编辑期**唯一匹配替换的原子性;同步期胜者分支的**文件全文即真相**,无 old/new 可匹配。改 `merge_winner(main, branch_dir, modified_files)`:逐文件读分支全文 → 写主区(自动建嵌套目录),任一失败回滚已写(借 apply_edits 的回滚**模式**,不是它的代码)。
- `modified_files`(BTreeSet 有序稳态)恰是天然的合并清单 —— NLM 此点引用成立。

### ✅ 采纳:清理 —— 函数级可测
- `cleanup`:GitWorktree → `git worktree remove --force`(败则 remove_dir_all + prune 兜底);ShadowCopy → remove_dir_all。
- **❌ 驳回「进程异常退出必清理」验收**:进程级信号/退出钩需环境断言;测清理**函数**本身。

### ◻ 收窄:CLI 主流程接线(BoN 端到端)留下一刀
- 真接入需把分支 cwd 贯穿 `execute_tool_call` 全部工具路径解析 —— 独立一刀的体量。本轮止于:隔离模块 + 合回 + 清理,全部确定性可测。NLM「成功标志 = BoN 接入 CLI」**推迟一轮**,与前两轮「单刀最小」纪律一致。

### 验收确定性化(NLM 三信号改造)
- 并发写冲突 → 影子分支 A/B 各写 `a.txt`,主区**期间不变**;merge A 后主区 = AAA。✅ 纯文件系统断言。
- 嵌套路径合回 → 分支新增 `src/inner/mod.rs`,merge 自动建目录。✅
- 自清洁 → `cleanup` 后分支目录不存在。✅(进程级部分驳回,见上)
