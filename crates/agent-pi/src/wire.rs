//! Wire records → backend-neutral [`AgentEvent`]s. Pure; tested on fixtures.

use pide_core::{AgentEvent, DialogKind, DialogRequest};
use serde_json::Value;

/// One parsed stdout record.
#[derive(Debug, Clone)]
pub enum Inbound {
    Event(AgentEvent),
    Response {
        id: Option<String>,
        command: String,
        success: bool,
        error: Option<String>,
        data: Value,
    },
    /// A blocking extension dialog (`select`/`confirm`/`input`/`editor`): pi waits for
    /// an `extension_ui_response` (or its own `timeout`).
    Dialog(DialogRequest),
    /// pide's extension asking pide something (`ctx.ui.input("pide:query", <json>)`,
    /// docs/COLLAB.md): answered at once with an `extension_ui_response`, never shown.
    Query {
        id: String,
        payload: Value,
    },
}

/// The dialog title pide's extension uses for questions to pide (`ext/pide.ts`).
pub const QUERY_TITLE: &str = "pide:query";

/// Records we know about and deliberately don't surface (yet).
const QUIET: &[&str] = &[
    "agent_start",
    "agent_end",
    "turn_start",
    "turn_end",
    "entry_appended",
    "session_info_changed",
    "thinking_level_changed",
    "compaction_start",
    "compaction_end",
    "auto_retry_start",
    "auto_retry_end",
    "summarization_retry_scheduled",
    "summarization_retry_attempt_start",
    "summarization_retry_finished",
    "bash_execution_update",
];

