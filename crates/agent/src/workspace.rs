//! 分支工作区隔离(iter-25):Best-of-N 真实接入的物理前提 —— 并发分支各自在隔离
//! 目录跑副作用工具,互不踩踏;胜者的 `modified_files` 整文搬运合回主区。
//! agent 层模块,引擎(langgraph)零感知。CLI 接线(cwd 贯穿工具执行)留下一刀,见 CONTRACT-iteration-25。

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

/// 一个分支的隔离工作区。
#[derive(Debug)]
pub enum Workspace {
    /// `git worktree add --detach` 出的工作树(main 是 git 仓库且 git 可用时的首选:秒建、共享对象库)。
    GitWorktree { dir: PathBuf },
    /// 影子拷贝回落(非 git 仓库 / git 不可用):递归拷贝,跳过 `.git`/`target`/`.ridge`/`node_modules`。
    ShadowCopy { dir: PathBuf },
}

impl Workspace {
    pub fn dir(&self) -> &Path {
        match self {
            Workspace::GitWorktree { dir } | Workspace::ShadowCopy { dir } => dir,
        }
    }
}

/// main 看起来是 git 仓库?(只探 `.git` 存在,不 spawn git —— 纯文件系统判定,可测。)
pub fn is_git_repo(p: &Path) -> bool {
    p.join(".git").exists()
}

/// 影子拷贝跳过表。ponytail: 固定清单够用;要按 .gitignore 精确过滤再升级。
const SHADOW_SKIP: &[&str] = &[".git", "target", ".ridge", "node_modules"];

