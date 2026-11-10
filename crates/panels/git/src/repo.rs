//! The git side of the panel, without UI. Reading (status) goes through gix,
//! in process; everything that changes the repository goes through the `git`
//! command, so hooks, signing and the user's config apply. Blocking: call off
//! the UI thread.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, bail};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Untracked,
    Conflict,
}

impl Kind {
    pub fn letter(self) -> &'static str {
        match self {
            Kind::Added => "A",
            Kind::Modified => "M",
            Kind::Deleted => "D",
            Kind::Renamed => "R",
            Kind::Untracked => "U",
            Kind::Conflict => "C",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Relative to the repository's working tree (what git commands take).
    pub rel: String,
    pub kind: Kind,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    /// The repository's working tree (may be above the project root).
    pub workdir: PathBuf,
    /// HEAD → index.
    pub staged: Vec<Entry>,
    /// Index → working tree, untracked files included.
    pub unstaged: Vec<Entry>,
}

impl Status {
    pub fn abs(&self, rel: &str) -> PathBuf {
        self.workdir.join(rel)
    }
}

/// `git status` for the repository holding `root`. `Ok(None)` outside a repository.
pub fn status(root: &Path) -> anyhow::Result<Option<Status>> {
    use gix::status::index_worktree::iter::Summary;
    use gix::status::{Item, UntrackedFiles};

    if pide_core::git::git_dir(root).is_none() {
        return Ok(None);
    }
    let repo = gix::discover(root)?;
    let Some(workdir) = repo.workdir().map(Path::to_path_buf) else {
        return Ok(None); // bare
    };
    let iter = repo
        .status(gix::progress::Discard)?
        .untracked_files(UntrackedFiles::Files)
        .into_iter(Vec::<gix::bstr::BString>::new())?;
    let mut out = Status {
        workdir,
        ..Default::default()
    };
    for item in iter {
        let item = item?;
        match &item {
            Item::TreeIndex(change) => {
                use gix::diff::index::ChangeRef;
                let kind = match change {
                    ChangeRef::Addition { .. } => Kind::Added,
                    ChangeRef::Deletion { .. } => Kind::Deleted,
                    ChangeRef::Modification { .. } => Kind::Modified,
                    ChangeRef::Rewrite { .. } => Kind::Renamed,
                };
                out.staged.push(Entry {
                    rel: change.location().to_string(),
                    kind,
                });
            }
            Item::IndexWorktree(i) => {
                let kind = match i.summary() {
                    None | Some(Summary::IntentToAdd) => continue,
                    Some(Summary::Added) => Kind::Untracked,
                    Some(Summary::Removed) => Kind::Deleted,
                    Some(Summary::Renamed | Summary::Copied) => Kind::Renamed,
                    Some(Summary::Conflict) => Kind::Conflict,
                    Some(Summary::Modified | Summary::TypeChange) => Kind::Modified,
                };
                out.unstaged.push(Entry {
                    rel: i.rela_path().to_string(),
                    kind,
                });
            }
        }
    }
    out.staged.sort_by(|a, b| a.rel.cmp(&b.rel));
    out.unstaged.sort_by(|a, b| a.rel.cmp(&b.rel));
    out.staged.dedup();
    out.unstaged.dedup();
    Ok(Some(out))
}

/// Changes against HEAD, the way the Commit window shows them (JetBrains
/// style: no staged / unstaged split): tracked files, and unversioned ones.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub workdir: PathBuf,
    pub tracked: Vec<Entry>,
    pub unversioned: Vec<Entry>,
}

impl Changes {
    pub fn abs(&self, rel: &str) -> PathBuf {
        self.workdir.join(rel)
    }
}

impl From<Status> for Changes {
    fn from(s: Status) -> Self {
        use std::collections::BTreeMap;
        let rank = |k: Kind| match k {
            Kind::Conflict => 0,
            Kind::Added => 1,
            Kind::Deleted => 2,
            Kind::Renamed => 3,
            Kind::Modified => 4,
            Kind::Untracked => 5,
        };
        let mut by_path: BTreeMap<String, Kind> = BTreeMap::new();
        for e in s.staged.iter().chain(&s.unstaged) {
            by_path
                .entry(e.rel.clone())
                .and_modify(|k| {
                    // Staged as new, edited since: still new. A staged file
                    // that's also "untracked" can't happen; keep the strongest.
                    if rank(e.kind) < rank(*k) {
                        *k = e.kind
                    }
                })
                .or_insert(e.kind);
        }
        let mut out = Changes {
            workdir: s.workdir,
            ..Default::default()
        };
        for (rel, kind) in by_path {
            let e = Entry { rel, kind };
            if kind == Kind::Untracked {
                out.unversioned.push(e);
            } else {
                out.tracked.push(e);
            }
        }
        out
    }
}