fn s(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn idx(v: &Value) -> usize {
    v.get("contentIndex").and_then(Value::as_u64).unwrap_or(0) as usize
}

/// Text of a user message: `content` is a string or an array of blocks.
fn message_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(t)) => t.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Parse one stdout line. Unknown or malformed input becomes `Unknown`; never panics.
pub fn parse_line(line: &str) -> Vec<Inbound> {
    let v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => {
            return vec![Inbound::Event(AgentEvent::Unknown(Value::String(
                line.to_string(),
            )))];
        }
    };
    let ty = v.get("type").and_then(Value::as_str).unwrap_or_default();
    let ev = |e| vec![Inbound::Event(e)];
    match ty {
        "response" => vec![Inbound::Response {
            id: v.get("id").and_then(Value::as_str).map(str::to_string),
            command: s(&v, "command"),
            success: v.get("success").and_then(Value::as_bool).unwrap_or(false),
            error: v.get("error").and_then(Value::as_str).map(str::to_string),
            data: v.get("data").cloned().unwrap_or(Value::Null),
        }],
        "message_start" => {
            let m = v.get("message").cloned().unwrap_or_default();
            match m.get("role").and_then(Value::as_str) {
                Some("assistant") => ev(AgentEvent::AssistantStart),
                Some("user") => ev(AgentEvent::UserPrompt {
                    text: message_text(&m),
                }),
                // system prompt, tool results (covered by tool_execution_*), custom: not shown
                _ => vec![],
            }
        }
        "message_update" => {
            let e = v.get("assistantMessageEvent").cloned().unwrap_or_default();
            match e.get("type").and_then(Value::as_str).unwrap_or_default() {
                "text_delta" => ev(AgentEvent::TextDelta {
                    index: idx(&e),
                    delta: s(&e, "delta"),
                }),
                "text_end" => ev(AgentEvent::TextEnd {
                    index: idx(&e),
                    content: s(&e, "content"),
                }),
                "thinking_delta" => ev(AgentEvent::ThinkingDelta {
                    index: idx(&e),
                    delta: s(&e, "delta"),
                }),
                "thinking_end" => ev(AgentEvent::ThinkingEnd {
                    index: idx(&e),
                    content: s(&e, "content"),
                }),
                // starts, tool-call argument streaming: shown via tool_execution_* instead
                _ => vec![],
            }
        }
        "message_end" => {
            let m = v.get("message").cloned().unwrap_or_default();
            if m.get("role").and_then(Value::as_str) != Some("assistant") {
                return vec![];
            }
            let stop = s(&m, "stopReason");
            let error = matches!(stop.as_str(), "error" | "aborted").then(|| {
                m.get("errorMessage")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or(stop)
            });
            ev(AgentEvent::AssistantEnd { error })
        }
        "tool_execution_start" => ev(AgentEvent::ToolStart {
            id: s(&v, "toolCallId"),
            name: s(&v, "toolName"),
            args: v.get("args").cloned().unwrap_or_default(),
        }),
        "tool_execution_update" => ev(AgentEvent::ToolUpdate {
            id: s(&v, "toolCallId"),
            partial: v.get("partialResult").cloned().unwrap_or_default(),
        }),
        "tool_execution_end" => {
            let id = s(&v, "toolCallId");
            let result = v.get("result").cloned().unwrap_or_default();
            let is_error = v.get("isError").and_then(Value::as_bool).unwrap_or(false);
            let collab = (!is_error)
                .then(|| collab_event(&id, &result["details"]))
                .flatten();
            let mut out = ev(AgentEvent::ToolEnd {
                id,
                name: s(&v, "toolName"),
                result,
                is_error,
                duration_ms: v.get("durationMs").and_then(Value::as_u64),
            });
            out.extend(collab.map(Inbound::Event));
            out
        }
        "queue_update" => {
            let list = |k: &str| -> Vec<String> {
                v.get(k)
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default()
            };
            ev(AgentEvent::Queue {
                steering: list("steering"),
                follow_up: list("followUp"),
            })
        }
        "agent_settled" => ev(AgentEvent::Settled {
            aborted: v.get("aborted").and_then(Value::as_bool).unwrap_or(false),
        }),
        "extension_error" => ev(AgentEvent::Error(format!(
            "extension {} failed in {}: {}",
            s(&v, "extensionPath"),
            s(&v, "event"),
            s(&v, "error")
        ))),
        "extension_ui_request" => match s(&v, "method").as_str() {
            "setStatus" => ev(AgentEvent::Status {
                key: s(&v, "statusKey"),
                text: v
                    .get("statusText")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            }),
            "notify" => ev(AgentEvent::Notify {
                message: s(&v, "message"),
                error: s(&v, "notifyType") == "error",
            }),
            "input" if s(&v, "title") == QUERY_TITLE => vec![Inbound::Query {
                id: s(&v, "id"),
                payload: v
                    .get("placeholder")
                    .and_then(Value::as_str)
                    .and_then(|p| serde_json::from_str(p).ok())
                    .unwrap_or(Value::Null),
            }],
            m @ ("select" | "confirm" | "input" | "editor") => {
                let opt = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
                vec![Inbound::Dialog(DialogRequest {
                    id: s(&v, "id"),
                    kind: match m {
                        "select" => DialogKind::Select,
                        "confirm" => DialogKind::Confirm,
                        "input" => DialogKind::Input,
                        _ => DialogKind::Editor,
                    },
                    title: s(&v, "title"),
                    message: opt("message"),
                    options: v
                        .get("options")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default(),
                    placeholder: opt("placeholder"),
                    prefill: opt("prefill"),
                    timeout_ms: v.get("timeout").and_then(Value::as_u64),
                })]
            }
            // setWidget, setTitle, set_editor_text: M2
            _ => vec![],
        },
        t if QUIET.contains(&t) => vec![],
        _ => ev(AgentEvent::Unknown(v)),
    }
}

