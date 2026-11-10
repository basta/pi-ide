//! The Commit window's logic without UI: a file's changes against HEAD as
//! hunks, the content to commit when only some are checked, and drafting a
//! commit message with a one-off `pi -p` call. Blocking: call off the UI thread.

use std::path::Path;

use anyhow::{Context as _, bail};
use pide_core::changes::Hunk;

/// A text file's HEAD and working-tree versions, and the changes between them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileHunks {
    pub before: String,
    pub after: String,
    pub hunks: Vec<Hunk>,
}

/// `None` for files that aren't UTF-8 text (those are committed whole).
pub fn load(abs: &Path) -> Option<FileHunks> {
    let before = pide_core::git::head_text(abs).unwrap_or_default();
    let after = match std::fs::read(abs) {
        Ok(bytes) => String::from_utf8(bytes).ok()?,
        Err(_) => String::new(), // deleted
    };
    let hunks = pide_core::changes::hunks(&before, &after);
    Some(FileHunks {
        before,
        after,
        hunks,
    })
}

/// The file with only the checked changes applied to its HEAD version.
pub fn partial_content(f: &FileHunks, include: &[bool]) -> String {
    let mut text = f.after.clone();
    // Last first: undoing a later change leaves earlier line numbers valid.
    for (i, h) in f.hunks.iter().enumerate().rev() {
        if !include.get(i).copied().unwrap_or(true) {
            text = pide_core::changes::revert_hunk(&f.before, &text, h);
        }
    }
    text
}

/// The lines a change removes and adds, for its preview under the file.
pub fn hunk_lines(f: &FileHunks, h: &Hunk) -> Vec<(char, String)> {
    let old: Vec<&str> = f.before.lines().collect();
    let new: Vec<&str> = f.after.lines().collect();
    let mut out: Vec<(char, String)> = old
        .get(h.before.clone())
        .unwrap_or(&[])
        .iter()
        .map(|l| ('-', l.to_string()))
        .collect();
    out.extend(
        new.get(h.after.clone())
            .unwrap_or(&[])
            .iter()
            .map(|l| ('+', l.to_string())),
    );
    out
}

/// A unified diff of one file, for pi.
pub fn unified(rel: &str, before: &str, after: &str) -> String {
    similar::TextDiff::from_lines(before, after)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{rel}"), &format!("b/{rel}"))
        .to_string()
}

/// Diffs bigger than this are cut (pi needs the gist, not every line).
const MAX_DIFF: usize = 40_000;

pub fn draft_prompt(diff: &str, recent_subjects: &[String]) -> String {
    let mut diff = diff.to_string();
    if diff.len() > MAX_DIFF {
        let mut cut = MAX_DIFF;
        while !diff.is_char_boundary(cut) {
            cut -= 1;
        }
        diff.truncate(cut);
        diff.push_str("\n… (diff cut here)\n");
    }
    let style = if recent_subjects.is_empty() {
        String::new()
    } else {
        format!(
            "Match the style of this repository's recent commit subjects:\n{}\n\n",
            recent_subjects
                .iter()
                .map(|s| format!("- {s}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    format!(
        "Write a git commit message for the diff below. First line: a short summary \
         (under 72 characters). Then, only if it helps, a blank line and a brief body \
         saying what changed and why. {style}Reply with only the commit message, no \
         code fences, no quotes.\n\n{diff}"
    )
}

/// Ask pi once (`pi -p`, no session, no tools) and return its answer.
pub fn ask_pi(cwd: &Path, prompt: &str) -> anyhow::Result<String> {
    let pi = pide_runtime::process::find_command("pi").context("pi isn't installed")?;
    let out = std::process::Command::new(pi)
        .args(["-p", "--no-session", "--no-tools", "--thinking", "low"])
        .arg(prompt)
        .current_dir(cwd)
        .env(
            "PATH",
            std::env::join_paths(pide_runtime::process::search_path()).unwrap_or_default(),
        )
        .stdin(std::process::Stdio::null())
        .output()
        .context("run pi")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
        bail!("pi failed: {last}");
    }
    let text = clean_reply(&String::from_utf8_lossy(&out.stdout));
    if text.is_empty() {
        bail!("pi gave no answer");
    }
    Ok(text)
}

/// Strip a wrapping code fence and surrounding blank lines.
fn clean_reply(s: &str) -> String {
    let t = s.trim();
    let t = match t.strip_prefix("```") {
        Some(rest) => {
            let rest = rest.split_once('\n').map_or("", |(_, r)| r);
            rest.trim_end().strip_suffix("```").unwrap_or(rest)
        }
        None => t,
    };
    t.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fh(before: &str, after: &str) -> FileHunks {
        FileHunks {
            before: before.into(),
            after: after.into(),
            hunks: pide_core::changes::hunks(before, after),
        }
    }

    #[test]
    fn partial() {
        let f = fh("a\nb\nc\nd\ne\n", "A\nb\nc\nd\nE\nF\n");
        assert_eq!(f.hunks.len(), 2);
        assert_eq!(partial_content(&f, &[true, true]), f.after);
        assert_eq!(partial_content(&f, &[false, false]), f.before);
        assert_eq!(partial_content(&f, &[true, false]), "A\nb\nc\nd\ne\n");
        assert_eq!(partial_content(&f, &[false, true]), "a\nb\nc\nd\nE\nF\n");
        assert_eq!(
            hunk_lines(&f, &f.hunks[1]),
            [('-', "e".into()), ('+', "E".into()), ('+', "F".into())]
        );
    }

    #[test]
    fn replies() {
        assert_eq!(clean_reply("```\nFix it\n```\n"), "Fix it");
        assert_eq!(clean_reply("```text\nFix it\n\nBody\n```"), "Fix it\n\nBody");
        assert_eq!(clean_reply("  Fix it \n"), "Fix it");
    }

    #[test]
    fn prompt_cuts_big_diffs() {
        let p = draft_prompt(&"x".repeat(MAX_DIFF * 2), &["Add a".into()]);
        assert!(p.len() < MAX_DIFF + 1000);
        assert!(p.contains("- Add a"));
        assert!(p.contains("diff cut here"));
    }
}