fn shadow_copy(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if SHADOW_SKIP.iter().any(|s| name == std::ffi::OsStr::new(s)) {
            continue;
        }
        let src = entry.path();
        let dst = to.join(&name);
        if entry.file_type()?.is_dir() {
            shadow_copy(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// 建一个隔离工作区到 `dest`:git 仓库先试 `git worktree add --detach`(**best-effort**,
/// git 缺席/失败即静默回落影子拷贝 —— 环境差异不是错误);否则直接影子拷贝。
pub fn create_isolated(main: &Path, dest: &Path) -> io::Result<Workspace> {
    if is_git_repo(main) {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(main)
            .args(["worktree", "add", "--detach"])
            .arg(dest)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            return Ok(Workspace::GitWorktree {
                dir: dest.to_path_buf(),
            });
        }
    }
    shadow_copy(main, dest)?;
    Ok(Workspace::ShadowCopy {
        dir: dest.to_path_buf(),
    })
}

/// 胜者合回:把分支里 `modified`(相对路径,BTreeSet 有序稳态)整文搬进主区,自动建嵌套目录。
/// **全有或全无**:任一文件失败即回滚已写(原有文件恢复原文,新建文件删除),主区要么全收要么原样。
/// 注意是整文搬运而非 `apply_edits` —— 同步期分支全文即真相,无 old/new 可匹配(guidance-25)。
pub fn merge_winner(
    main: &Path,
    branch_dir: &Path,
    modified: &BTreeSet<String>,
) -> Result<usize, String> {
    // 先全部读出分支真相(读期零写,失败零损)。
    let mut contents: Vec<(&str, Vec<u8>)> = Vec::with_capacity(modified.len());
    for rel in modified {
        let data = std::fs::read(branch_dir.join(rel))
            .map_err(|e| format!("读分支文件 {rel} 失败: {e}"))?;
        contents.push((rel, data));
    }
    // 逐个落主区;败则回滚已写。
    let mut written: Vec<(&str, Option<Vec<u8>>)> = Vec::new(); // (rel, 原文;None=原本不存在)
    for (rel, data) in &contents {
        let dst = main.join(rel);
        let original = std::fs::read(&dst).ok();
        let attempt = (|| -> io::Result<()> {
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&dst, data)
        })();
        if let Err(e) = attempt {
            for (wrel, orig) in &written {
                let wdst = main.join(wrel);
                match orig {
                    Some(bytes) => {
                        let _ = std::fs::write(&wdst, bytes);
                    }
                    None => {
                        let _ = std::fs::remove_file(&wdst);
                    }
                }
            }
            return Err(format!(
                "写主区 {rel} 失败: {e};已回滚 {} 个文件",
                written.len()
            ));
        }
        written.push((rel, original));
    }
    Ok(written.len())
}

/// 清理分支工作区(best-effort,败者与胜者用毕皆清):worktree 走 `git worktree remove --force`
/// (败则 remove_dir_all + `git worktree prune` 兜底);影子拷贝直接删目录。
pub fn cleanup(main: &Path, ws: &Workspace) {
    match ws {
        Workspace::GitWorktree { dir } => {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(main)
                .args(["worktree", "remove", "--force"])
                .arg(dir)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if !ok {
                let _ = std::fs::remove_dir_all(dir);
                let _ = std::process::Command::new("git")
                    .arg("-C")
                    .arg(main)
                    .args(["worktree", "prune"])
                    .output();
            }
        }
        Workspace::ShadowCopy { dir } => {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每测一个独立临时根(进程 id + 名字),测毕删除 —— 不依赖外部 tempfile crate。
    fn temp_root(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ridge-ws-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn write(p: &Path, rel: &str, s: &str) {
        let f = p.join(rel);
        if let Some(parent) = f.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(f, s).unwrap();
    }
    fn read(p: &Path, rel: &str) -> String {
        std::fs::read_to_string(p.join(rel)).unwrap()
    }

    /// iter-25 核心:两分支并行改同一文件互不踩、主区期间不变;胜者合回后主区=胜者内容。
    #[test]
    fn shadow_isolation_and_winner_merge() {
        let root = temp_root("iso");
        let main = root.join("main");
        write(&main, "a.txt", "base");

        let wa = create_isolated(&main, &root.join("br-a")).unwrap();
        let wb = create_isolated(&main, &root.join("br-b")).unwrap();
        write(wa.dir(), "a.txt", "AAA");
        write(wb.dir(), "a.txt", "BBB");
        assert_eq!(read(&main, "a.txt"), "base"); // 分支写不透传主区

        let modified: BTreeSet<String> = ["a.txt".to_string()].into();
        assert_eq!(merge_winner(&main, wa.dir(), &modified).unwrap(), 1);
        assert_eq!(read(&main, "a.txt"), "AAA"); // 胜者内容,败者 BBB 被弃

        cleanup(&main, &wa);
        cleanup(&main, &wb);
        assert!(!wa.dir().exists() && !wb.dir().exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 合回自动建嵌套目录(分支新增的深路径文件)。
    #[test]
    fn merge_creates_nested_dirs() {
        let root = temp_root("nest");
        let main = root.join("main");
        write(&main, "top.txt", "x");
        let ws = create_isolated(&main, &root.join("br")).unwrap();
        write(ws.dir(), "src/inner/mod.rs", "pub fn f() {}");

        let modified: BTreeSet<String> = ["src/inner/mod.rs".to_string()].into();
        merge_winner(&main, ws.dir(), &modified).unwrap();
        assert_eq!(read(&main, "src/inner/mod.rs"), "pub fn f() {}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 全有或全无:清单里有分支不存在的文件 → Err,主区原样(读期先失败,零写)。
    #[test]
    fn merge_fails_atomically_on_missing_branch_file() {
        let root = temp_root("atomic");
        let main = root.join("main");
        write(&main, "a.txt", "base");
        let ws = create_isolated(&main, &root.join("br")).unwrap();
        write(ws.dir(), "a.txt", "AAA");

        let modified: BTreeSet<String> = ["a.txt".to_string(), "ghost.txt".to_string()].into();
        assert!(merge_winner(&main, ws.dir(), &modified).is_err());
        assert_eq!(read(&main, "a.txt"), "base"); // 未被半合
        assert!(!main.join("ghost.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// git 探测:真 .git 目录为真,平目录为假。
    #[test]
    fn detects_git_repo_by_dot_git() {
        let root = temp_root("git");
        assert!(!is_git_repo(&root));
        std::fs::create_dir_all(root.join(".git")).unwrap();
        assert!(is_git_repo(&root));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 影子拷贝跳过表:.git/target 等不进分支。
    #[test]
    fn shadow_copy_skips_heavy_dirs() {
        let root = temp_root("skip");
        let main = root.join("main");
        write(&main, "keep.txt", "k");
        write(&main, "target/junk.bin", "j");
        write(&main, ".ridge/signals/s.md", "s");
        // 注意:main 无 .git,走影子路径。
        let ws = create_isolated(&main, &root.join("br")).unwrap();
        assert!(ws.dir().join("keep.txt").exists());
        assert!(!ws.dir().join("target").exists());
        assert!(!ws.dir().join(".ridge").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
