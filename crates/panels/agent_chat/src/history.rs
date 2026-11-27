//! Reading a pi session file back into the chat (off the UI thread), and tool-call display text.

use super::*;

/// Plain (`Send`) history parsed from a pi session file off the UI thread.
pub(crate) enum HistItem {
    User(String),
    Text(String),
    Tool {
        id: String,
        name: String,
        summary: String,
        output: String,
        failed: bool,
    },
    /// The session's model / thinking level as last recorded (shown before pi starts).
    Model {
        provider: String,
        id: String,
    },
    Thinking(String),
    /// An `ide_pin` call (its tool call id); the pin itself is saved with the session.
    Pin(String),
    /// An `ide_tour` call (its tool call id).
    Tour(String),
}

/// One-line description of a tool call's arguments.
pub(crate) fn summarize_args(name: &str, args: &Value) -> String {
    let field = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
    let s = match name {
        "bash" => field("command"),
        "read" | "write" | "edit" | "ls" => field("path"),
        "grep" | "find" => field("pattern"),
        // Other tools: a path or paths if they take one, else their arguments.
        _ => field("path").or_else(|| {
            let paths: Vec<&str> = args
                .get("paths")?
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .collect();
            Some(if paths.is_empty() {
                "all files".to_string()
            } else {
                paths.join(", ")
            })
        }),
    }
    .unwrap_or_else(|| args.to_string());
    let s = s.replace('\n', " ⏎ ");
    if s.chars().count() > 160 {
        format!("{}…", s.chars().take(160).collect::<String>())
    } else {
        s
    }
}

/// The text of a tool result (`content` blocks), clipped for display: its
/// first lines, or its last ones (`tail`: a failure's error, a running
/// command's progress).
pub(crate) fn result_text(result: &Value, tail: bool) -> String {
    let text = result
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|| result.to_string());
    let lines: Vec<&str> = text.lines().collect();
    if tail && lines.len() > 8 {
        format!(
            "… {} lines before\n{}",
            lines.len() - 8,
            lines[lines.len() - 8..].join("\n")
        )
    } else if lines.len() > 8 {
        format!(
            "{}\n… {} more lines",
            lines[..8].join("\n"),
            lines.len() - 8
        )
    } else {
        text.trim_end().to_string()
    }
}

/// Conversation history from a pi session file (`docs/session-format.md`).
/// Branches are flattened in file order — good enough to read back.
pub(crate) fn parse_history(path: &std::path::Path) -> Vec<HistItem> {
    use std::io::BufRead;
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let entries: Vec<Value> = std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<Value>(&l).ok())
        .collect();
    let mut out: Vec<HistItem> = Vec::new();
    for v in pide_core::sessions::current_branch(entries) {
        match v.get("type").and_then(Value::as_str) {
            Some("message") => {}
            Some("model_change") => {
                if let (Some(p), Some(m)) = (v["provider"].as_str(), v["modelId"].as_str()) {
                    out.push(HistItem::Model {
                        provider: p.into(),
                        id: m.into(),
                    });
                }
                continue;
            }
            Some("thinking_level_change") => {
                if let Some(t) = v["thinkingLevel"].as_str() {
                    out.push(HistItem::Thinking(t.into()));
                }
                continue;
            }
            _ => continue,
        }
        let m = &v["message"];
        match m.get("role").and_then(Value::as_str) {
            Some("user") => {
                if let Some(t) = user_text(m) {
                    out.push(HistItem::User(t));
                }
            }
            Some("assistant") => {
                for block in m
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            let t = block
                                .get("text")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            if !t.trim().is_empty() {
                                out.push(HistItem::Text(t.to_string()));
                            }
                        }
                        Some("toolCall") => {
                            let name = block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string();
                            let id = block.get("id").and_then(Value::as_str).unwrap_or_default();
                            // pide's own tools show as cards of their own (or not at all).
                            if name == "ide_pin" {
                                out.push(HistItem::Pin(id.to_string()));
                                continue;
                            }
                            if name == "ide_tour" {
                                out.push(HistItem::Tour(id.to_string()));
                                continue;
                            }
                            if name.starts_with("ide_") && name != "ide_diagnostics" {
                                continue;
                            }
                            out.push(HistItem::Tool {
                                id: block
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                                summary: summarize_args(&name, &block["arguments"]),
                                name,
                                output: String::new(),
                                failed: false,
                            });
                        }
                        _ => {}
                    }
                }
            }
            Some("toolResult") => {
                let id = m
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let failed = m.get("isError").and_then(Value::as_bool).unwrap_or(false);
                let text = result_text(m, failed);
                if let Some(HistItem::Tool {
                    output, failed: f, ..
                }) = out
                    .iter_mut()
                    .rev()
                    .find(|h| matches!(h, HistItem::Tool { id: tid, .. } if tid == id))
                {
                    *output = text;
                    *f = failed;
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn history_follows_the_current_branch() {
        let e = |id: &str, parent: Option<&str>| serde_json::json!({ "type": "message", "id": id, "parentId": parent });
        // a → b → c, then the user went back to b and continued with d → e.
        let entries = vec![
            serde_json::json!({ "type": "session" }),
            e("a", None),
            e("b", Some("a")),
            e("c", Some("b")),
            e("d", Some("b")),
            e("e", Some("d")),
        ];
        let ids: Vec<String> = pide_core::sessions::current_branch(entries)
            .iter()
            .map(|v| {
                v.get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("-")
                    .to_string()
            })
            .collect();
        assert_eq!(ids, ["-", "a", "b", "d", "e"]);
        // No ids at all: unchanged.
        let plain = vec![serde_json::json!({ "x": 1 }), serde_json::json!({ "x": 2 })];
        assert_eq!(pide_core::sessions::current_branch(plain.clone()), plain);
    }
}