/// A pide extension tool's result `details` (`ext/pide.ts`, docs/COLLAB.md) as
/// an event. Paths are as the extension sent them (relative to the project when
/// inside it); the connection resolves them against pi's working directory.
pub fn collab_event(tool_call_id: &str, d: &Value) -> Option<AgentEvent> {
    use pide_core::collab::{Anchor, HunkNote, Pin, ProgressStep};
    if d.get("pide").and_then(Value::as_u64) != Some(1) {
        return None;
    }
    let line = |v: &Value, k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(1).max(1) as usize;
    match d.get("kind")?.as_str()? {
        "pin" => {
            let start = line(d, "line");
            Some(AgentEvent::Pin(Pin {
                id: tool_call_id.to_string(),
                path: s(d, "path").into(),
                anchor: Anchor::new(start, s(d, "text")),
                end_line: line(d, "end_line").max(start),
                note: s(d, "note"),
                seen: false,
                dismissed: false,
            }))
        }
        "explain" => Some(AgentEvent::ExplainChange {
            path: s(d, "path").into(),
            summary: s(d, "summary"),
            hunks: d
                .get("hunks")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|h| HunkNote {
                            anchor: Anchor::new(line(h, "line"), s(h, "text")),
                            note: s(h, "note"),
                            seen: false,
                            dismissed: false,
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }),
        "progress" => Some(AgentEvent::Progress(
            d.get("steps")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|st| ProgressStep {
                            text: s(st, "text"),
                            done: st.get("done").and_then(Value::as_bool).unwrap_or(false),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        )),
        _ => None, // "refused": the model was told; nothing to show
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_is_unknown_not_a_panic() {
        assert!(matches!(
            &parse_line("not json")[..],
            [Inbound::Event(AgentEvent::Unknown(_))]
        ));
        assert!(matches!(
            &parse_line(r#"{"type":"brand_new_event"}"#)[..],
            [Inbound::Event(AgentEvent::Unknown(_))]
        ));
        assert!(matches!(
            &parse_line(r#"{"no_type":1}"#)[..],
            [Inbound::Event(AgentEvent::Unknown(_))]
        ));
    }

    #[test]
    fn aborted_assistant_message_reports_error() {
        let out = parse_line(
            r#"{"type":"message_end","message":{"role":"assistant","stopReason":"aborted"}}"#,
        );
        assert!(
            matches!(&out[..], [Inbound::Event(AgentEvent::AssistantEnd { error: Some(e) })] if e == "aborted")
        );
    }

    #[test]
    fn pide_queries_are_not_dialogs() {
        let out = parse_line(
            r#"{"type":"extension_ui_request","id":"q1","method":"input","title":"pide:query","placeholder":"{\"kind\":\"diagnostics\",\"paths\":[\"a.rs\"]}","timeout":5000}"#,
        );
        assert!(
            matches!(&out[..], [Inbound::Query { id, payload }] if id == "q1" && payload["kind"] == "diagnostics" && payload["paths"][0] == "a.rs")
        );
    }

    #[test]
    fn dialogs_are_flagged() {
        let out = parse_line(
            r#"{"type":"extension_ui_request","id":"u1","method":"select","title":"Allow rm?","options":["Allow","Block"],"timeout":10000}"#,
        );
        assert!(
            matches!(&out[..], [Inbound::Dialog(d)] if d.id == "u1" && d.kind == DialogKind::Select && d.options == ["Allow", "Block"] && d.timeout_ms == Some(10000))
        );
    }
}

/// A pi Model object (recorded: tests/fixtures/model_rpc_responses.jsonl).
pub fn model_info(v: &Value) -> Option<pide_core::ModelInfo> {
    Some(pide_core::ModelInfo {
        provider: v.get("provider")?.as_str()?.to_string(),
        id: v.get("id")?.as_str()?.to_string(),
        name: v
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        reasoning: v.get("reasoning").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// `get_commands`' data (recorded: tests/fixtures/get_commands_response.json).
pub fn slash_commands(data: &Value) -> Vec<pide_core::SlashCommand> {
    data.get("commands")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|c| {
                    Some(pide_core::SlashCommand {
                        name: c.get("name")?.as_str()?.to_string(),
                        description: c
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        source: c
                            .get("source")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `get_session_stats`' data (recorded: tests/fixtures/session_stats_response.json).
pub fn session_stats(data: &Value) -> pide_core::SessionStats {
    let t = &data["tokens"];
    let u = |v: &Value| v.as_u64().or_else(|| v.as_f64().map(|f| f as u64));
    let c = &data["contextUsage"];
    pide_core::SessionStats {
        cost: data["cost"].as_f64().unwrap_or(0.0),
        tokens_total: u(&t["total"]).unwrap_or(0),
        tokens_input: u(&t["input"]).unwrap_or(0),
        tokens_output: u(&t["output"]).unwrap_or(0),
        tokens_cache_read: u(&t["cacheRead"]).unwrap_or(0),
        context_percent: c["percent"].as_f64(),
        context_tokens: u(&c["tokens"]),
        context_window: u(&c["contextWindow"]),
    }
}

/// `get_available_thinking_levels`' data (recorded: tests/fixtures/thinking_rpc_responses.jsonl).
pub fn thinking_levels(data: &Value) -> Vec<String> {
    data.get("levels")
        .and_then(Value::as_array)
        .map(|l| {
            l.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}