/// Commit exactly what's given, whatever is staged: `full` files as they are
/// on disk (new, changed or deleted), `partial` files with the given content
/// (some of their changes). Built in a temporary index from HEAD, so the
/// user's staging area isn't used; afterwards the committed paths are reset
/// in the real index to the new HEAD. Returns git's one-line summary.
pub fn commit_selection(
    workdir: &Path,
    message: &str,
    amend: bool,
    full: &[String],
    partial: &[(String, String)],
) -> anyhow::Result<String> {
    let index = std::env::temp_dir().join(format!(
        "pide-commit-{}-{}.index",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let env = [("GIT_INDEX_FILE", index.as_os_str())];
    let result = (|| {
        let has_head = git(workdir, &["rev-parse", "--verify", "-q", "HEAD"], None).is_ok();
        if has_head {
            git_env(workdir, &["read-tree", "HEAD"], None, &env)?;
        } else {
            git_env(workdir, &["read-tree", "--empty"], None, &env)?;
        }
        if !full.is_empty() {
            let mut args = vec!["add", "-A", "--"];
            args.extend(full.iter().map(String::as_str));
            git_env(workdir, &args, None, &env)?;
        }
        for (rel, content) in partial {
            let sha = git(workdir, &["hash-object", "-w", "--stdin"], Some(content))?;
            let mode = git(workdir, &["ls-tree", "HEAD", "--", rel], None)
                .ok()
                .and_then(|l| l.split_whitespace().next().map(str::to_string))
                .unwrap_or_else(|| "100644".into());
            let info = format!("{mode},{},{rel}", sha.trim());
            git_env(
                workdir,
                &["update-index", "--add", "--cacheinfo", &info],
                None,
                &env,
            )?;
        }
        let mut args = vec!["commit", "-q", "-F", "-"];
        if amend {
            args.push("--amend");
        }
        git_env(workdir, &args, Some(message), &env)
    })();
    let _ = std::fs::remove_file(&index);
    result?;
    // The real index: committed paths now match HEAD (what's left of a
    // partly committed file shows as a change again).
    let paths: Vec<String> = full
        .iter()
        .cloned()
        .chain(partial.iter().map(|(p, _)| p.clone()))
        .collect();
    if !paths.is_empty() {
        let _ = with_paths(workdir, &["reset", "-q"], &paths);
    }
    git(workdir, &["log", "-1", "--format=%h %s"], None).map(|s| s.trim().to_string())
}

/// Put files back to HEAD, in the index and on disk. A file that's new in the
/// index is only un-added (it stays on disk, unversioned).
pub fn rollback_to_head(workdir: &Path, entries: &[Entry]) -> anyhow::Result<()> {
    let added: Vec<String> = entries
        .iter()
        .filter(|e| e.kind == Kind::Added)
        .map(|e| e.rel.clone())
        .collect();
    let rest: Vec<String> = entries
        .iter()
        .filter(|e| !matches!(e.kind, Kind::Added | Kind::Untracked))
        .map(|e| e.rel.clone())
        .collect();
    if !added.is_empty() {
        with_paths(workdir, &["rm", "-q", "--cached", "-r"], &added)?;
    }
    if !rest.is_empty() {
        with_paths(
            workdir,
            &["restore", "--source=HEAD", "--staged", "--worktree"],
            &rest,
        )?;
    }
    Ok(())
}

/// The last `n` commit subjects (for matching their style).
pub fn recent_subjects(workdir: &Path, n: usize) -> Vec<String> {
    git(workdir, &["log", &format!("-{n}"), "--format=%s"], None)
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// Commits ahead of / behind the upstream branch; `None` without an upstream.
pub fn ahead_behind(workdir: &Path) -> Option<(u32, u32)> {
    let out = git(
        workdir,
        &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"],
        None,
    )
    .ok()?;
    let mut it = out.split_whitespace().map(|n| n.parse().ok());
    Some((it.next()??, it.next()??))
}

pub fn stage(workdir: &Path, rels: &[String]) -> anyhow::Result<()> {
    with_paths(workdir, &["add", "--all"], rels)
}

pub fn unstage(workdir: &Path, rels: &[String]) -> anyhow::Result<()> {
    // `restore --staged` needs a HEAD; before the first commit, drop from the index.
    if git(workdir, &["rev-parse", "--verify", "-q", "HEAD"], None).is_ok() {
        with_paths(workdir, &["restore", "--staged"], rels)
    } else {
        with_paths(workdir, &["rm", "--cached", "-r", "-q"], rels)
    }
}

/// Throw away the working-tree changes of tracked files (back to the index).
pub fn rollback(workdir: &Path, rels: &[String]) -> anyhow::Result<()> {
    with_paths(workdir, &["restore", "--worktree"], rels)
}

/// Commit what's staged. Returns git's one-line summary.
pub fn commit(workdir: &Path, message: &str, amend: bool) -> anyhow::Result<String> {
    let mut args = vec!["commit", "-q", "-F", "-"];
    if amend {
        args.push("--amend");
    }
    git(workdir, &args, Some(message))?;
    git(workdir, &["log", "-1", "--format=%h %s"], None).map(|s| s.trim().to_string())
}

/// The last commit's message (for Amend).
pub fn last_message(workdir: &Path) -> Option<String> {
    git(workdir, &["log", "-1", "--format=%B"], None)
        .ok()
        .map(|s| s.trim_end().to_string())
}

fn with_paths(workdir: &Path, args: &[&str], rels: &[String]) -> anyhow::Result<()> {
    let mut all: Vec<&str> = args.to_vec();
    all.push("--");
    all.extend(rels.iter().map(String::as_str));
    git(workdir, &all, None).map(|_| ())
}

/// Run a read-only `git` command; its stdout.
pub(crate) fn git_out(workdir: &Path, args: &[&str]) -> anyhow::Result<String> {
    git(workdir, args, None)
}

/// Like [`git_out`], raw bytes (file contents).
pub(crate) fn git_bytes(workdir: &Path, args: &[&str]) -> anyhow::Result<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(workdir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .context("run git")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        bail!("git {}: {err}", args.first().copied().unwrap_or(""));
    }
    Ok(out.stdout)
}

fn git(workdir: &Path, args: &[&str], stdin: Option<&str>) -> anyhow::Result<String> {
    git_env(workdir, args, stdin, &[])
}

fn git_env(
    workdir: &Path,
    args: &[&str],
    stdin: Option<&str>,
    env: &[(&str, &std::ffi::OsStr)],
) -> anyhow::Result<String> {
    use std::io::Write as _;
    use std::process::Stdio;
    let mut child = Command::new("git")
        .arg("-C")
        .arg(workdir)
        .args(args)
        .envs(env.iter().copied())
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("run git")?;
    if let Some(text) = stdin {
        child.stdin.take().unwrap().write_all(text.as_bytes())?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        bail!("git {}: {err}", args.first().copied().unwrap_or(""));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .args([] as [&str; 0])
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    fn rels(e: &[Entry]) -> Vec<(&str, Kind)> {
        e.iter().map(|e| (e.rel.as_str(), e.kind)).collect()
    }

    #[test]
    fn status_stage_unstage_commit() {
        let dir = std::env::temp_dir().join(format!("pide-git-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        sh(&dir, &["init", "-q", "-b", "main"]);
        sh(&dir, &["config", "user.name", "t"]);
        sh(&dir, &["config", "user.email", "t@t"]);
        sh(&dir, &["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        std::fs::write(dir.join("gone.txt"), "x\n").unwrap();

        // Before the first commit: everything untracked; stage + unstage work without HEAD.
        let s = status(&dir).unwrap().unwrap();
        assert_eq!(
            rels(&s.unstaged),
            [("a.txt", Kind::Untracked), ("gone.txt", Kind::Untracked)]
        );
        stage(&s.workdir, &["a.txt".into()]).unwrap();
        assert_eq!(
            rels(&status(&dir).unwrap().unwrap().staged),
            [("a.txt", Kind::Added)]
        );
        unstage(&s.workdir, &["a.txt".into()]).unwrap();
        assert!(status(&dir).unwrap().unwrap().staged.is_empty());

        stage(&s.workdir, &["a.txt".into(), "gone.txt".into()]).unwrap();
        assert_eq!(
            commit(&s.workdir, "first", false)
                .unwrap()
                .split_once(' ')
                .unwrap()
                .1,
            "first"
        );

        // Modify, delete, add in a subfolder.
        std::fs::write(dir.join("a.txt"), "two\n").unwrap();
        std::fs::remove_file(dir.join("gone.txt")).unwrap();
        std::fs::write(dir.join("src/new.rs"), "fn x() {}\n").unwrap();
        let s = status(&dir.join("src")).unwrap().unwrap(); // found from a subfolder
        assert_eq!(
            rels(&s.unstaged),
            [
                ("a.txt", Kind::Modified),
                ("gone.txt", Kind::Deleted),
                ("src/new.rs", Kind::Untracked)
            ]
        );
        stage(&s.workdir, &["gone.txt".into()]).unwrap(); // staging a deletion
        let s = status(&dir).unwrap().unwrap();
        assert_eq!(rels(&s.staged), [("gone.txt", Kind::Deleted)]);

        rollback(&s.workdir, &["a.txt".into()]).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one\n");

        commit(&s.workdir, "second", false).unwrap();
        commit(&s.workdir, "second, amended", true).unwrap();
        assert_eq!(last_message(&s.workdir).unwrap(), "second, amended");
        assert_eq!(ahead_behind(&s.workdir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_only_what_is_checked() {
        let dir = std::env::temp_dir().join(format!("pide-git-sel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        sh(&dir, &["init", "-q", "-b", "main"]);
        sh(&dir, &["config", "user.name", "t"]);
        sh(&dir, &["config", "user.email", "t@t"]);
        sh(&dir, &["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("a.txt"), "1\n2\n3\n4\n5\n").unwrap();
        std::fs::write(dir.join("b.txt"), "b\n").unwrap();
        std::fs::write(dir.join("gone.txt"), "g\n").unwrap();
        sh(&dir, &["add", "."]);
        sh(&dir, &["commit", "-q", "-m", "base"]);

        // a.txt: two changes, only the first goes in. b.txt: staged, not checked.
        std::fs::write(dir.join("a.txt"), "ONE\n2\n3\n4\nFIVE\n").unwrap();
        std::fs::write(dir.join("b.txt"), "b2\n").unwrap();
        sh(&dir, &["add", "b.txt"]);
        std::fs::remove_file(dir.join("gone.txt")).unwrap();
        std::fs::write(dir.join("new.txt"), "n\n").unwrap();
        let c: Changes = status(&dir).unwrap().unwrap().into();
        let tracked: Vec<(&str, Kind)> = c.tracked.iter().map(|e| (e.rel.as_str(), e.kind)).collect();
        assert_eq!(
            tracked,
            [("a.txt", Kind::Modified), ("b.txt", Kind::Modified), ("gone.txt", Kind::Deleted)]
        );
        assert_eq!(c.unversioned[0].rel, "new.txt");

        commit_selection(
            &dir,
            "partial",
            false,
            &["gone.txt".into(), "new.txt".into()],
            &[("a.txt".into(), "ONE\n2\n3\n4\n5\n".into())],
        )
        .unwrap();
        let show = |spec: &str| git(&dir, &["show", spec], None).unwrap();
        assert_eq!(show("HEAD:a.txt"), "ONE\n2\n3\n4\n5\n");
        assert_eq!(show("HEAD:b.txt"), "b\n"); // staged but unchecked: not committed
        assert_eq!(show("HEAD:new.txt"), "n\n");
        assert!(git(&dir, &["show", "HEAD:gone.txt"], None).is_err());
        // The file on disk keeps its other change; b.txt stays staged.
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "ONE\n2\n3\n4\nFIVE\n");
        let s = status(&dir).unwrap().unwrap();
        assert_eq!(rels(&s.staged), [("b.txt", Kind::Modified)]);
        assert_eq!(rels(&s.unstaged), [("a.txt", Kind::Modified)]);

        // Roll b.txt and a.txt back to HEAD.
        let c: Changes = s.into();
        rollback_to_head(&dir, &c.tracked).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "b\n");
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "ONE\n2\n3\n4\n5\n");
        assert!(status(&dir).unwrap().unwrap().staged.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn outside_a_repository() {
        assert_eq!(status(Path::new("/")).unwrap(), None);
    }
}
