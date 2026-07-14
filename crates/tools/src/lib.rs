//! # tools —— agent 的真实工具集(M1 物理闭环)
//!
//! 只用 std,跨平台。这是让 agent 从「真空运行」走向能触碰真实世界的第一块:
//! 有了真实的文件读写与 shell 退出码,验证器(verifier)才有客观信号可判。
//!
//! ⚠ M1 阶段 **不做沙箱**:`run_shell` 直接在宿主机跑,只用于受控命令,别喂不可信输入。
//! 沙箱(Docker/gVisor/WASM)留到 harness 阶段。

use std::io;
use std::path::Path;
use std::process::Command;

/// 读文件全文。
pub fn read_file(path: impl AsRef<Path>) -> io::Result<String> {
    std::fs::read_to_string(path)
}

/// 整文件写入(覆盖)。父目录不存在会报错 —— 调用方自己保证目录。
pub fn write_file(path: impl AsRef<Path>, contents: &str) -> io::Result<()> {
    std::fs::write(path, contents)
}

/// 精准编辑:把文件里**唯一**出现的 `old` 换成 `new`(Claude Code 的 Edit 语义)。
/// 0 处或多处匹配都报错 —— 逼调用方带足够上下文保证唯一,避免整文件覆写丢内容、省 token。
pub fn edit_file(path: impl AsRef<Path>, old: &str, new: &str) -> io::Result<()> {
    let content = read_file(&path)?;
    match content.matches(old).count() {
        0 => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "old_string 未找到 —— 先 read_file 核对原文",
        )),
        1 => write_file(path, &content.replacen(old, new, 1)),
        n => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("old_string 匹配 {n} 处,需唯一 —— 带上更多上下文"),
        )),
    }
}

/// 批量编辑里的一处:某文件的一次唯一匹配替换(同 [`edit_file`] 语义)。
#[derive(Debug, Clone)]
pub struct Edit {
    pub path: String,
    pub old: String,
    pub new: String,
}

impl Edit {
    pub fn new(path: impl Into<String>, old: impl Into<String>, new: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            old: old.into(),
            new: new.into(),
        }
    }
}

/// 跨文件**原子**批量编辑:先把所有编辑在内存里校验 + 应用(每处唯一匹配,同文件按序叠加),
/// **全部通过才逐个落盘**;写盘中途失败 → 回滚已写的文件。返回改动的文件数。
/// 这让 agent 一次改多处/多文件、只确认一份汇总 diff、要么全成要么全不动(不留半成品破坏编译)。
/// ponytail: 回滚靠重写原文,非事务级;极端并发/掉电下仍可能不一致 —— 单机 agent 场景够用。
pub fn apply_edits(edits: &[Edit]) -> Result<usize, String> {
    use std::collections::BTreeMap;
    // 涉及的每个文件读一次原文。
    let mut content: BTreeMap<&str, String> = BTreeMap::new();
    for e in edits {
        if !content.contains_key(e.path.as_str()) {
            let text = read_file(&e.path).map_err(|err| format!("读 {} 失败: {err}", e.path))?;
            content.insert(&e.path, text);
        }
    }
    let originals = content.clone();
    // 按序把每处编辑叠加到对应文件内容(同文件多处 = 顺序 apply)。全体校验唯一匹配。
    for e in edits {
        let c = content.get_mut(e.path.as_str()).unwrap();
        match c.matches(&e.old).count() {
            1 => *c = c.replacen(&e.old, &e.new, 1),
            0 => return Err(format!("{}: old_string 未找到", e.path)),
            n => return Err(format!("{}: old_string 匹配 {n} 处,需唯一", e.path)),
        }
    }
    // 全 OK → 落盘;任一写失败 → 回滚已写的,报错。
    let mut written: Vec<&str> = Vec::new();
    for (path, text) in &content {
        if let Err(err) = write_file(path, text) {
            for wp in &written {
                let _ = write_file(wp, &originals[wp]);
            }
            return Err(format!(
                "写 {path} 失败: {err};已回滚 {} 个文件",
                written.len()
            ));
        }
        written.push(path);
    }
    Ok(content.len())
}

