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
}
