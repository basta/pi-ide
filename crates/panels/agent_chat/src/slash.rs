//! Slash-command suggestions above the composer: typing `/` lists the
//! session's commands (pi's extension commands, prompt templates, skills).

use pide_core::SlashCommand;

/// What's being typed, if the composer holds a command name in progress
/// (`/`, `/sea`); `None` once there's a space or anything before the slash.
pub(crate) fn typed(text: &str) -> Option<&str> {
    let rest = text.strip_prefix('/')?;
    (!rest.contains(char::is_whitespace)).then_some(rest)
}

/// Commands matching `query`: names starting with it first, then containing it.
pub(crate) fn matches(commands: &[SlashCommand], query: &str) -> Vec<usize> {
    let q = query.to_lowercase();
    let mut starts = Vec::new();
    let mut contains = Vec::new();
    for (i, c) in commands.iter().enumerate() {
        let name = c.name.to_lowercase();
        if name.starts_with(&q) {
            starts.push(i);
        } else if name.contains(&q) {
            contains.push(i);
        }
    }
    starts.extend(contains);
    starts.truncate(8);
    starts
}

/// A suggestion under the composer: what it shows, and the composer text after
/// choosing it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Suggestion {
    pub label: String,
    pub detail: String,
    pub tag: String,
    pub replace_with: String,
}

/// An `@file` mention being typed at the end of the text: (byte start of the
/// `@`, what follows it).
pub(crate) fn mention(text: &str) -> Option<(usize, &str)> {
    let start = text
        .rfind(|c: char| c.is_whitespace())
        .map(|i| i + 1)
        .unwrap_or(0);
    let token = &text[start..];
    let q = token.strip_prefix('@')?;
    (!q.contains('@')).then_some((start, q))
}

/// Project files matching a mention: file name starts first, then path contains.
pub(crate) fn file_matches(files: &[String], query: &str, limit: usize) -> Vec<usize> {
    let q = query.to_lowercase();
    let mut name_hits = Vec::new();
    let mut path_hits = Vec::new();
    for (i, f) in files.iter().enumerate() {
        let lower = f.to_lowercase();
        let name = lower.rsplit('/').next().unwrap_or(&lower);
        if name.starts_with(&q) {
            name_hits.push(i);
        } else if lower.contains(&q) {
            path_hits.push(i);
        }
        if name_hits.len() >= limit {
            break;
        }
    }
    name_hits.extend(path_hits);
    name_hits.truncate(limit);
    name_hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(name: &str) -> SlashCommand {
        SlashCommand {
            name: name.into(),
            description: String::new(),
            source: "extension".into(),
        }
    }

    #[::core::prelude::v1::test]
    fn suggests_while_typing_a_name() {
        assert_eq!(typed("/"), Some(""));
        assert_eq!(typed("/sea"), Some("sea"));
        assert_eq!(typed("/search-setup now"), None);
        assert_eq!(typed("hi /x"), None);
        let list = [
            c("search-setup"),
            c("mcp"),
            c("skill:research"),
            c("search-status"),
        ];
        assert_eq!(matches(&list, "sea"), vec![0, 3, 2]);
        assert_eq!(matches(&list, ""), vec![0, 1, 2, 3]);
        assert!(matches(&list, "zzz").is_empty());
    }

    #[::core::prelude::v1::test]
    fn mentions_at_the_end() {
        assert_eq!(mention("look at @src/ma"), Some((8, "src/ma")));
        assert_eq!(mention("@"), Some((0, "")));
        assert_eq!(mention("mail me@x"), None);
        assert_eq!(mention("@a.rs and more"), None);
        let files = ["src/main.rs", "README.md", "crates/x/main.py", "domain.rs"].map(String::from);
        assert_eq!(file_matches(&files, "main", 10), vec![0, 2, 3]);
        assert_eq!(file_matches(&files, "x/", 10), vec![2]);
    }
}
