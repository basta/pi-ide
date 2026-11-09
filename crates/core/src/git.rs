//! Git, as much as the status bar and the editor gutter need (spec M4): the
//! current branch, a file's committed text, and which lines differ from it.
//! Uses the `git` command and plain file reads; call off the UI thread.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The repository's `.git` directory for a project (following a worktree's `.git` file).
pub fn git_dir(root: &Path) -> Option<PathBuf> {
    let mut dir = Some(root);
    while let Some(d) = dir {
        let dot = d.join(".git");
        if dot.is_dir() {
            return Some(dot);
        }
        if dot.is_file() {
            // Worktree: "gitdir: /path/to/repo/.git/worktrees/x"
            let text = std::fs::read_to_string(&dot).ok()?;
            let target = text.trim().strip_prefix("gitdir:")?.trim();
            let p = PathBuf::from(target);
            return Some(if p.is_absolute() { p } else { d.join(p) });
        }
        dir = d.parent();
    }
    None
}

/// `main`, or a short commit hash when detached. `None` outside a repository.
pub fn branch(root: &Path) -> Option<String> {
    let head = std::fs::read_to_string(git_dir(root)?.join("HEAD")).ok()?;
    parse_head(&head)
}

pub fn parse_head(head: &str) -> Option<String> {
    let head = head.trim();
    match head.strip_prefix("ref:") {
        Some(r) => Some(r.trim().trim_start_matches("refs/heads/").to_string()),
        None if head.len() >= 7 => Some(head[..7].to_string()),
        None => None,
    }
}

/// The file's text at HEAD. `None` if it isn't tracked (a new file) or there's no repo.
pub fn head_text(path: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(path.parent()?)
        .args(["show", "--no-textconv"])
        .arg(format!("HEAD:./{}", path.file_name()?.to_string_lossy()))
        .env("PATH", std::env::join_paths(path_dirs()).unwrap_or_default())
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8(out.stdout).ok()).flatten()
}

fn path_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    dirs.extend(["/usr/bin", "/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    dirs
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    /// Lines removed before `lines.start` (an empty range).
    Deleted,
}

/// Changed lines in the current text, 0-based and end-exclusive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineChange {
    pub kind: ChangeKind,
    pub lines: std::ops::Range<u32>,
}

/// What changed from `base` to `current`, line by line, for gutter marks.
pub fn line_changes(base: &str, current: &str) -> Vec<LineChange> {
    use similar::{DiffOp, TextDiff};
    let diff = TextDiff::configure().algorithm(similar::Algorithm::Patience).diff_lines(base, current);
    let mut out = Vec::new();
    for op in diff.ops() {
        match *op {
            DiffOp::Equal { .. } => {}
            DiffOp::Insert { new_index, new_len, .. } => {
                out.push(LineChange { kind: ChangeKind::Added, lines: new_index as u32..(new_index + new_len) as u32 })
            }
            DiffOp::Delete { new_index, .. } => {
                out.push(LineChange { kind: ChangeKind::Deleted, lines: new_index as u32..new_index as u32 })
            }
            DiffOp::Replace { new_index, new_len, .. } => {
                out.push(LineChange { kind: ChangeKind::Modified, lines: new_index as u32..(new_index + new_len) as u32 })
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_file_parsing() {
        assert_eq!(parse_head("ref: refs/heads/main\n").as_deref(), Some("main"));
        assert_eq!(parse_head("ref: refs/heads/feature/x").as_deref(), Some("feature/x"));
        assert_eq!(parse_head("2780c1e9d1b3a4f\n").as_deref(), Some("2780c1e"));
    }

    #[test]
    fn changes_added_modified_deleted() {
        let base = "a\nb\nc\nd\n";
        let cur = "a\nB\nc\nnew\n";
        let ch = line_changes(base, cur);
        assert!(ch.contains(&LineChange { kind: ChangeKind::Modified, lines: 1..2 }), "{ch:?}");
        assert!(ch.iter().any(|c| c.lines.contains(&3)), "{ch:?}");
        let del = line_changes("a\nb\nc\n", "a\nc\n");
        assert_eq!(del, vec![LineChange { kind: ChangeKind::Deleted, lines: 1..1 }]);
        let add = line_changes("a\n", "a\nb\nc\n");
        assert_eq!(add, vec![LineChange { kind: ChangeKind::Added, lines: 1..3 }]);
        assert!(line_changes("same\n", "same\n").is_empty());
    }

    #[test]
    fn real_repo_branch_and_head_text() {
        let dir = std::env::temp_dir().join(format!("pide-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            Command::new("git").arg("-C").arg(&dir).args(args).output().map(|o| o.status.success()).unwrap_or(false)
        };
        if !git(&["init", "-q", "-b", "trunk"]) {
            return; // no git here
        }
        std::fs::write(dir.join("f.txt"), "one\n").unwrap();
        git(&["add", "f.txt"]);
        git(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "-m", "x"]);
        std::fs::write(dir.join("f.txt"), "one\ntwo\n").unwrap();
        assert_eq!(branch(&dir).as_deref(), Some("trunk"));
        assert_eq!(head_text(&dir.join("f.txt")).as_deref(), Some("one\n"));
        assert_eq!(head_text(&dir.join("untracked.txt")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
