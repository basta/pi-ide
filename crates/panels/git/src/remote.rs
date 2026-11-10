//! Branches and remotes through the `git` command (so credentials, hooks and
//! the user's config apply): the branch list with tracking, outgoing commits,
//! pull / push / fetch, checkout, new branch, merge, rebase, rename, delete.
//! Blocking: call off the UI thread.

use std::path::Path;

use anyhow::bail;

use crate::repo::git_out;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchInfo {
    /// `main`, or `origin/main` for a remote branch.
    pub name: String,
    pub remote: bool,
    pub current: bool,
    /// Commits ahead of / behind its upstream (local branches with one).
    pub ahead: u32,
    pub behind: u32,
    /// Its upstream (`origin/main`), if any.
    pub upstream: Option<String>,
}

/// Local branches (current first), then remote ones.
pub fn branches(workdir: &Path) -> anyhow::Result<Vec<BranchInfo>> {
    let out = git_out(
        workdir,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(HEAD)%00%(upstream:short)%00%(upstream:track,nobracket)",
            "refs/heads",
            "refs/remotes",
        ],
    )?;
    Ok(parse_branches(&out))
}

fn parse_branches(out: &str) -> Vec<BranchInfo> {
    let mut list: Vec<BranchInfo> = out
        .lines()
        .filter_map(|l| {
            let mut f = l.split('\0');
            let full = f.next()?;
            let head = f.next()? == "*";
            let upstream = f.next().filter(|s| !s.is_empty()).map(str::to_string);
            let track = f.next().unwrap_or("");
            let (name, remote) = if let Some(n) = full.strip_prefix("refs/heads/") {
                (n, false)
            } else {
                let n = full.strip_prefix("refs/remotes/")?;
                if n.ends_with("/HEAD") {
                    return None;
                }
                (n, true)
            };
            let count = |word: &str| {
                track
                    .split(", ")
                    .find_map(|p| p.strip_prefix(word))
                    .and_then(|n| n.trim().parse().ok())
                    .unwrap_or(0)
            };
            Some(BranchInfo {
                name: name.to_string(),
                remote,
                current: head,
                ahead: count("ahead "),
                behind: count("behind "),
                upstream,
            })
        })
        .collect();
    list.sort_by(|a, b| {
        (a.remote, !a.current, &a.name).cmp(&(b.remote, !b.current, &b.name))
    });
    list
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutCommit {
    pub hash: String,
    pub subject: String,
    pub author: String,
    pub time: i64,
}

/// What a push of the current branch would send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outgoing {
    pub branch: String,
    /// Where it goes: `origin/main`.
    pub target: String,
    /// The branch has no upstream yet (push sets it).
    pub new_branch: bool,
    pub commits: Vec<OutCommit>,
    /// Commits on the target we don't have (a plain push would be rejected).
    pub behind: u32,
}