/// 把一批编辑渲染成**汇总 diff**(按文件分组,每处 `-` 旧 / `+` 新),给权限门一次确认。
pub fn edits_diff(edits: &[Edit]) -> String {
    let mut s = String::new();
    let mut last_path = "";
    for e in edits {
        if e.path != last_path {
            s.push_str(&format!("--- {}\n", e.path));
            last_path = &e.path;
        }
        for l in e.old.lines() {
            s.push_str(&format!("  - {l}\n"));
        }
        for l in e.new.lines() {
            s.push_str(&format!("  + {l}\n"));
        }
    }
    s
}

/// 读文件的一段:从第 `offset` 行(1 起)读至多 `limit` 行。大文件不必整读,省上下文。
pub fn read_file_range(path: impl AsRef<Path>, offset: usize, limit: usize) -> io::Result<String> {
    let content = read_file(path)?;
    Ok(content
        .lines()
        .skip(offset.saturating_sub(1))
        .take(limit)
        .collect::<Vec<_>>()
        .join("\n"))
}

/// 递归搜索时跳过的噪声目录 —— 别把 target/.git 也扫了。
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", ".codegraph"];
/// 单次搜索最多返回的命中行数,防爆上下文。
const SEARCH_CAP: usize = 200;

/// 跨平台代码搜索:在 `root` 下递归找**文件名匹配 `glob`**(如 `*.rs`)、**内容含 `needle` 子串**
/// 的行,返回 `相对路径:行号:内容`。Windows 无 grep,这是 agent 找代码的可移植接缝。
/// ponytail: 子串匹配非正则、glob 只认单个前/后缀 `*`;命中上限 [`SEARCH_CAP`] 行,超出截断。
pub fn search(root: impl AsRef<Path>, needle: &str, glob: &str) -> io::Result<String> {
    let root = root.as_ref();
    let mut out = Vec::new();
    search_dir(root, root, needle, glob, &mut out)?;
    let truncated = out.len() > SEARCH_CAP;
    out.truncate(SEARCH_CAP);
    if truncated {
        out.push(format!(
            "… (命中超过 {SEARCH_CAP} 行,已截断;缩小范围或用更精确的 pattern)"
        ));
    }
    Ok(out.join("\n"))
}

