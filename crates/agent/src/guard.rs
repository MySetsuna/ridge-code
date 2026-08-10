use crate::config::HookCfg;
use provider::ToolCall;

/// 地址越狱开关(iter-34):进程级,默认 **关**。开则 `jail` 放行 cwd 子树外的写。
/// **只放宽 cwd 子树这一条** —— 危险命令硬拦截、受保护路径(tests/.git)守卫、只读模式全不受影响。
/// 与 `jail` 已读的进程级 cwd 同层(进程内 TUI 与后台任务共享),不逐调用穿参。
static ALLOW_JAILBREAK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// 置地址越狱开关(启动读 config、TUI `/jailbreak` 实时切)。安全放宽,开启时 TUI 须显红标。
pub fn set_allow_jailbreak(on: bool) {
    ALLOW_JAILBREAK.store(on, std::sync::atomic::Ordering::Relaxed);
}
/// 读地址越狱开关(`jail` 与状态栏红标用)。
pub fn allow_jailbreak() -> bool {
    ALLOW_JAILBREAK.load(std::sync::atomic::Ordering::Relaxed)
}

/// 写操作沙箱守卫:路径须落在**进程 cwd 子树**内(`--cwd` 设的工作目录),越狱 → `Err(BLOCKED 串)`。
/// 深度防御,与危险命令拦截同层:即使模型幻觉出绝对路径/`..` 逃逸,也硬拒,防写出工作目录祸害宿主。
pub(crate) fn jail(path: &str) -> Result<(), String> {
    jail_guard(allow_jailbreak(), path)
}

/// jail 决策纯函数(iter-34):`allow` 为开关快照。`allow==true` → 放行;否则钳在 cwd 子树。
/// 抽纯函数是为可测且**不在测试里翻全局**(AtomicBool 全局若被某测试改会污染并行的 `jail_blocks_write_outside_cwd`)。
fn jail_guard(allow: bool, path: &str) -> Result<(), String> {
    if allow {
        return Ok(());
    }
    let root = std::env::current_dir().map_err(|e| format!("BLOCKED (jail): 取 cwd 失败: {e}"))?;
    tools::jail_path(&root, path)
        .map(|_| ())
        .map_err(|e| format!("BLOCKED (jail): {e}"))
}

/// 有副作用的内置工具(改文件 / 跑 shell)。只读模式过滤/拒绝它们;jail 只管其中的写文件路径。
pub(crate) fn is_mutating_tool(name: &str) -> bool {
    matches!(
        name,
        "run_shell" | "write_file" | "edit_file" | "apply_edits"
    )
}

/// **受保护路径**(词法判定):路径任一组件为 `tests`(约定测试目录)或 `.git`。防**奖励黑客**
/// —— 删/清空失败测试以伪造 CI 绿(loop engineering 头号失败模式)。用 `tests`(复数目录)而非 `test`
/// 单词,免误伤 `cargo test`/`test_output.log` 等。
fn is_protected_path(path: &str) -> bool {
    path.replace('\\', "/")
        .split('/')
        .any(|c| matches!(c, "tests" | ".git"))
}

/// 约束守卫(写臂):往受保护路径写**空内容** = 清空测试 → 拒。正常带内容的编辑放行。
pub(crate) fn constraint_guard_write(path: &str, contents: &str) -> Option<String> {
    (is_protected_path(path) && contents.trim().is_empty())
        .then(|| format!("BLOCKED (constraint): 拒绝清空受保护路径 {path}(防奖励黑客删/空测试)"))
}

