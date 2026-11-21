//! History for the Git Log tool window, without UI: commits and refs read with
//! gix in process, the commit graph's lanes, and (through the `git` command)
//! one commit's changed files and file texts. Blocking: call off the UI thread.

use std::collections::{BinaryHeap, HashMap};
use std::path::{Path, PathBuf};

use gix::ObjectId;

/// How many commits one read walks; the list says when there are more.
pub const LIMIT: usize = 5000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RefKind {
    /// The branch HEAD is on (or HEAD itself when detached).
    Head,
    Local,
    Remote,
    Tag,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefLabel {
    /// Short name: `main`, `origin/main`, `v0.5`.
    pub name: String,
    pub kind: RefKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub id: ObjectId,
    pub parents: Vec<ObjectId>,
    pub subject: String,
    /// The message after the subject line (trimmed; may be empty).
    pub body: String,
    pub author: String,
    pub email: String,
    /// Seconds since the epoch, and the author's UTC offset in seconds.
    pub time: i64,
    pub offset: i32,
    pub refs: Vec<RefLabel>,
}

impl Commit {
    pub fn short(&self) -> String {
        self.id.to_hex_with_len(7).to_string()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    pub name: String,
    /// The full ref name (`refs/heads/main`), what a filter walks from.
    pub full: String,
    pub id: ObjectId,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Branches {
    /// The branch HEAD is on; `None` when detached.
    pub head: Option<String>,
    pub local: Vec<Branch>,
    pub remote: Vec<Branch>,
    pub tags: Vec<Branch>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Log {
    pub workdir: PathBuf,
    /// Newest first, children always before their parents.
    pub commits: Vec<Commit>,
    pub branches: Branches,
    /// More commits exist beyond [`LIMIT`].
    pub truncated: bool,
}

/// Read the history of the repository holding `root`, from every branch and
/// tag, or only from `only` (a full ref name). `Ok(None)` outside a repository
/// or in one without commits.
pub fn read(root: &Path, only: Option<&str>) -> anyhow::Result<Option<Log>> {
    if pide_core::git::git_dir(root).is_none() {
        return Ok(None);
    }
    let mut repo = gix::discover(root)?;
    repo.object_cache_size_if_unset(32 * 1024 * 1024);
    let Some(workdir) = repo.workdir().map(Path::to_path_buf) else {
        return Ok(None);
    };
    let head_id = repo.head_id().ok().map(|id| id.detach());
    let Some(head_id) = head_id else {
        return Ok(None); // unborn: no commits yet
    };
    let head_name = repo.head_name()?.map(|n| n.as_bstr().to_string());

    let mut branches = Branches {
        head: head_name
            .as_deref()
            .and_then(|n| n.strip_prefix("refs/heads/"))
            .map(str::to_string),
        ..Default::default()
    };
    let mut labels: HashMap<ObjectId, Vec<RefLabel>> = HashMap::new();
    let platform = repo.references()?;
    for r in platform.all()? {
        let Ok(mut r) = r else { continue };
        let full = r.name().as_bstr().to_string();
        let (kind, name) = if let Some(n) = full.strip_prefix("refs/heads/") {
            (RefKind::Local, n)
        } else if let Some(n) = full.strip_prefix("refs/remotes/") {
            if n.ends_with("/HEAD") {
                continue; // origin/HEAD: a pointer, not a branch
            }
            (RefKind::Remote, n)
        } else if let Some(n) = full.strip_prefix("refs/tags/") {
            (RefKind::Tag, n)
        } else {
            continue;
        };
        let name = name.to_string();
        let Ok(id) = r.peel_to_id() else { continue };
        let id = id.detach();
        let kind = if kind == RefKind::Local && head_name.as_deref() == Some(full.as_str()) {
            RefKind::Head
        } else {
            kind
        };
        let branch = Branch {
            name: name.clone(),
            full,
            id,
        };
        match kind {
            RefKind::Head | RefKind::Local => branches.local.push(branch),
            RefKind::Remote => branches.remote.push(branch),
            RefKind::Tag => branches.tags.push(branch),
        }
        labels.entry(id).or_default().push(RefLabel { name, kind });
    }
    if head_name.is_none() {
        labels.entry(head_id).or_default().push(RefLabel {
            name: "HEAD".into(),
            kind: RefKind::Head,
        });
    }
    for l in labels.values_mut() {
        l.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.name.cmp(&b.name)));
    }
    for list in [
        &mut branches.local,
        &mut branches.remote,
        &mut branches.tags,
    ] {
        list.sort_by(|a, b| a.name.cmp(&b.name));
    }

    let tips: Vec<ObjectId> = match only {
        Some(full) => branches
            .local
            .iter()
            .chain(&branches.remote)
            .chain(&branches.tags)
            .filter(|b| b.full == full)
            .map(|b| b.id)
            .collect(),
        None => {
            let mut t: Vec<ObjectId> = std::iter::once(head_id)
                .chain(branches.local.iter().map(|b| b.id))
                .chain(branches.remote.iter().map(|b| b.id))
                .chain(branches.tags.iter().map(|b| b.id))
                .collect();
            t.sort();
            t.dedup();
            t
        }
    };
    // Tags can point at non-commits (a tagged tree or blob): skip those tips.
    let tips: Vec<ObjectId> = tips
        .into_iter()
        .filter(|id| {
            repo.find_object(*id)
                .is_ok_and(|o| o.kind == gix::object::Kind::Commit)
        })
        .collect();

    let walk = repo
        .rev_walk(tips)
        .sorting(gix::revision::walk::Sorting::ByCommitTime(
            gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
        ))
        .all()?;
    let mut commits = Vec::new();
    let mut truncated = false;
    for info in walk {
        if commits.len() == LIMIT {
            truncated = true;
            break;
        }
        let info = info?;
        let commit = info.object()?;
        let parents: Vec<ObjectId> = info.parent_ids().map(|p| p.detach()).collect();
        let message = commit.message_raw_sloppy().to_string();
        let (subject, body) = split_message(&message);
        let (author, email, time, offset) = match commit.author() {
            Ok(a) => {
                let t = a.time().ok();
                (
                    a.name.to_string().trim().to_string(),
                    a.email.to_string().trim().to_string(),
                    t.map_or(0, |t| t.seconds),
                    t.map_or(0, |t| t.offset),
                )
            }
            Err(_) => (String::new(), String::new(), 0, 0),
        };
        commits.push(Commit {
            id: info.id,
            parents,
            subject,
            body,
            author,
            email,
            time,
            offset,
            refs: labels.remove(&info.id).unwrap_or_default(),
        });
    }
    Ok(Some(Log {
        workdir,
        commits: topo_order(commits),
        branches,
        truncated,
    }))
}

/// Every ref and HEAD with what they point at: cheap, and it changes whenever
/// the log would (a commit, checkout, fetch, new branch or tag).
pub fn fingerprint(root: &Path) -> Option<Vec<(String, String)>> {
    pide_core::git::git_dir(root)?;
    let repo = gix::discover(root).ok()?;
    let mut out: Vec<(String, String)> = repo
        .references()
        .ok()?
        .all()
        .ok()?
        .filter_map(Result::ok)
        .map(|r| {
            let target = match r.target() {
                gix::refs::TargetRef::Object(id) => id.to_string(),
                gix::refs::TargetRef::Symbolic(name) => name.as_bstr().to_string(),
            };
            (r.name().as_bstr().to_string(), target)
        })
        .collect();
    let head = repo.head_id().map(|id| id.to_string()).unwrap_or_default();
    out.push(("HEAD".into(), head));
    out.sort();
    Some(out)
}

fn split_message(message: &str) -> (String, String) {
    let message = message.trim();
    match message.split_once('\n') {
        Some((subject, rest)) => (subject.trim().to_string(), rest.trim().to_string()),
        None => (message.to_string(), String::new()),
    }
}

/// Newest-first by time, but never a parent before one of its children (clock
/// skew would otherwise break the graph): like `git log --date-order`.
fn topo_order(commits: Vec<Commit>) -> Vec<Commit> {
    let index: HashMap<ObjectId, usize> =
        commits.iter().enumerate().map(|(i, c)| (c.id, i)).collect();
    let mut children = vec![0usize; commits.len()];
    for c in &commits {
        for p in &c.parents {
            if let Some(&i) = index.get(p) {
                children[i] += 1;
            }
        }
    }
    // Min-heap on the time-sorted position.
    let mut ready: BinaryHeap<std::cmp::Reverse<usize>> = (0..commits.len())
        .filter(|&i| children[i] == 0)
        .map(std::cmp::Reverse)
        .collect();
    let mut order = Vec::with_capacity(commits.len());
    while let Some(std::cmp::Reverse(i)) = ready.pop() {
        order.push(i);
        for p in &commits[i].parents {
            if let Some(&j) = index.get(p) {
                children[j] -= 1;
                if children[j] == 0 {
                    ready.push(std::cmp::Reverse(j));
                }
            }
        }
    }
    let mut slots: Vec<Option<Commit>> = commits.into_iter().map(Some).collect();
    order.into_iter().filter_map(|i| slots[i].take()).collect()
}

/// One row of the commit graph. Lanes are columns; a segment `(a, b)` runs
/// from lane `a` to lane `b`: `top` from the row's top edge to the commit's
/// dot, `bottom` from the dot's height to the bottom edge.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GraphRow {
    pub node: usize,
    pub top: Vec<(usize, usize)>,
    pub bottom: Vec<(usize, usize)>,
    /// Lanes in use on this row (for its width).
    pub width: usize,
}

pub fn graph(commits: &[Commit]) -> Vec<GraphRow> {
    let mut lanes: Vec<Option<ObjectId>> = Vec::new();
    let mut rows = Vec::with_capacity(commits.len());
    for c in commits {
        let mut row = GraphRow::default();
        let mut node = None;
        for (i, l) in lanes.iter().enumerate() {
            match l {
                Some(id) if *id == c.id => {
                    let n = *node.get_or_insert(i);
                    row.top.push((i, n));
                }
                Some(_) => row.top.push((i, i)),
                None => {}
            }
        }
        let node = match node {
            Some(n) => n,
            None => free_lane(&mut lanes),
        };
        row.node = node;
        for l in lanes.iter_mut() {
            if *l == Some(c.id) {
                *l = None;
            }
        }
        // Lanes going straight through this row.
        for (i, l) in lanes.iter().enumerate() {
            if l.is_some() {
                row.bottom.push((i, i));
            }
        }
        for (k, p) in c.parents.iter().enumerate() {
            // The first parent continues this lane, even when another lane also
            // leads to it: the lines meet at the parent's dot.
            if k == 0 {
                lanes[node] = Some(*p);
                row.bottom.push((node, node));
                continue;
            }
            let target = match lanes.iter().position(|l| *l == Some(*p)) {
                Some(j) => j, // another lane already leads to this parent
                None => {
                    let j = free_lane(&mut lanes);
                    lanes[j] = Some(*p);
                    j
                }
            };
            row.bottom.push((node, target));
        }
        while lanes.last().is_some_and(Option::is_none) {
            lanes.pop();
        }
        row.width = row
            .top
            .iter()
            .chain(&row.bottom)
            .map(|&(a, b)| a.max(b) + 1)
            .max()
            .unwrap_or(0)
            .max(node + 1);
        rows.push(row);
    }
    rows
}

fn free_lane(lanes: &mut Vec<Option<ObjectId>>) -> usize {
    match lanes.iter().position(Option::is_none) {
        Some(i) => i,
        None => {
            lanes.push(None);
            lanes.len() - 1
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFile {
    /// `A`, `M`, `D`, `R`, `C`, `T`.
    pub kind: char,
    /// Path after the commit (for a deletion, the deleted path).
    pub path: String,
    /// Path before, when renamed or copied.
    pub old_path: Option<String>,
    /// `None` for binary files.
    pub added: Option<u32>,
    pub deleted: Option<u32>,
}

/// What a commit changed, against its first parent (everything, for a root commit).
pub fn changed_files(workdir: &Path, commit: &Commit) -> anyhow::Result<Vec<ChangedFile>> {
    let id = commit.id.to_string();
    let parent = commit.parents.first().map(|p| p.to_string());
    changed_between(workdir, parent, id)
}

/// Like [`changed_files`], for a commit known only by its hash.
pub fn changed_files_rev(workdir: &Path, hash: &str) -> anyhow::Result<Vec<ChangedFile>> {
    let line = crate::repo::git_out(workdir, &["rev-list", "--parents", "-n1", hash])?;
    let mut ids = line.split_whitespace();
    let id = ids.next().unwrap_or(hash).to_string();
    changed_between(workdir, ids.next().map(str::to_string), id)
}

fn changed_between(
    workdir: &Path,
    parent: Option<String>,
    id: String,
) -> anyhow::Result<Vec<ChangedFile>> {
    let base: Vec<&str> = match &parent {
        Some(p) => vec![
            "diff-tree",
            "-r",
            "-M",
            "--no-commit-id",
            p.as_str(),
            id.as_str(),
        ],
        None => vec![
            "diff-tree",
            "-r",
            "-M",
            "--no-commit-id",
            "--root",
            id.as_str(),
        ],
    };
    let mut status_args = base.clone();
    status_args.insert(1, "--name-status");
    let mut num_args = base;
    num_args.insert(1, "--numstat");
    let status = crate::repo::git_out(workdir, &status_args)?;
    let numstat = crate::repo::git_out(workdir, &num_args)?;
    Ok(parse_changes(&status, &numstat))
}

fn parse_changes(name_status: &str, numstat: &str) -> Vec<ChangedFile> {
    let counts: Vec<(Option<u32>, Option<u32>)> = numstat
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let mut it = l.splitn(3, '\t');
            (
                it.next().and_then(|n| n.parse().ok()),
                it.next().and_then(|n| n.parse().ok()),
            )
        })
        .collect();
    name_status
        .lines()
        .filter(|l| !l.is_empty())
        .enumerate()
        .filter_map(|(i, l)| {
            let mut parts = l.split('\t');
            let kind = parts.next()?.chars().next()?;
            let first = parts.next()?.to_string();
            let (path, old_path) = match parts.next() {
                Some(new) => (new.to_string(), Some(first)),
                None => (first, None),
            };
            let (added, deleted) = counts.get(i).copied().unwrap_or((None, None));
            Some(ChangedFile {
                kind,
                path,
                old_path,
                added,
                deleted,
            })
        })
        .collect()
}

/// A file's text in a commit (`None` when it doesn't exist there or isn't text).
pub fn file_at(workdir: &Path, rev: &str, path: &str) -> Option<String> {
    let bytes = crate::repo::git_bytes(workdir, &["show", &format!("{rev}:{path}")]).ok()?;
    String::from_utf8(bytes).ok()
}

/// "Today 09:12", "Yesterday 22:30", "Mon 14:05" (this week), else "2026-11-03",
/// in the author's own time zone.
pub fn when(time: i64, offset: i32, now: i64) -> String {
    let local = time + offset as i64;
    let today = (now + offset as i64).div_euclid(86_400);
    let day = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let clock = format!("{:02}:{:02}", secs / 3600, secs % 3600 / 60);
    match today - day {
        0 => format!("Today {clock}"),
        1 => format!("Yesterday {clock}"),
        2..=6 => {
            // 1970-01-01 was a Thursday.
            let names = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
            format!("{} {clock}", names[day.rem_euclid(7) as usize])
        }
        _ => {
            let (y, m, d) = civil(day);
            format!("{y}-{m:02}-{d:02}")
        }
    }
}

/// Full date and time for the details pane: "2026-11-10 09:12 +09:00".
pub fn when_full(time: i64, offset: i32) -> String {
    let local = time + offset as i64;
    let (y, m, d) = civil(local.div_euclid(86_400));
    let secs = local.rem_euclid(86_400);
    let sign = if offset < 0 { '-' } else { '+' };
    let o = offset.unsigned_abs();
    format!(
        "{y}-{m:02}-{d:02} {:02}:{:02} {sign}{:02}:{:02}",
        secs / 3600,
        secs % 3600 / 60,
        o / 3600,
        o % 3600 / 60
    )
}

/// Days since 1970-01-01 → (year, month, day). Howard Hinnant's algorithm.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> ObjectId {
        ObjectId::from_bytes_or_panic(&[n; 20])
    }

    fn c(n: u8, parents: &[u8]) -> Commit {
        Commit {
            id: id(n),
            parents: parents.iter().map(|&p| id(p)).collect(),
            subject: n.to_string(),
            body: String::new(),
            author: String::new(),
            email: String::new(),
            time: 0,
            offset: 0,
            refs: Vec::new(),
        }
    }

    #[test]
    fn graph_branch_and_merge() {
        // 5 merges 4 (side branch from 2) into 3; 3 → 2 → 1.
        let commits = [c(5, &[3, 4]), c(4, &[2]), c(3, &[2]), c(2, &[1]), c(1, &[])];
        let rows = graph(&commits);
        let nodes: Vec<usize> = rows.iter().map(|r| r.node).collect();
        assert_eq!(nodes, [0, 1, 0, 0, 0]);
        assert_eq!(rows[0].bottom, [(0, 0), (0, 1)]); // merge opens lane 1
        assert_eq!(rows[1].top, [(0, 0), (1, 1)]);
        assert_eq!(rows[1].bottom, [(0, 0), (1, 1)]);
        assert_eq!(rows[2].bottom, [(1, 1), (0, 0)]);
        // 2 is expected by both lanes: lane 1 joins lane 0.
        assert_eq!(rows[3].top, [(0, 0), (1, 0)]);
        assert_eq!(rows[3].bottom, [(0, 0)]);
        assert!(rows[4].bottom.is_empty());
        assert_eq!(rows[0].width, 2);
        assert_eq!(rows[4].width, 1);
    }

    #[test]
    fn graph_two_tips() {
        // Two branch tips (3 and 2) on the same parent 1.
        let rows = graph(&[c(3, &[1]), c(2, &[1]), c(1, &[])]);
        assert_eq!(rows[0].node, 0);
        assert_eq!(rows[1].node, 1);
        assert_eq!(rows[1].top, [(0, 0)]);
        assert_eq!(rows[1].bottom, [(0, 0), (1, 1)]);
        assert_eq!(rows[2].top, [(0, 0), (1, 0)]); // the lines meet at 1
        assert_eq!(rows[2].width, 2);
    }

    #[test]
    fn topo_order_fixes_clock_skew() {
        // Parent 1 sorted (by time) before its child 2.
        let out = topo_order(vec![c(3, &[2]), c(1, &[]), c(2, &[1])]);
        let order: Vec<String> = out.iter().map(|c| c.subject.clone()).collect();
        assert_eq!(order, ["3", "2", "1"]);
    }

    #[test]
    fn dates() {
        let now = 1_794_268_800 + 9 * 3600; // 2026-11-10 09:00 UTC
        assert_eq!(when(now - 600, 0, now), "Today 08:50");
        assert_eq!(when(now - 86_400, 0, now), "Yesterday 09:00");
        assert_eq!(when(now - 30 * 86_400, 0, now), "2026-10-11");
        assert_eq!(when_full(now, 9 * 3600), "2026-11-10 18:00 +09:00");
        assert_eq!(civil(0), (1970, 1, 1));
    }

    fn sh(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "Ann")
            .env("GIT_AUTHOR_EMAIL", "ann@x")
            .env("GIT_COMMITTER_NAME", "Ann")
            .env("GIT_COMMITTER_EMAIL", "ann@x")
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn read_a_repository() {
        let dir = std::env::temp_dir().join(format!("pide-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(read(&dir, None).unwrap(), None);
        sh(&dir, &["init", "-q", "-b", "main"]);
        sh(&dir, &["config", "commit.gpgsign", "false"]);
        sh(&dir, &["config", "tag.gpgsign", "false"]);
        assert_eq!(read(&dir, None).unwrap(), None); // no commits yet

        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        sh(&dir, &["add", "."]);
        sh(&dir, &["commit", "-q", "-m", "first\n\nthe body"]);
        sh(&dir, &["tag", "v1"]);
        sh(&dir, &["checkout", "-q", "-b", "side"]);
        std::fs::write(dir.join("b.txt"), "side\n").unwrap();
        sh(&dir, &["add", "."]);
        sh(&dir, &["commit", "-q", "-m", "on side"]);
        sh(&dir, &["checkout", "-q", "main"]);
        sh(&dir, &["mv", "a.txt", "c.txt"]);
        std::fs::write(dir.join("c.txt"), "one\ntwo\n").unwrap();
        sh(&dir, &["add", "."]);
        sh(&dir, &["commit", "-q", "-m", "rename"]);
        sh(
            &dir,
            &["merge", "-q", "--no-ff", "side", "-m", "merge side"],
        );

        let log = read(&dir, None).unwrap().unwrap();
        let subjects: Vec<&str> = log.commits.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects.len(), 4);
        assert_eq!(subjects[0], "merge side");
        assert_eq!(subjects[3], "first");
        assert_eq!(log.commits[3].body, "the body");
        assert_eq!(log.commits[0].author, "Ann");
        assert_eq!(log.commits[0].parents.len(), 2);
        assert_eq!(
            log.commits[0].refs,
            [RefLabel {
                name: "main".into(),
                kind: RefKind::Head
            }]
        );
        assert_eq!(log.commits[3].refs[0].name, "v1");
        assert_eq!(log.branches.head.as_deref(), Some("main"));
        let names: Vec<&str> = log.branches.local.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["main", "side"]);
        assert_eq!(graph(&log.commits)[0].width, 2);

        // Only one branch.
        let side = read(&dir, Some("refs/heads/side")).unwrap().unwrap();
        let subjects: Vec<&str> = side.commits.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects, ["on side", "first"]);

        // What "rename" changed, and the file before and after.
        let rename = log.commits.iter().find(|c| c.subject == "rename").unwrap();
        let files = changed_files(&log.workdir, rename).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!((files[0].kind, files[0].path.as_str()), ('R', "c.txt"));
        assert_eq!(files[0].old_path.as_deref(), Some("a.txt"));
        assert_eq!(files[0].added, Some(1));
        let parent = rename.parents[0].to_string();
        assert_eq!(
            file_at(&log.workdir, &parent, "a.txt").as_deref(),
            Some("one\n")
        );
        assert_eq!(
            file_at(&log.workdir, &rename.id.to_string(), "c.txt").as_deref(),
            Some("one\ntwo\n")
        );
        assert_eq!(file_at(&log.workdir, &parent, "c.txt"), None);
        // A root commit lists everything it added.
        let first = changed_files(&log.workdir, &log.commits[3]).unwrap();
        assert_eq!((first[0].kind, first[0].path.as_str()), ('A', "a.txt"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn changes_parse() {
        let status = "M\tsrc/main.rs\nR087\told.rs\tnew.rs\nA\timg.png\n";
        let numstat = "2\t1\tsrc/main.rs\n3\t0\t{old.rs => new.rs}\n-\t-\timg.png\n";
        let files = parse_changes(status, numstat);
        assert_eq!(files.len(), 3);
        assert_eq!(
            (files[0].kind, files[0].added, files[0].deleted),
            ('M', Some(2), Some(1))
        );
        assert_eq!(files[1].path, "new.rs");
        assert_eq!(files[1].old_path.as_deref(), Some("old.rs"));
        assert_eq!(files[2].added, None);
    }
}
