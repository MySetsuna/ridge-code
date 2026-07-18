# CONTRACT · iteration-41 —— 加固最近四轮:Hook 过危险拦截(补安全不变量)+ 顶层 key 解析收敛(减法)

> maker = 无外部方向(NLM 认证过期不可用);checker=maker 由本人担,直接从**北极星不变量(CLAUDE.md §8)+ 现状(iter-40)**自算差集。**加法克制**:iter-37–40 连加 5 功能,本轮做加固/减法,不堆新功能。价值门禁:两项均属**正确性/安全/减法**,非花哨。

## 缺口(代码索引核实)

- **安全不变量违背(iter-40 引入)**:§8 不变量⑦「危险命令拦截**不可绕过**」。但 `run_hook_command`(lib.rs)直接 `Command` 跑 hook shell,**未过 `tools::is_dangerous_command`** —— `pre_tool`/`post_tool`/`session`/`stop` hook 里的 `rm -rf /`、`mkfs`、fork 炸弹等灾难命令会被执行。这是 run_shell 工具早已硬拦、而 hook 通道漏拦的一致性缺口。
- **重复(iter-37/38 留)**:顶层 key 解析逻辑(`RIDGE_API_KEY` env → 顶层内联 `api_key` → 顶层 `key_env`→auth)在 `main.rs::real_provider`(前 3 档)与 `tui.rs::current_api_key` **各实现一遍**,发散风险(改一处漏一处 → 行为不一致)。

## 目标
1. **Hook 过危险拦截**:`run_hook_command` 执行前先 `is_dangerous_command` 检查,命中 → **不执行 + 审计留痕**(`hook_blocked`),返回失败。恢复「危险命令拦截不可绕过」对**所有** shell 通道(工具 + hook)成立。
2. **顶层 key 解析收敛**:抽 `resolve_top_level_key(cfg, auth) -> Option<String>` 单一函数(env→内联→key_env→auth),`real_provider` 与 `current_api_key` 共用之,删两处重复档逻辑。

## 设计(最小面)
- **lib.rs**:`fn hook_is_safe(cmd) -> bool = tools::is_dangerous_command(cmd).is_none()`(纯,可测);`run_hook_command` 起手 `if !hook_is_safe(command) { audit("hook_blocked", command); return None; }`。`pub fn resolve_top_level_key(cfg, auth)`(纯核,env 由 `resolve_key_env` 统一读)。
- **main.rs**:`real_provider` 前 3 档替换为 `if let Some(k) = resolve_top_level_key(cfg, auth)`;providers[] 迭代档不变。
- **tui.rs**:`current_api_key` 改为 `resolve_top_level_key(&Config::load(config_path()), &load_auth())`。
- **不改**:hook 事件/触发点/env 注入、preset/login/命令、引擎图。

## 边界(不做)
- 不给 hook 加**完整**权限门(hook 是用户自配,类比 git hooks;只补**灾难 denylist** 这条不可绕过的硬线,不做逐命令 [y/N])。
- 不动 `providers[]` 档解析(`resolve_key_with` 已单测)。
- 不引入新配置项/新命令。

## 确定性验收信号
门禁 `cargo test --workspace && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` 全 **exit 0**。新增:
- `hook_is_safe_blocks_disaster`:`hook_is_safe("rm -rf /")==false`、`hook_is_safe("mkfs.ext4 /dev/sda")==false`、`hook_is_safe("cargo fmt")==true`、`hook_is_safe("echo hi")==true`。
- `resolve_top_level_key_precedence`:顶层内联 `api_key` 优先;无内联时 `key_env`→auth;皆无→None(唯一命名 env 不扰并行)。
- 既有测试全绿(hook 引擎/登录/工具/闭环);`real_provider`/`current_api_key` 行为等价(顶层内联 key 仍启动 —— 复用既有路径,不新增断言即可,靠等价重构 + 现有绿)。

## 停机
单轮;连续 2 轮验收不过 → 报告。收尾:回写 ARCHITECTURE(hook 安全线 + key 解析收敛)、报告、提交带 `iter-41`。**NLM 源替换**:本轮认证过期,记为待办(下次可用时补传);不阻塞本轮完成。
