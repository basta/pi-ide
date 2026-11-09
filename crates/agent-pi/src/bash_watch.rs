//! Files pi's `bash` tool changed (`sed -i`, `mv`, `cargo fmt`, …), with their
//! before-text, so they show up in Changes and can be reverted.
//!
//! When a bash call starts streaming, `snapshot` records every project file's
//! modification time (gitignore-aware) and the content of the files whose
//! before-text git can't give back (modified or untracked; or every small text
//! file when the project isn't a git repo). When the call ends, `changed`
//! compares: a file with a new time, a new file or a missing one changed, and its
//! before-text is the saved content, or HEAD for a file that was clean in git.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Tool caches and build output (often not gitignored): what a test run or a
/// build writes there isn't a change worth reviewing.
const NOISE: &[&str] = &[
    ".git",
    ".DS_Store",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".tox",
    "node_modules",
    "target",
    ".next",
    "dist",
    "build",
];

/// Don't walk past this many files.
const MAX_FILES: usize = 100_000;
/// Don't keep content of files bigger than this…
const MAX_SAVED_FILE: u64 = 1024 * 1024;
/// …or more than this much in total.
const MAX_SAVED_TOTAL: u64 = 32 * 1024 * 1024;

pub struct TreeSnapshot {
    mtimes: HashMap<PathBuf, SystemTime>,
    saved: HashMap<PathBuf, String>,
    git: bool,
}

fn walk(root: &Path) -> HashMap<PathBuf, (SystemTime, u64)> {
    let mut out = HashMap::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .filter_entry(|e| !NOISE.iter().any(|n| e.file_name() == *n))
        .build();
    for e in walker.flatten() {
        if !e.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if let Ok(m) = e.metadata() {
            out.insert(
                e.path().to_path_buf(),
                (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()),
            );
        }
        if out.len() >= MAX_FILES {
            break;
        }
    }
    out
}

/// Paths `git status` lists (modified, added, untracked…), absolute. `None`: not a git repo.
fn git_dirty(root: &Path) -> Option<Vec<PathBuf>> {
    let out = std::process::Command::new("git")
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut items = text.split('\0').filter(|s| !s.is_empty());
    let mut paths = Vec::new();
    while let Some(entry) = items.next() {
        if entry.len() < 4 {
            continue;
        }
        let (xy, path) = entry.split_at(3);
        if xy.starts_with('R') || xy.chars().nth(1) == Some('R') {
            if let Some(old) = items.next() {
                paths.push(root.join(old));
            }
        }
        paths.push(root.join(path));
    }
    Some(paths)
}

fn read_text(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.iter().take(8000).any(|&b| b == 0) {
        return None; // binary
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Before a bash call runs. Blocking (walks the project): call off the async thread.
pub fn snapshot(root: &Path) -> TreeSnapshot {
    let files = walk(root);
    let dirty = git_dirty(root);
    let git = dirty.is_some();
    // What git can't give back: modified/untracked files; everything when there's no git.
    let candidates: Vec<PathBuf> = match dirty {
        Some(d) => d,
        None => files.keys().cloned().collect(),
    };
    let mut saved = HashMap::new();
    let mut total = 0u64;
    for p in candidates {
        let Some(&(_, len)) = files.get(&p) else {
            continue;
        };
        if len > MAX_SAVED_FILE || total + len > MAX_SAVED_TOTAL {
            continue;
        }
        if let Some(t) = read_text(&p) {
            total += len;
            saved.insert(p, t);
        }
    }
    TreeSnapshot {
        mtimes: files.into_iter().map(|(p, (t, _))| (p, t)).collect(),
        saved,
        git,
    }
}

/// The file's text at HEAD (`None`: not tracked or no git).
fn head_text(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let out = std::process::Command::new("git")
        .arg("show")
        .arg(format!("HEAD:{}", rel.to_string_lossy()))
        .current_dir(root)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// After the call: (path, before-text) for every file it changed; `None` before
/// means the file is new. Files whose before-text isn't known are left out.
/// Blocking: call off the async thread.
pub fn changed(root: &Path, before: &TreeSnapshot) -> Vec<(PathBuf, Option<String>)> {
    let now = walk(root);
    let mut out = Vec::new();
    for (path, (mtime, _)) in &now {
        match before.mtimes.get(path) {
            None => out.push((path.clone(), None)), // new file
            Some(t) if t == mtime => {}
            Some(_) => {
                let text = before
                    .saved
                    .get(path)
                    .cloned()
                    .or_else(|| before.git.then(|| head_text(root, path)).flatten());
                match text {
                    Some(t) => out.push((path.clone(), Some(t))),
                    None => log::info!(
                        "bash changed {} but its before-text isn't known",
                        path.display()
                    ),
                }
            }
        }
    }
    for path in before.mtimes.keys() {
        if !now.contains_key(path) {
            // Deleted: bring back what it was.
            let text = before
                .saved
                .get(path)
                .cloned()
                .or_else(|| before.git.then(|| head_text(root, path)).flatten());
            if let Some(t) = text {
                out.push((path.clone(), Some(t)));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_modified_new_and_deleted_files() {
        let dir = std::env::temp_dir().join(format!("pide-bash-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "old a\n").unwrap();
        std::fs::write(dir.join("b.txt"), "old b\n").unwrap();
        std::fs::write(dir.join("keep.txt"), "same\n").unwrap();
        let snap = snapshot(&dir);
        // mtime resolution: make sure the rewrite gets a different time.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.join("src/a.rs"), "new a\n").unwrap();
        std::fs::remove_file(dir.join("b.txt")).unwrap();
        std::fs::write(dir.join("c.txt"), "made by bash\n").unwrap();
        std::fs::create_dir_all(dir.join("__pycache__")).unwrap();
        std::fs::write(dir.join("__pycache__/x.pyc"), "noise").unwrap();
        let got: Vec<(String, Option<String>)> = changed(&dir, &snap)
            .into_iter()
            .map(|(p, b)| (p.strip_prefix(&dir).unwrap().display().to_string(), b))
            .collect();
        assert_eq!(
            got,
            [
                ("b.txt".to_string(), Some("old b\n".to_string())),
                ("c.txt".to_string(), None),
                ("src/a.rs".to_string(), Some("old a\n".to_string())),
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
