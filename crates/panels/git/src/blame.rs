//! `git blame` for a file, line by line (who last changed each line, in which
//! commit), parsed from `--porcelain`. Lines not committed yet have a zero
//! hash. Blocking: call off the UI thread.

use std::collections::HashMap;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlameCommit {
    pub hash: String,
    pub author: String,
    pub time: i64,
    pub offset: i32,
    pub summary: String,
    /// The file's path in that commit (it may have been renamed since).
    pub filename: String,
    pub previous: Option<String>,
}

impl BlameCommit {
    pub fn uncommitted(&self) -> bool {
        self.hash.bytes().all(|b| b == b'0')
    }
    pub fn short(&self) -> &str {
        &self.hash[..self.hash.len().min(7)]
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlameLine {
    /// Index into [`Blame::commits`].
    pub commit: usize,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Blame {
    pub commits: Vec<BlameCommit>,
    pub lines: Vec<BlameLine>,
}

/// Blame the file as it is on disk (`rel` from the repository's working tree).
pub fn blame(workdir: &Path, rel: &str) -> anyhow::Result<Blame> {
    let out = crate::repo::git_out(workdir, &["blame", "--porcelain", "--", rel])?;
    Ok(parse(&out))
}

/// Who last changed one line (1-based).
pub fn blame_line(workdir: &Path, rel: &str, line: u32) -> anyhow::Result<(BlameCommit, String)> {
    let range = format!("{line},{line}");
    let out = crate::repo::git_out(workdir, &["blame", "--porcelain", "-L", &range, "--", rel])?;
    let b = parse(&out);
    let l = b
        .lines
        .first()
        .ok_or_else(|| anyhow::anyhow!("no line {line}"))?;
    Ok((b.commits[l.commit].clone(), l.text.clone()))
}

fn parse(out: &str) -> Blame {
    let mut b = Blame::default();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut current: Option<usize> = None;
    for line in out.lines() {
        if let Some(text) = line.strip_prefix('\t') {
            if let Some(c) = current {
                b.lines.push(BlameLine {
                    commit: c,
                    text: text.to_string(),
                });
            }
            continue;
        }
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        if key.len() == 40 && key.bytes().all(|c| c.is_ascii_hexdigit()) {
            let c = *index.entry(key.to_string()).or_insert_with(|| {
                b.commits.push(BlameCommit {
                    hash: key.to_string(),
                    author: String::new(),
                    time: 0,
                    offset: 0,
                    summary: String::new(),
                    filename: String::new(),
                    previous: None,
                });
                b.commits.len() - 1
            });
            current = Some(c);
            continue;
        }
        let Some(c) = current.and_then(|c| b.commits.get_mut(c)) else {
            continue;
        };
        match key {
            "author" => c.author = value.to_string(),
            "author-time" => c.time = value.parse().unwrap_or(0),
            "author-tz" => c.offset = tz(value),
            "summary" => c.summary = value.to_string(),
            "filename" => c.filename = value.to_string(),
            "previous" => c.previous = value.split(' ').next().map(str::to_string),
            _ => {}
        }
    }
    b
}

/// "+0900" → seconds east of UTC.
fn tz(s: &str) -> i32 {
    let (sign, d) = match s.split_at_checked(1) {
        Some(("-", d)) => (-1, d),
        Some((_, d)) => (1, d),
        None => return 0,
    };
    let h: i32 = d.get(..2).and_then(|x| x.parse().ok()).unwrap_or(0);
    let m: i32 = d.get(2..4).and_then(|x| x.parse().ok()).unwrap_or(0);
    sign * (h * 3600 + m * 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUT: &str = "\
1111111111111111111111111111111111111111 1 1 2
author Ann
author-time 1794000000
author-tz +0900
summary First
filename a.txt
\tone
1111111111111111111111111111111111111111 2 2
\ttwo
0000000000000000000000000000000000000000 3 3 1
author Not Committed Yet
author-time 1794100000
author-tz -0530
summary Version of a.txt from a.txt
previous 1111111111111111111111111111111111111111 a.txt
filename a.txt
\tthree
";

    #[test]
    fn porcelain() {
        let b = parse(OUT);
        assert_eq!(b.commits.len(), 2);
        assert_eq!(b.lines.len(), 3);
        assert_eq!(b.lines[1].commit, 0);
        assert_eq!(b.lines[1].text, "two");
        assert_eq!(b.commits[0].author, "Ann");
        assert_eq!(b.commits[0].offset, 9 * 3600);
        assert_eq!(b.commits[0].short(), "1111111");
        assert!(b.commits[1].uncommitted());
        assert_eq!(b.commits[1].offset, -(5 * 3600 + 30 * 60));
        assert_eq!(b.commits[1].previous.as_deref(), Some("1111111111111111111111111111111111111111"));
    }

    #[test]
    fn real_repository() {
        let dir = std::env::temp_dir().join(format!("pide-blame-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sh = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(&dir)
                    .args(args)
                    .env("GIT_AUTHOR_NAME", "Ann")
                    .env("GIT_AUTHOR_EMAIL", "a@x")
                    .env("GIT_COMMITTER_NAME", "Ann")
                    .env("GIT_COMMITTER_EMAIL", "a@x")
                    .status()
                    .unwrap()
                    .success()
            )
        };
        sh(&["init", "-q", "-b", "main"]);
        sh(&["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        sh(&["add", "."]);
        sh(&["commit", "-q", "-m", "first"]);
        std::fs::write(dir.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        let b = blame(&dir, "a.txt").unwrap();
        let texts: Vec<&str> = b.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["one", "TWO", "three"]);
        assert_eq!(b.commits[b.lines[0].commit].summary, "first");
        assert!(b.commits[b.lines[1].commit].uncommitted());
        let (c, text) = blame_line(&dir, "a.txt", 1).unwrap();
        assert_eq!((c.summary.as_str(), text.as_str()), ("first", "one"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
