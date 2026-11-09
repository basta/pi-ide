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
}