pub fn outgoing(workdir: &Path) -> anyhow::Result<Outgoing> {
    let branch = git_out(workdir, &["symbolic-ref", "--short", "-q", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if branch.is_empty() {
        bail!("HEAD isn't on a branch (detached): check out a branch to push");
    }
    let upstream = git_out(
        workdir,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"],
    )
    .ok()
    .map(|s| s.trim().to_string());
    let (target, new_branch, range): (String, bool, Vec<String>) = match upstream {
        Some(u) => (u.clone(), false, vec![format!("{u}..HEAD")]),
        None => {
            let remote = first_remote(workdir)?;
            (
                format!("{remote}/{branch}"),
                true,
                vec!["HEAD".into(), "--not".into(), "--remotes".into()],
            )
        }
    };
    let mut args = vec!["log", "--format=%H%x00%s%x00%an%x00%at"];
    args.extend(range.iter().map(String::as_str));
    let commits = git_out(workdir, &args)?
        .lines()
        .filter_map(|l| {
            let mut f = l.split('\0');
            Some(OutCommit {
                hash: f.next()?.to_string(),
                subject: f.next()?.to_string(),
                author: f.next()?.to_string(),
                time: f.next()?.parse().unwrap_or(0),
            })
        })
        .collect();
    let behind = if new_branch {
        0
    } else {
        git_out(workdir, &["rev-list", "--count", "HEAD..@{upstream}"])
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    };
    Ok(Outgoing {
        branch,
        target,
        new_branch,
        commits,
        behind,
    })
}

fn first_remote(workdir: &Path) -> anyhow::Result<String> {
    let remotes = git_out(workdir, &["remote"])?;
    let mut names: Vec<&str> = remotes.lines().filter(|l| !l.is_empty()).collect();
    if names.is_empty() {
        bail!("this repository has no remote to push to");
    }
    // `origin` if there is one.
    names.sort_by_key(|n| *n != "origin");
    Ok(names[0].to_string())
}

/// Push the current branch (setting its upstream the first time).
pub fn push(workdir: &Path, out: &Outgoing, force: bool) -> anyhow::Result<String> {
    let mut args = vec!["push"];
    if force {
        args.push("--force-with-lease");
    }
    let remote;
    if out.new_branch {
        remote = out
            .target
            .strip_suffix(&format!("/{}", out.branch))
            .unwrap_or("origin")
            .to_string();
        args.extend(["-u", remote.as_str(), out.branch.as_str()]);
    }
    git_out(workdir, &args)?;
    Ok(format!(
        "Pushed {} commit{} to {}",
        out.commits.len(),
        if out.commits.len() == 1 { "" } else { "s" },
        out.target
    ))
}

/// Update Project: `git pull` with the user's own pull settings (merge or rebase).
pub fn pull(workdir: &Path) -> anyhow::Result<String> {
    let before = git_out(workdir, &["rev-parse", "HEAD"]).unwrap_or_default();
    git_out(workdir, &["pull", "--no-edit"])?;
    let after = git_out(workdir, &["rev-parse", "HEAD"]).unwrap_or_default();
    if before == after {
        return Ok("Already up to date".into());
    }
    let n = git_out(
        workdir,
        &["rev-list", "--count", &format!("{}..{}", before.trim(), after.trim())],
    )
    .map(|s| s.trim().to_string())
    .unwrap_or_default();
    Ok(format!("Updated: {n} new commit(s)"))
}

pub fn fetch(workdir: &Path) -> anyhow::Result<String> {
    git_out(workdir, &["fetch", "--all", "--prune"])?;
    Ok("Fetched".into())
}

/// Check out a branch; a remote one gets (or reuses) a local tracking branch.
pub fn checkout(workdir: &Path, b: &BranchInfo) -> anyhow::Result<String> {
    if !b.remote {
        git_out(workdir, &["checkout", "-q", &b.name])?;
        return Ok(format!("Checked out {}", b.name));
    }
    let local = b.name.split_once('/').map_or(b.name.as_str(), |(_, n)| n);
    let exists = git_out(
        workdir,
        &["rev-parse", "--verify", "-q", &format!("refs/heads/{local}")],
    )
    .is_ok();
    if exists {
        git_out(workdir, &["checkout", "-q", local])?;
    } else {
        git_out(workdir, &["checkout", "-q", "--track", &b.name])?;
    }
    Ok(format!("Checked out {local}"))
}

pub fn new_branch(workdir: &Path, name: &str, from: &str) -> anyhow::Result<String> {
    git_out(workdir, &["checkout", "-q", "-b", name, from])?;
    Ok(format!("Created and checked out {name}"))
}

pub fn merge(workdir: &Path, name: &str) -> anyhow::Result<String> {
    git_out(workdir, &["merge", "--no-edit", name])?;
    Ok(format!("Merged {name}"))
}

pub fn rebase(workdir: &Path, onto: &str) -> anyhow::Result<String> {
    git_out(workdir, &["rebase", onto])?;
    Ok(format!("Rebased onto {onto}"))
}

pub fn rename(workdir: &Path, old: &str, new: &str) -> anyhow::Result<String> {
    git_out(workdir, &["branch", "-m", old, new])?;
    Ok(format!("Renamed {old} to {new}"))
}

/// Delete a local branch (git refuses when it isn't merged).
pub fn delete(workdir: &Path, name: &str) -> anyhow::Result<String> {
    git_out(workdir, &["branch", "-d", name])?;
    Ok(format!("Deleted {name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse() {
        let out = "refs/heads/feature\0 \0\0\nrefs/heads/main\0*\0origin/main\0ahead 1, behind 2\nrefs/remotes/origin/HEAD\0 \0\0\nrefs/remotes/origin/main\0 \0\0\n";
        let b = parse_branches(out);
        let names: Vec<&str> = b.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["main", "feature", "origin/main"]);
        assert!(b[0].current);
        assert_eq!((b[0].ahead, b[0].behind), (1, 2));
        assert_eq!(b[0].upstream.as_deref(), Some("origin/main"));
        assert!(b[2].remote);
    }

    fn sh(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn push_pull_with_a_local_remote() {
        let base = std::env::temp_dir().join(format!("pide-remote-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (origin, a, b) = (base.join("origin.git"), base.join("a"), base.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        sh(&base, &["init", "-q", "--bare", "-b", "main", "origin.git"]);
        sh(&a, &["init", "-q", "-b", "main"]);
        sh(&a, &["config", "commit.gpgsign", "false"]);
        sh(&a, &["remote", "add", "origin", origin.to_str().unwrap()]);
        std::fs::write(a.join("x.txt"), "1\n").unwrap();
        sh(&a, &["add", "."]);
        sh(&a, &["commit", "-q", "-m", "one"]);

        // First push: no upstream yet.
        let out = outgoing(&a).unwrap();
        assert!(out.new_branch);
        assert_eq!(out.target, "origin/main");
        assert_eq!(out.commits.len(), 1);
        push(&a, &out, false).unwrap();
        let out = outgoing(&a).unwrap();
        assert!(!out.new_branch);
        assert!(out.commits.is_empty());

        // Someone else pushes; we fetch and pull it.
        sh(&base, &["clone", "-q", origin.to_str().unwrap(), "b"]);
        sh(&b, &["config", "commit.gpgsign", "false"]);
        std::fs::write(b.join("y.txt"), "2\n").unwrap();
        sh(&b, &["add", "."]);
        sh(&b, &["commit", "-q", "-m", "two"]);
        sh(&b, &["push", "-q"]);
        fetch(&a).unwrap();
        assert_eq!(outgoing(&a).unwrap().behind, 1);
        let list = branches(&a).unwrap();
        assert_eq!((list[0].name.as_str(), list[0].behind), ("main", 1));
        assert!(pull(&a).unwrap().starts_with("Updated"));
        assert_eq!(std::fs::read_to_string(a.join("y.txt")).unwrap(), "2\n");

        // Branch actions.
        new_branch(&a, "side", "main").unwrap();
        let list = branches(&a).unwrap();
        assert!(list.iter().any(|b| b.name == "side" && b.current));
        checkout(&a, list.iter().find(|b| b.name == "main").unwrap()).unwrap();
        rename(&a, "side", "side2").unwrap();
        merge(&a, "side2").unwrap();
        delete(&a, "side2").unwrap();
        assert!(!branches(&a).unwrap().iter().any(|b| b.name.starts_with("side")));
        let _ = std::fs::remove_dir_all(&base);
    }
}