/// 约束守卫(shell 臂):删除类命令(rm/rmdir/del/unlink/shred)或截断重定向(`>`)touch 受保护路径 → 拒。
pub(crate) fn constraint_guard_shell(cmd: &str) -> Option<String> {
    let lc = cmd.to_lowercase();
    let has_delete = lc
        .split(|c: char| c.is_whitespace())
        .any(|t| matches!(t, "rm" | "rmdir" | "del" | "unlink" | "shred"));
    let has_truncate = lc.contains('>');
    if !(has_delete || has_truncate) {
        return None;
    }
    // 按空白/引号切 token,看是否有 token 的路径组件命中受保护目录。
    let touches_protected = lc
        .split(|c: char| c.is_whitespace() || c == '"' || c == '\'')
        .any(is_protected_path);
    touches_protected.then(|| {
        format!("BLOCKED (constraint): 拒绝对受保护路径(测试)的删除/清空 `{cmd}`(防奖励黑客)")
    })
}

/// 只读模式(`--read-only`)的深度防御:副作用工具即使被 offer/幻觉调到,也硬拒。
/// `Some(观察串)` = 拒绝(与 offering 过滤形成双保险)。
pub(crate) fn read_only_block(read_only: bool, name: &str) -> Option<String> {
    (read_only && is_mutating_tool(name))
        .then(|| format!("BLOCKED (read-only): 只读模式拒绝副作用工具 {name}"))
}

// ───────────────────────── Hook 引擎(iter-40)─────────────────────────

static HOOKS: std::sync::OnceLock<Vec<HookCfg>> = std::sync::OnceLock::new();
static NOTIFY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 启动时装入用户 config 的 hooks(进程级 set-once,与 DYNAMIC_COMMANDS/ALLOW_JAILBREAK 先例一致)。
pub fn set_hooks(hooks: Vec<HookCfg>) {
    let _ = HOOKS.set(hooks);
}
/// 任务完成响铃开关(内置通知 hook)。
pub fn set_notify(on: bool) {
    NOTIFY.store(on, std::sync::atomic::Ordering::Relaxed);
}
fn active_hooks() -> &'static [HookCfg] {
    HOOKS.get().map(|v| v.as_slice()).unwrap_or(&[])
}

// ─────────────────── 外置沙箱包裹 seam(iter-46)───────────────────

static SANDBOX_CMD: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

/// 启动时装入 config 的 `sandbox_cmd`(进程级 set-once,与 HOOKS/ALLOW_JAILBREAK 先例一致)。
/// 配了则 `run_shell` 经它跑(真隔离交平台);None = 宿主直跑。
pub fn set_sandbox_cmd(cmd: Option<String>) {
    let _ = SANDBOX_CMD.set(cmd.filter(|s| !s.trim().is_empty()));
}
pub(crate) fn active_sandbox_cmd() -> Option<String> {
    SANDBOX_CMD.get().and_then(|o| o.clone())
}

/// 引号感知分词(纯):`"..."`/`'...'` 内空白保留,裸词按空白切。给 sandbox_cmd 模板拆 argv。
fn consume_sandbox_char(
    c: char,
    current: &mut String,
    started: &mut bool,
    quote: &mut Option<char>,
) -> bool {
    if let Some(q) = *quote {
        if c == q {
            *quote = None;
        } else {
            current.push(c);
        }
        return false;
    }
    match c {
        '"' | '\'' => {
            *quote = Some(c);
            *started = true;
        }
        c if c.is_whitespace() => {
            if *started {
                *started = false;
                return true;
            }
        }
        c => {
            current.push(c);
            *started = true;
        }
    }
    false
}

pub fn sandbox_split(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    for c in s.chars() {
        if consume_sandbox_char(c, &mut cur, &mut started, &mut quote) {
            out.push(std::mem::take(&mut cur));
        }
    }
    if started {
        out.push(cur);
    }
    out
}

/// 把用户 shell 命令包进 sandbox_cmd 模板(纯):split 模板 → `{cwd}` 替换 → **user_cmd 作最后单个 arg 追加**。
/// argv 方式(非 shell 字符串拼接)→ user_cmd 原样进用户包裹器的解释器(如 `sh -c`),免跨平台二次引号地狱。
pub fn sandbox_argv(sandbox_cmd: &str, user_cmd: &str, cwd: &str) -> Vec<String> {
    let mut argv: Vec<String> = sandbox_split(sandbox_cmd)
        .into_iter()
        .map(|a| a.replace("{cwd}", cwd))
        .collect();
    argv.push(user_cmd.to_string());
    argv
}

