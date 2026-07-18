# CONTRACT · iteration-34 —— 地址越狱开关 + 状态栏标红(安全放宽须显眼)

> maker = 用户需求原文「允许地址越狱,但要在状态栏标红」。checker = 本文正确性门禁。安全语义为一等关切。

## 缺口(代码索引核实)

`jail(path)`(lib.rs)无条件把写路径钳在**进程 cwd 子树**内,越狱 → `BLOCKED`。无任何放行开关。用户要:可开一个「允许越狱」开关(写出 cwd 子树),但开着时**状态栏标红**警示。

## 目标

一个默认 **关** 的进程级开关 `allow_jailbreak`:开则 `jail` 放行 cwd 外的写;**危险命令硬拦截、受保护路径(tests/.git)守卫、只读模式全不受影响**(只放宽 cwd 子树这一条)。开启时 TUI 顶状态栏显**红色 `⚠越狱` 徽标**。启动读 config;TUI `/jailbreak [on|off]` 会话内实时切换(默认不持久化,持久化走 `/config set allow_jailbreak true`,更安全:新会话默认锁死)。

## 设计(最小面 / 安全优先)

- lib.rs 进程级 `static ALLOW_JAILBREAK: AtomicBool`(默认 false)+ `pub set_allow_jailbreak(bool)` / `pub allow_jailbreak() -> bool`。与 `jail` 已读的进程级 `cwd` 同层,不逐调用穿参(免动 `execute_tool_call` 签名及其所有调用点/测试)。
- `jail(path)` = `jail_guard(allow_jailbreak(), path)`;**纯函数** `jail_guard(allow, path)`:`allow → Ok(())`;否则原 cwd 子树钳制。**测试只测 `jail_guard(显式 bool, ...)`,绝不改全局**(避免并行测试相互污染 —— AtomicBool 全局若被某测试翻转会殃及 `jail_blocks_write_outside_cwd`)。
- Config 加 `allow_jailbreak: Option<bool>`;`CONFIG_KEYS += "allow_jailbreak"`,`config_set` 归 bool。main 启动 `agent::set_allow_jailbreak(cfg.allow_jailbreak.unwrap_or(false))`。
- tui.rs:`/jailbreak [on|off|(空=查状态)]` 实时切 `agent::set_allow_jailbreak` + 红/绿 note;顶状态栏 `draw` 读 `agent::allow_jailbreak()`,开则在 `RidgeCode` 徽标后插红底黑字 `⚠越狱` span。`/help` 补 `/jailbreak`。

## 边界(不做)

- 越狱粒度(按目录白名单放行部分外部路径)—— 超范围;当前是全开/全关。
- 越狱状态持久化默认关(新会话默认锁死更安全)—— 持久化仅经显式 `/config set`。
- 放宽危险命令拦截 / 受保护路径守卫 / 只读 —— **绝不**;越狱只放宽 cwd 子树写。

## 确定性验收信号

门禁 `cargo test --workspace && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` 全 **exit 0**。新增/守:
- `jail_guard_allows_when_on_blocks_when_off`:`jail_guard(true, <cwd 外绝对路径>)==Ok`;`jail_guard(false, <cwd 外绝对路径>)` 为 `Err` 且含 `BLOCKED`。
- `jail_blocks_write_outside_cwd`(既有)仍绿 —— 证全局默认关、无测试污染。
- `config_set` 认 `allow_jailbreak`(bool 归一):`config_set("{}","allow_jailbreak","true")` Ok 且含 `"allow_jailbreak": true`。
- 危险命令即使越狱开仍拦(逻辑复核:`jail_guard` 不碰 `is_dangerous_command`/`constraint_guard_*`;`run_shell` 臂不经 `jail`)。

## 停机

单轮;连续 2 轮验收不过 → 报告。价值门禁不适用(用户明确需求)。安全放宽必须默认关 + 显眼红标,二者缺一即验收不通过。
