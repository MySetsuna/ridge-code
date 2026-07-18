# CONTRACT · iteration-46 —— 外置沙箱包裹 seam(合「外置能力不进内核」铁律)

> maker = 用户(AskUserQuestion「都做」之三 + 沙箱方案选「外置沙箱包裹 seam(荐)」);checker = 我。价值:北极星「安全层沙箱」欠账的**务实、可确定性测、不违铁律**解法 —— 真 OS 隔离交平台(docker/wsl),内核只提供包裹接缝。

## 背景(代码核实)

- 现有安全:`tools::jail_path`(写限 cwd 子树)+ `is_dangerous_command`(run_shell 灾难命令拦截)——**应用层护栏,非 OS 隔离**;`run_shell` 仍宿主直跑。
- Landlock/Seccomp(Linux-only)早驳;Windows AppContainer FFI 重且**难确定性测**(要跑进程验文件系统)、与「外置能力不进内核」铁律张力大 → 亦驳。
- 结论:内核提供**包裹接缝**,把「真隔离」委托给用户环境的 docker/wsl/自定义沙箱(与仓内自述「真 OS 隔离是重量级件另议」一致)。

## 目标(P0)

1. **Config `sandbox_cmd: Option<String>`**(serde-default;模板,支持 `{cwd}` 占位)。
2. **纯核**(agent lib,离线可测):
   - `sandbox_split(s) -> Vec<String>`:引号感知分词(`"..."`/`'...'`/裸词)。
   - `sandbox_argv(sandbox_cmd, user_cmd, cwd) -> Vec<String>`:split → `{cwd}` 替换 → **user_cmd 作最后单个 arg 追加**(argv 方式,免跨平台二次 shell 引号地狱)。
3. **`tools::run_argv(argv) -> io::Result<ShellResult>`**:`Command::new(argv[0]).args(argv[1..]).output()`(不经 cmd/sh 包裹,verbatim 传参)。
4. **接线**:`execute_tool_call` 的 run_shell 分支,在**危险命令拦截 + 约束守卫之后**(防御纵深:即便沙箱也先挡灾难命令),配了 sandbox_cmd → `run_argv(sandbox_argv(...))`,否则 `run_shell`(宿主直跑,行为不变)。
5. **进程全局** `SANDBOX_CMD: OnceLock<Option<String>>` + `set_sandbox_cmd`(启动装,与 HOOKS/ALLOW_JAILBREAK set-once 先例一致);main.rs 启动 `set_sandbox_cmd(cfg.sandbox_cmd.clone())`。

## 边界(不做)

- 不引 OS 隔离 FFI(AppContainer/受限令牌/Landlock)—— 违铁律、难测。
- 不做 shell 字符串模板 `{cmd}` 插值(引号地狱)—— user_cmd 恒作独立 argv 元素传入用户包裹器的解释器(如 `sh -c`)。
- 不改 jail/危险拦截/只读逻辑;沙箱是**叠加**的纵深防御,非替换。
- 不动 samples/install 脚本(用户文件);文档化即可。

## 确定性验收信号(纯函数/数据结构断言,无跑进程/计时/PTY)

门禁 `cargo test --workspace && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` 全 **exit 0**。新增测试:
1. `sandbox_split_respects_quotes`:`"a b c"`→3 词;`docker run -v "C:/my proj":/w sh -c`→ `C:/my proj:/w` 合为一 arg;全空白 → 空。
2. `sandbox_argv_substitutes_cwd_and_appends_cmd`:`sandbox_argv("docker run --rm -v {cwd}:/w -w /w alpine sh -c", "ls -la", "/proj x")` == 预期 argv,且 `argv.last() == "ls -la"`(user_cmd 恒最后单 arg,含空格不被再切)。
3. 既有测试全绿(未配 sandbox_cmd 时 run_shell 宿主直跑,零行为变化;`SANDBOX_CMD` 未 set → `active_sandbox_cmd()` None)。
- **不**单测 `run_argv` 真执行(需 docker/wsl);受测的是 argv 构造纯逻辑。

## 停机

单轮;收尾:回写 ARCHITECTURE(安全节:外置沙箱包裹 seam)、报告、提交带 `iter-46`。