/// 选出匹配某事件(+ 工具名)的 hook。`matcher` 缺/空 = 匹配所有工具;否则工具名含该子串。纯函数。
pub fn hooks_for_event<'a>(hooks: &'a [HookCfg], event: &str, tool: &str) -> Vec<&'a HookCfg> {
    hooks
        .iter()
        .filter(|h| {
            h.event == event
                && h.matcher
                    .as_deref()
                    .map(|m| m.is_empty() || tool.contains(m))
                    .unwrap_or(true)
        })
        .collect()
}

/// 工具调用的「主参数」(喂给 hook 的 `RIDGE_TOOL_ARG`):按常见键取一个。纯函数。
fn tool_primary_arg(call: &ToolCall) -> String {
    for k in ["cmd", "path", "query", "url", "task"] {
        if let Some(s) = call.arguments.get(k).and_then(|v| v.as_str()) {
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }
    String::new()
}

/// hook 命令是否安全(纯):不命中灾难 denylist 即安全。**hook 与 run_shell 工具同守这条硬线**
/// —— §8 不变量⑦「危险命令拦截不可绕过」对所有 shell 通道成立(iter-41 补:此前 hook 通道漏拦)。
fn hook_is_safe(command: &str) -> bool {
    tools::is_dangerous_command(command).is_none()
}

/// 跑一条 hook 命令(跨平台),把工具名/主参数经 **env**(非全局,只挂这条 Command —— BSP 并发安全)
/// 注入。返回退出码。灾难命令 → 不执行 + 审计留痕 + None;best-effort:起不来 → None。
fn run_hook_command(command: &str, tool: &str, arg: &str) -> Option<i32> {
    if !hook_is_safe(command) {
        audit("hook_blocked", command); // 灾难命令不执行(不可绕过);留痕便于排查误配
        return None;
    }
    use std::process::Command;
    let mut c = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", command]);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(command);
        c
    };
    c.env("RIDGE_TOOL", tool).env("RIDGE_TOOL_ARG", arg);
    c.output().ok().map(|o| o.status.code().unwrap_or(-1))
}

/// pre_tool hook:任一 **blocking** hook 命令非 0 退出 → 返回 BLOCKED(拦下工具)。否则 None(放行)。
pub(crate) fn run_pre_tool_hooks(call: &ToolCall) -> Option<String> {
    let arg = tool_primary_arg(call);
    for h in hooks_for_event(active_hooks(), "pre_tool", &call.name) {
        let code = run_hook_command(&h.command, &call.name, &arg);
        if h.blocking.unwrap_or(false) && code.map(|c| c != 0).unwrap_or(true) {
            return Some(format!(
                "BLOCKED (pre_tool hook rejected `{}`) —— 见 config.hooks",
                call.name
            ));
        }
    }
    None
}

/// post_tool hook:工具跑完 fire-and-forget(如写文件后格式化)。
pub(crate) fn run_post_tool_hooks(call: &ToolCall) {
    let arg = tool_primary_arg(call);
    for h in hooks_for_event(active_hooks(), "post_tool", &call.name) {
        let _ = run_hook_command(&h.command, &call.name, &arg);
    }
}

/// 审计行格式(纯函数,不含时间戳 —— 时间戳由 [`audit`] 落盘时前置,保持本函数确定性可测)。
pub fn audit_line(event: &str, detail: &str) -> String {
    if detail.is_empty() {
        format!("[{event}]")
    } else {
        format!("[{event}] {detail}")
    }
}

/// 会话审计留痕(内置 hook,总是开):事件追加进 `~/.ridge/audit.log`(前置 epoch 秒)。best-effort。
fn audit(event: &str, detail: &str) {
    let Some(home) = std::env::var("USERPROFILE")
        .ok()
        .or_else(|| std::env::var("HOME").ok())
    else {
        return;
    };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = format!("{home}/.ridge/audit.log");
    if let Some(dir) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{ts} {}", audit_line(event, detail));
    }
}

