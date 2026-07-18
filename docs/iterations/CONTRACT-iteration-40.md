# CONTRACT · iteration-40 —— Hook 系统 + 四个内置 Hook

> maker = 用户需求「支持自定义 Hook,补充一些必要 Hook(写文件后格式化 / 危险操作前确认 / 会话审计留痕 / 任务完成通知)」。checker = 本文正确性门禁。价值门禁不适用(用户明确需求)。

## 目标
1. **自定义 Hook 引擎**:config.json `hooks: [{event, matcher, command, blocking}]`,四事件 `pre_tool`/`post_tool`/`session_start`/`stop`;`pre_tool` 的 blocking hook 命令非 0 退出可**拦下工具**。
2. **四个内置/必要 Hook**:①会话审计留痕(始终开,内部实现);②任务完成通知(`notify` 响铃);③写文件后格式化、④危险操作前确认 —— 由引擎支持,随 `install.ps1` 下发示例配置。

## 设计(最小面)
- **lib.rs**:`HookCfg`(serde)+ Config `hooks`/`notify`;进程全局 `HOOKS: OnceLock`/`NOTIFY: AtomicBool`(set-once,jailbreak 先例);纯核 `hooks_for_event(hooks,event,tool)`(event+matcher 子串)、`audit_line`;`run_hook_command`(**带 `.env()` 的 Command 注入 `RIDGE_TOOL`/`RIDGE_TOOL_ARG`,不设全局 env → BSP 并发安全**);`run_pre_tool_hooks`(blocking→BLOCKED)/`run_post_tool_hooks`;`audit`(→`~/.ridge/audit.log`,前置 epoch 秒);`fire_session_hooks(event,detail)`(审计 + 会话 hook + stop 响铃)。
- **execute_tool_call**:前置 `run_pre_tool_hooks`(拦截早返)、末尾 `run_post_tool_hooks`(match 值收进 `obs` 再 fire)。
- **main.rs**:启动 `set_hooks`/`set_notify`/`fire_session_hooks("session_start")`;run_once/headless 任务毕 `fire_session_hooks("stop")`。
- **tui.rs**:done 处理任务毕 `fire_session_hooks("stop", steps/tokens)`。
- **install.ps1**:示例配置加 `notify` + 两条示例 hook(post_tool 格式化 / pre_tool 危险确认)。
- **不改**:引擎图、登录、命令、Panel。

## 边界(不做)
- hook 传全量工具输入 JSON(仅传主参数 `RIDGE_TOOL_ARG`)—— 够用即止,YAGNI。
- 内置格式化/危险确认**硬编码 shell**(非通用/项目相关)—— 作示例配置下发,不进内核。
- session_start 拦截、更多事件类型 —— YAGNI。

## 确定性验收信号
门禁全 exit 0。新增:
- `hooks_for_event_filters`:event+matcher 过滤(pre_tool 命中/不命中工具、无 matcher 匹配全部、会话事件命中、未声明事件空)。
- `audit_line_format`:`[event]` / `[event] detail`。
- HookCfg 经 `Config::parse` 反序列化(含 blocking)。既有工具/execute_tool_call 测试保持绿(HOOKS 未设→空→零副作用)。

## 停机
单轮;连续 2 轮验收不过 → 报告。收尾:回写 ARCHITECTURE、报告、提交带 `iter-40`;三迭代统一替换 NLM 源 + 重构建本地安装。