fn search_dir(
    base: &Path,
    dir: &Path,
    needle: &str,
    glob: &str,
    out: &mut Vec<String>,
) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let path = entry.path();
        if ft.is_dir() {
            if !SKIP_DIRS.contains(&name.as_ref()) {
                search_dir(base, &path, needle, glob, out)?;
            }
        } else if ft.is_file() && glob_match(glob, &name) {
            // 读不成 UTF-8(二进制)就跳过,别把搜索搞挂。
            if let Ok(content) = read_file(&path) {
                let rel = path.strip_prefix(base).unwrap_or(&path);
                for (i, line) in content.lines().enumerate() {
                    if line.contains(needle) {
                        out.push(format!("{}:{}:{}", rel.display(), i + 1, line.trim_end()));
                        if out.len() > SEARCH_CAP {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// 极简 glob:`*` / `*.rs`(后缀)/ `main.*`(前缀)/ 精确名。ponytail: 只认单个 `*`,够用即可。
fn glob_match(pat: &str, name: &str) -> bool {
    if pat == "*" || pat.is_empty() {
        true
    } else if let Some(suffix) = pat.strip_prefix('*') {
        name.ends_with(suffix)
    } else if let Some(prefix) = pat.strip_suffix('*') {
        name.starts_with(prefix)
    } else {
        name == pat
    }
}

/// 一次 shell 执行的结果。`code` 是退出码(被信号杀掉时为 -1)。
#[derive(Debug, Clone)]
pub struct ShellResult {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl ShellResult {
    /// 退出码 0 即成功 —— 验证器认这个客观信号,不认模型自述。
    pub fn success(&self) -> bool {
        self.code == 0
    }
}

/// 危险命令拦截:**即使用户批准也拒绝**执行的灾难性命令(无沙箱阶段的安全硬门槛)。
/// 返回 `Some(原因)` 表示危险。保守的 denylist —— 不求完备,只拦最灾难的那几类,宁可漏拦不误伤日常命令。
pub fn is_dangerous_command(cmd: &str) -> Option<&'static str> {
    // 归一化:小写 + 压缩空白,挡住 `rm   -rf    /` 之类的绕过。
    let c = cmd.to_lowercase();
    let c: String = c.split_whitespace().collect::<Vec<_>>().join(" ");
    const DENY: &[(&str, &str)] = &[
        ("rm -rf /", "递归删除根目录"),
        ("rm -rf /*", "递归删除根目录"),
        ("rm -rf ~", "递归删除 home"),
        ("mkfs", "格式化文件系统"),
        ("dd of=/dev/", "直写块设备"),
        (":(){", "fork 炸弹"),
        ("> /dev/sd", "覆写块设备"),
        ("chmod -r 777 /", "破坏系统权限"),
        ("format c:", "格式化 C 盘"),
        ("del /f /s /q c:", "删空 C 盘"),
    ];
    DENY.iter()
        .find(|(pat, _)| c.contains(pat))
        .map(|(_, why)| *why)
}

/// 跨平台执行一条 shell 命令:Windows 走 `cmd /C`,其余走 `sh -c`。
pub fn run_shell(cmd: &str) -> io::Result<ShellResult> {
    let output = if cfg!(windows) {
        Command::new("cmd").args(["/C", cmd]).output()?
    } else {
        Command::new("sh").arg("-c").arg(cmd).output()?
    };

    Ok(ShellResult {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read_roundtrips() {
        // 确定性验收:写进去的内容能原样读回(哈希/内容一致)。
        let mut path = std::env::temp_dir();
        path.push("ridge_tools_roundtrip.txt");
        let body = "hello ridge\n物理闭环\n";
        write_file(&path, body).unwrap();
        let got = read_file(&path).unwrap();
        assert_eq!(got, body);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn shell_reports_exit_code() {
        // 确定性验收:退出码被如实带回。
        assert_eq!(run_shell("exit 0").unwrap().code, 0);
        assert!(run_shell("exit 0").unwrap().success());
        assert_eq!(run_shell("exit 3").unwrap().code, 3);
        assert!(!run_shell("exit 3").unwrap().success());
    }

    #[test]
    fn shell_captures_stdout() {
        let out = run_shell("echo ridge").unwrap();
        assert_eq!(out.code, 0);
        assert!(out.stdout.contains("ridge"));
    }

    #[test]
    fn edit_file_replaces_unique_occurrence() {
        let mut path = std::env::temp_dir();
        path.push("ridge_edit_unique.txt");
        write_file(&path, "let x = 1;\nlet y = 2;\n").unwrap();
        edit_file(&path, "let x = 1;", "let x = 42;").unwrap();
        assert_eq!(read_file(&path).unwrap(), "let x = 42;\nlet y = 2;\n");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn edit_file_rejects_missing_and_ambiguous() {
        let mut path = std::env::temp_dir();
        path.push("ridge_edit_reject.txt");
        write_file(&path, "dup\ndup\n").unwrap();
        assert!(edit_file(&path, "nope", "x").is_err()); // 0 处
        assert!(edit_file(&path, "dup", "x").is_err()); // 2 处 → 不唯一
        assert_eq!(read_file(&path).unwrap(), "dup\ndup\n"); // 报错时原文不动
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn apply_edits_multi_file_atomic() {
        let dir = std::env::temp_dir().join("ridge_batch_edit");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.rs");
        let b = dir.join("b.rs");
        write_file(&a, "let x = 1;\n").unwrap();
        write_file(&b, "let y = 2;\n").unwrap();

        // 跨 2 文件各改一处 → 都生效,返回 2。
        let edits = vec![
            Edit::new(a.to_str().unwrap(), "let x = 1;", "let x = 10;"),
            Edit::new(b.to_str().unwrap(), "let y = 2;", "let y = 20;"),
        ];
        assert_eq!(apply_edits(&edits).unwrap(), 2);
        assert_eq!(read_file(&a).unwrap(), "let x = 10;\n");
        assert_eq!(read_file(&b).unwrap(), "let y = 20;\n");

        // 原子性:一处 old 不存在 → 整批不落盘,两文件都不变。
        let bad = vec![
            Edit::new(a.to_str().unwrap(), "let x = 10;", "let x = 99;"),
            Edit::new(b.to_str().unwrap(), "MISSING", "nope"),
        ];
        assert!(apply_edits(&bad).is_err());
        assert_eq!(
            read_file(&a).unwrap(),
            "let x = 10;\n",
            "失败批次不得改动任何文件"
        );
        assert_eq!(read_file(&b).unwrap(), "let y = 20;\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_edits_same_file_sequential() {
        let mut path = std::env::temp_dir();
        path.push("ridge_batch_same.rs");
        write_file(&path, "a\nb\n").unwrap();
        let edits = vec![
            Edit::new(path.to_str().unwrap(), "a", "AA"),
            Edit::new(path.to_str().unwrap(), "b", "BB"),
        ];
        assert_eq!(apply_edits(&edits).unwrap(), 1, "同文件 2 处 = 1 个文件");
        assert_eq!(read_file(&path).unwrap(), "AA\nBB\n");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn edits_diff_renders_grouped_hunks() {
        let edits = vec![
            Edit::new("x.rs", "old1", "new1"),
            Edit::new("x.rs", "old2", "new2"),
        ];
        let d = edits_diff(&edits);
        assert!(d.contains("--- x.rs"));
        assert!(d.contains("- old1") && d.contains("+ new1") && d.contains("- old2"));
        assert_eq!(d.matches("--- x.rs").count(), 1, "同文件只出一次文件头");
    }

    #[test]
    fn read_file_range_reads_slice() {
        let mut path = std::env::temp_dir();
        path.push("ridge_read_range.txt");
        write_file(&path, "l1\nl2\nl3\nl4\nl5\n").unwrap();
        assert_eq!(read_file_range(&path, 2, 2).unwrap(), "l2\nl3");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn search_finds_matching_lines_by_glob() {
        let dir = std::env::temp_dir().join("ridge_search_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        write_file(dir.join("a.rs"), "fn needle() {}\nother\n").unwrap();
        write_file(dir.join("b.txt"), "needle here too\n").unwrap();
        let hits = search(&dir, "needle", "*.rs").unwrap();
        assert!(hits.contains("a.rs:1:"), "命中 .rs 行: {hits}");
        assert!(!hits.contains("b.txt"), "glob 应过滤掉 .txt: {hits}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn glob_match_covers_common_shapes() {
        assert!(glob_match("*", "anything.rs"));
        assert!(glob_match("*.rs", "main.rs"));
        assert!(!glob_match("*.rs", "main.txt"));
        assert!(glob_match("Cargo.*", "Cargo.toml"));
        assert!(glob_match("Cargo.toml", "Cargo.toml"));
        assert!(!glob_match("Cargo.toml", "cargo.toml"));
    }

    #[test]
    fn dangerous_commands_are_flagged() {
        assert!(is_dangerous_command("rm -rf /").is_some());
        assert!(is_dangerous_command("RM   -RF   /").is_some()); // 大小写/空白绕过
        assert!(is_dangerous_command("sudo mkfs.ext4 /dev/sda1").is_some());
        assert!(is_dangerous_command(":(){ :|:& };:").is_some());
        // 日常命令不误伤。
        assert!(is_dangerous_command("cargo build").is_none());
        assert!(is_dangerous_command("rm -rf target/debug").is_none());
        assert!(is_dangerous_command("git status").is_none());
    }
}