/// 触发会话级 hook(`session_start` / `stop`):内置审计 + config 声明的会话 hook;`stop` 且 notify 开则响铃。
/// `detail` 进审计行(如 stop 带步数)。供 main/tui 生命周期调。
pub fn fire_session_hooks(event: &str, detail: &str) {
    audit(event, detail);
    for h in hooks_for_event(active_hooks(), event, "") {
        let _ = run_hook_command(&h.command, event, detail);
    }
    if event == "stop" && NOTIFY.load(std::sync::atomic::Ordering::Relaxed) {
        eprint!("\x07"); // 终端铃:任务完成通知(内置 hook)
        use std::io::Write;
        let _ = std::io::stderr().flush();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        audit_line, constraint_guard_shell, constraint_guard_write, hook_is_safe, hooks_for_event,
        is_mutating_tool, jail_guard, read_only_block, sandbox_argv, sandbox_split,
    };
    use crate::{builtin_tool_specs, Config};

    /// iter-46:sandbox_cmd 模板引号感知分词。
    #[test]
    fn sandbox_split_respects_quotes() {
        assert_eq!(sandbox_split("a b c"), vec!["a", "b", "c"]);
        // 引号内空白保留、与相邻裸段拼一 arg。
        assert_eq!(
            sandbox_split(r#"docker run -v "C:/my proj":/w sh -c"#),
            vec!["docker", "run", "-v", "C:/my proj:/w", "sh", "-c"]
        );
        assert!(sandbox_split("   ").is_empty());
    }

    /// iter-46:包裹 argv —— {cwd} 替换 + user_cmd 恒作最后单个 arg(含空格不被再切)。
    #[test]
    fn sandbox_argv_substitutes_cwd_and_appends_cmd() {
        let argv = sandbox_argv(
            "docker run --rm -v {cwd}:/w -w /w alpine sh -c",
            "ls -la",
            "/proj x",
        );
        assert_eq!(
            argv,
            vec![
                "docker",
                "run",
                "--rm",
                "-v",
                "/proj x:/w",
                "-w",
                "/w",
                "alpine",
                "sh",
                "-c",
                "ls -la"
            ]
        );
        // user_cmd 恒为最后单个 arg —— 免二次 shell 引号地狱。
        assert_eq!(argv.last().unwrap(), "ls -la");
    }

    /// 约束守卫抗奖励黑客:删/清空受保护路径(测试)被拦;正常编辑与 `cargo test` 不误伤。
    #[test]
    fn constraint_guard_blocks_test_tampering() {
        // 写臂:清空 tests/ 文件被拦;带内容写放行。
        assert!(
            constraint_guard_write("tests/foo.rs", "").is_some(),
            "清空测试应拦"
        );
        assert!(
            constraint_guard_write("tests/foo.rs", "   \n").is_some(),
            "空白=清空,应拦"
        );
        assert!(
            constraint_guard_write("tests/foo.rs", "fn t() {}").is_none(),
            "带内容的正常编辑不该误伤"
        );
        assert!(
            constraint_guard_write("src/lib.rs", "").is_none(),
            "非保护路径不拦"
        );

        // shell 臂:删 tests/ 被拦;截断重定向进 tests/ 被拦;cargo test / 删源码不误伤。
        assert!(
            constraint_guard_shell("rm tests/foo_test.rs").is_some(),
            "rm 测试应拦"
        );
        assert!(
            constraint_guard_shell("rm -rf tests").is_some(),
            "rm 测试目录应拦"
        );
        assert!(
            constraint_guard_shell("echo '' > tests/foo.rs").is_some(),
            "截断测试应拦"
        );
        assert!(
            constraint_guard_shell("cargo test > out.log").is_none(),
            "cargo test 不该被误伤(tests 复数目录才拦)"
        );
        assert!(
            constraint_guard_shell("rm src/tmp.rs").is_none(),
            "删源码非本守卫职责(jail 管边界)"
        );
    }

    /// iter-40:hook 事件+matcher 过滤 + 审计行格式。
    #[test]
    fn hooks_for_event_filters() {
        let cfg = Config::parse(
            r#"{"hooks":[
                {"event":"pre_tool","matcher":"run_shell","command":"guard.sh","blocking":true},
                {"event":"post_tool","command":"fmt.sh"},
                {"event":"stop","command":"notify.sh"}
            ]}"#,
        );
        assert_eq!(cfg.hooks.len(), 3);
        // pre_tool + matcher:命中 run_shell、不命中 read_file。
        let pre = hooks_for_event(&cfg.hooks, "pre_tool", "run_shell");
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0].blocking, Some(true));
        assert!(hooks_for_event(&cfg.hooks, "pre_tool", "read_file").is_empty());
        // post_tool 无 matcher → 匹配任意工具。
        assert_eq!(
            hooks_for_event(&cfg.hooks, "post_tool", "write_file").len(),
            1
        );
        // stop 会话事件命中;无声明的事件为空。
        assert_eq!(hooks_for_event(&cfg.hooks, "stop", "").len(), 1);
        assert!(hooks_for_event(&cfg.hooks, "session_start", "").is_empty());
    }

    /// iter-40:审计行格式(纯,无时间戳)。
    #[test]
    fn audit_line_format() {
        assert_eq!(audit_line("session_start", ""), "[session_start]");
        assert_eq!(audit_line("stop", "steps=4"), "[stop] steps=4");
    }

    /// iter-41:hook 命令同守灾难 denylist(§8 不变量⑦不可绕过对 hook 通道亦成立)。
    #[test]
    fn hook_is_safe_blocks_disaster() {
        assert!(!hook_is_safe("rm -rf /"));
        assert!(!hook_is_safe("mkfs.ext4 /dev/sda"));
        assert!(!hook_is_safe(":(){ :|:& };:"));
        assert!(hook_is_safe("cargo fmt"));
        assert!(hook_is_safe("echo formatted $RIDGE_TOOL_ARG"));
    }

    /// iter-34:地址越狱决策纯函数 —— 开则放行 cwd 外,关则拦。测显式 bool,**不翻全局**(免污染并行的 jail_blocks 测试)。
    #[test]
    fn jail_guard_allows_when_on_blocks_when_off() {
        let outside = std::env::temp_dir().join("ridge_jailbreak_probe.txt");
        let p = outside.to_str().unwrap();
        assert!(jail_guard(true, p).is_ok(), "越狱开:放行 cwd 外写");
        let blocked = jail_guard(false, p);
        assert!(
            blocked.is_err() && blocked.unwrap_err().contains("BLOCKED"),
            "越狱关:cwd 外写仍拦"
        );
    }

    /// 只读模式:装配时从 offering 里滤掉副作用工具,只留读/查/研究类。
    #[test]
    fn read_only_filters_out_mutating_tools() {
        let ro: Vec<String> = builtin_tool_specs()
            .into_iter()
            .filter(|s| !is_mutating_tool(&s.name))
            .map(|s| s.name)
            .collect();
        for m in ["run_shell", "write_file", "edit_file", "apply_edits"] {
            assert!(!ro.contains(&m.to_string()), "只读不应 offer {m}");
        }
        for r in [
            "read_file",
            "search",
            "web_search",
            "fetch_url",
            "todo_write",
        ] {
            assert!(ro.contains(&r.to_string()), "只读应保留 {r}");
        }
    }

    /// 只读模式深度防御:只拦副作用工具,读类放行;非只读一律不拦。
    #[test]
    fn read_only_block_rejects_mutating_only() {
        assert!(read_only_block(true, "write_file").is_some());
        assert!(read_only_block(true, "run_shell").is_some());
        assert!(read_only_block(true, "read_file").is_none());
        assert!(read_only_block(false, "write_file").is_none());
        assert!(read_only_block(true, "edit_file")
            .unwrap()
            .starts_with("BLOCKED (read-only)"));
    }
}
