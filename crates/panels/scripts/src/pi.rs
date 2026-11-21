//! pi's side of the `pide` module: the project window's sessions, what pi
//! changed and why, its notes and replies, its checklist, and the chat.
//!
//! A session's id is its entity id as a string; it stays the same while the
//! session is open. Paths are project-relative like the rest of the module.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use gpui_kit::*;
use gpui_shell::{HostError, HostModule, HostObject, HostValue};
use pide_core::collab::{Anchor, Reply};
use pide_core::{Agent, AgentEvent, AgentStatus, Services, SessionsEvent};
use serde_json::Value;

use crate::host::{later, num, rel, resolve, wake_with, with_project};

pub const DECLARATIONS: &str = r#"
/** pi's open sessions in this window, in tab order. */
export function piSessions(): PiSession[];
export interface PiSession {
  id: string, name: string, active: boolean,
  status: "idle" | "starting" | "busy" | "aborting" | "crashed",
  /** Finished while you looked elsewhere; waiting for an answer to a question. */
  unread: boolean, waiting: boolean,
  model: string | null, cost: number | null, contextPercent: number | null,
  /** pi's session file (absolute), once it has one. */
  file: string | null,
  changes: number, notes: number,
}
/** Files pi changed (one session, or all of them), with its reasons and notes. */
export function piChanges(session?: string): PiChange[];
export interface PiChange {
  session: string, path: string, kind: "added" | "modified" | "deleted",
  added: number | null, removed: number | null,
  /** pi's one-line reason (ide_explain_change), and where the change starts. */
  summary: string | null, firstLine: number | null,
  reviewed: boolean, reverted: boolean, tools: string,
  notes: PiNote[],
}
/** What pi pointed you at: pins and tour stops (ide_pin, ide_tour) and the notes on its changes. */
export function piNotes(session?: string): PiNote[];
export interface PiNote {
  session: string, id: string, kind: "pin" | "tour" | "change",
  path: string, line: number, endLine: number,
  /** The line's text when the note was made; outdated = that text is gone. */
  lineText: string, outdated: boolean,
  note: string, seen: boolean, dismissed: boolean,
  /** Your replies and pi's answers, oldest first. */
  thread: { pi: boolean, text: string }[],
}
/** pi's checklist for a session (ide_progress). */
export function piProgress(session: string): { text: string, done: boolean }[];
/** A changed file as it was before pi first touched it in that session (null: pi created it). */
export function piBefore(session: string, path: string): string | null;
/** The conversation (its current branch): an open session's id, or a saved session's file. */
export function piMessages(session: string): Promise<PiMessage[]>;
/** pi's saved sessions for this project, newest first; pass `file` to piMessages. */
export function piSavedSessions(): Promise<{ file: string, name: string | null, preview: string, modified: number }[]>;
export interface PiMessage {
  role: "user" | "assistant" | "tool",
  text: string,
  /** assistant: its reasoning, when the model shows it. */
  thinking?: string,
  /** assistant: the tool calls it made. tool: the call this answers. */
  tools?: { id: string, name: string, args: any }[],
  toolId?: string, toolName?: string, error?: boolean,
  time: string | null,
}
/** Show a session in the pi panel. */
export function piOpen(session: string): boolean;
"#;

/// Panels whose panel.json asks for `"pi": true` (by folder).
static ALLOWED: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Default::default);

/// Called as a panel loads with whether its manifest asks for pi.
pub fn remember(dir: &Path, pi: bool) {
    let mut set = ALLOWED.lock().unwrap();
    if pi {
        set.insert(dir.to_path_buf());
    } else {
        set.remove(dir);
    }
}

/// Refuse unless the calling panel asked for (and was granted) `pi`.
fn allowed() -> Result<(), HostError> {
    match crate::image::running_panel() {
        Some((dir, _)) if ALLOWED.lock().unwrap().contains(&dir) => Ok(()),
        _ => Err(HostError::new(
            "reading pi's sessions needs \"permissions\": { \"pi\": true } in panel.json",
        )),
    }
}

fn status(s: &AgentStatus) -> &'static str {
    match s {
        AgentStatus::Idle => "idle",
        AgentStatus::Starting => "starting",
        AgentStatus::Busy => "busy",
        AgentStatus::Aborting => "aborting",
        _ => "crashed",
    }
}

pub(crate) fn sid(agent: &Entity<Agent>) -> String {
    agent.entity_id().as_u64().to_string()
}

fn find(s: &Services, id: &str, cx: &App) -> Result<Entity<Agent>, HostError> {
    s.sessions
        .read(cx)
        .all()
        .iter()
        .find(|a| sid(a) == id)
        .cloned()
        .ok_or_else(|| HostError::new(format!("no open pi session {id}")))
}

/// The sessions a call is about: one, or all.
fn chosen(s: &Services, id: Option<String>, cx: &App) -> Result<Vec<Entity<Agent>>, HostError> {
    match id {
        Some(id) => Ok(vec![find(s, &id, cx)?]),
        None => Ok(s.sessions.read(cx).all().to_vec()),
    }
}

fn thread(t: &[Reply]) -> HostValue {
    HostValue::Array(
        t.iter()
            .map(|r| {
                HostObject::new()
                    .field("pi", r.pi)
                    .field("text", r.text.clone())
                    .into()
            })
            .collect(),
    )
}

#[allow(clippy::too_many_arguments)]
fn note(
    session: &str,
    id: String,
    kind: &str,
    path: String,
    a: &Anchor,
    end_line: usize,
    text: &str,
    seen: bool,
    dismissed: bool,
    t: &[Reply],
) -> HostValue {
    HostObject::new()
        .field("session", session)
        .field("id", id)
        .field("kind", kind)
        .field("path", path)
        .field("line", num(a.line as f64))
        .field("endLine", num(end_line.max(a.line) as f64))
        .field("lineText", a.text.clone())
        .field("outdated", a.outdated)
        .field("note", text)
        .field("seen", seen)
        .field("dismissed", dismissed)
        .field("thread", thread(t))
        .into()
}

fn change_notes(session: &str, root: &Path, c: &pide_core::changes::FileChange) -> Vec<HostValue> {
    let path = rel(root, &c.path);
    c.notes
        .iter()
        .enumerate()
        .map(|(i, n)| {
            note(
                session,
                format!("change:{path}:{i}"),
                "change",
                path.clone(),
                &n.anchor,
                n.anchor.line,
                &n.note,
                n.seen,
                n.dismissed,
                &n.thread,
            )
        })
        .collect()
}

fn opt_num(n: Option<impl Into<f64>>) -> HostValue {
    n.map(|n| num(n.into())).unwrap_or(HostValue::Null)
}

pub fn register(m: HostModule) -> HostModule {
    m.function("piSessions", |_| {
        allowed()?;
        with_project(|s, _, cx| {
            let sessions = s.sessions.read(cx);
            let active = sid(&sessions.active());
            Ok(HostValue::Array(
                sessions
                    .all()
                    .iter()
                    .map(|a| {
                        let id = sid(a);
                        let ag = a.read(cx);
                        let notes =
                            ag.pins.len() + ag.changes.iter().map(|c| c.notes.len()).sum::<usize>();
                        HostObject::new()
                            .field("id", id.clone())
                            .field("name", ag.name.to_string())
                            .field("active", id == active)
                            .field("status", status(ag.status()))
                            .field("unread", ag.unread)
                            .field("waiting", ag.waiting)
                            .field("model", ag.model.as_ref().map(|m| m.name.clone()))
                            .field("cost", opt_num(ag.stats.as_ref().map(|s| s.cost)))
                            .field(
                                "contextPercent",
                                opt_num(ag.stats.as_ref().and_then(|s| s.context_percent)),
                            )
                            .field(
                                "file",
                                ag.session_file.as_ref().map(|f| f.display().to_string()),
                            )
                            .field("changes", num(ag.changes.len() as f64))
                            .field("notes", num(notes as f64))
                            .into()
                    })
                    .collect(),
            ))
        })
    })
    .function("piChanges", |args| {
        allowed()?;
        let id = args.get(0).and_then(|v| v.as_str().map(str::to_string));
        with_project(move |s, _, cx| {
            let root = s.workspace.read(cx).root().to_path_buf();
            let mut out = vec![];
            for a in chosen(s, id, cx)? {
                let session = sid(&a);
                for c in &a.read(cx).changes {
                    let kind = match c.badge() {
                        "A" => "added",
                        "D" => "deleted",
                        _ => "modified",
                    };
                    out.push(
                        HostObject::new()
                            .field("session", session.clone())
                            .field("path", rel(&root, &c.path))
                            .field("kind", kind)
                            .field("added", opt_num(c.stats.map(|s| s.0 as f64)))
                            .field("removed", opt_num(c.stats.map(|s| s.1 as f64)))
                            .field("summary", c.summary.clone())
                            .field("firstLine", opt_num(c.first_line.map(|l| l as f64)))
                            .field("reviewed", c.reviewed)
                            .field("reverted", c.reverted)
                            .field("tools", c.tools_label().to_string())
                            .field("notes", HostValue::Array(change_notes(&session, &root, c)))
                            .into(),
                    );
                }
            }
            Ok(HostValue::Array(out))
        })
    })
    .function("piNotes", |args| {
        allowed()?;
        let id = args.get(0).and_then(|v| v.as_str().map(str::to_string));
        with_project(move |s, _, cx| {
            let root = s.workspace.read(cx).root().to_path_buf();
            let mut out = vec![];
            for a in chosen(s, id, cx)? {
                let session = sid(&a);
                let ag = a.read(cx);
                for p in &ag.pins {
                    let kind = if p.id.starts_with(pide_core::collab::TOUR_PREFIX) {
                        "tour"
                    } else {
                        "pin"
                    };
                    out.push(note(
                        &session,
                        p.id.clone(),
                        kind,
                        rel(&root, &p.path),
                        &p.anchor,
                        p.end_line,
                        &p.note,
                        p.seen,
                        p.dismissed,
                        &p.thread,
                    ));
                }
                for c in &ag.changes {
                    out.extend(change_notes(&session, &root, c));
                }
            }
            Ok(HostValue::Array(out))
        })
    })
    .function("piProgress", |args| {
        allowed()?;
        let id = args.string(0)?.to_string();
        with_project(move |s, _, cx| {
            let a = find(s, &id, cx)?;
            Ok(HostValue::Array(
                a.read(cx)
                    .progress
                    .iter()
                    .map(|p| {
                        HostObject::new()
                            .field("text", p.text.clone())
                            .field("done", p.done)
                            .into()
                    })
                    .collect(),
            ))
        })
    })
    .function("piBefore", |args| {
        allowed()?;
        let id = args.string(0)?.to_string();
        let path = args.string(1)?.to_string();
        with_project(move |s, _, cx| {
            let abs = resolve(s.workspace.read(cx).root(), &path)?;
            let a = find(s, &id, cx)?;
            let ag = a.read(cx);
            let c = ag.changes.iter().find(|c| c.path == abs).ok_or_else(|| {
                HostError::new(format!("pi didn't change {path} in that session"))
            })?;
            Ok(c.before.clone().into())
        })
    })
    .async_function("piMessages", |args| {
        allowed()?;
        let id = args.string(0)?.to_string();
        let file = with_project(move |s, _, cx| {
            if let Ok(a) = find(s, &id, cx) {
                return Ok(a.read(cx).session_file.clone());
            }
            // A saved session: only files in this project's pi session folder.
            let dir = pide_core::sessions::pi_session_dir(s.workspace.read(cx).root());
            let file = Path::new(&id);
            let inside = file.extension().is_some_and(|x| x == "jsonl")
                && file
                    .canonicalize()
                    .ok()
                    .zip(dir.canonicalize().ok())
                    .is_some_and(|(f, d)| f.starts_with(d));
            if !inside {
                return Err(HostError::new(format!(
                    "{id}: not an open session or a saved one of this project"
                )));
            }
            Ok(Some(file.to_path_buf()))
        })?;
        Ok(async move {
            let Some(file) = file else {
                return Ok(HostValue::Array(vec![]));
            };
            let text = std::fs::read_to_string(&file)
                .map_err(|e| HostError::new(format!("{}: {e}", file.display())))?;
            Ok(messages(&text))
        })
    })
    .async_function("piSavedSessions", |_| {
        allowed()?;
        let root = with_project(|s, _, cx| Ok(s.workspace.read(cx).root().to_path_buf()))?;
        Ok(async move {
            let saved = pide_core::sessions::list_saved(&root);
            Ok(HostValue::Array(
                saved
                    .into_iter()
                    .map(|s| {
                        let ms = s
                            .modified
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as f64)
                            .unwrap_or(0.);
                        HostObject::new()
                            .field("file", s.path.display().to_string())
                            .field("name", s.name)
                            .field("preview", s.preview)
                            .field("modified", num(ms))
                            .into()
                    })
                    .collect(),
            ))
        })
    })
    .function("piOpen", |args| {
        allowed()?;
        let id = args.string(0)?.to_string();
        with_project(move |s, win, cx| {
            let Ok(agent) = find(s, &id, cx) else {
                return Ok(false.into());
            };
            let sessions = s.sessions.clone();
            later(win, cx, move |window, cx| {
                sessions.update(cx, |ss, cx| {
                    if let Some(i) = ss.all().iter().position(|a| *a == agent) {
                        ss.set_active(i, cx);
                    }
                });
                pide_core::CommandRegistry::run("view.agent", window, cx);
            });
            Ok(true.into())
        })
    })
}

/// A pi session file (JSON lines) as the messages of its current branch.
pub fn messages(text: &str) -> HostValue {
    let entries: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let mut names: HashMap<String, String> = HashMap::new();
    let mut out = vec![];
    for v in pide_core::sessions::current_branch(entries) {
        if v.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let m = &v["message"];
        let time = v
            .get("timestamp")
            .and_then(Value::as_str)
            .map(str::to_string);
        let blocks = m.get("content");
        let text_of = |b: Option<&Value>| match b {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(items)) => items
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        match m.get("role").and_then(Value::as_str) {
            Some("user") => out.push(
                HostObject::new()
                    .field("role", "user")
                    .field("text", text_of(blocks))
                    .field("time", time)
                    .into(),
            ),
            Some("assistant") => {
                let tools: Vec<HostValue> = blocks
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|b| b.get("type").and_then(Value::as_str) == Some("toolCall"))
                    .map(|b| {
                        let id = b["id"].as_str().unwrap_or_default().to_string();
                        let name = b["name"].as_str().unwrap_or_default().to_string();
                        names.insert(id.clone(), name.clone());
                        HostObject::new()
                            .field("id", id)
                            .field("name", name)
                            .field("args", json(&b["arguments"]))
                            .into()
                    })
                    .collect();
                let thinking = blocks
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|b| b.get("type").and_then(Value::as_str) == Some("thinking"))
                    .filter_map(|b| b.get("thinking").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n");
                out.push(
                    HostObject::new()
                        .field("role", "assistant")
                        .field("text", text_of(blocks))
                        .field("thinking", (!thinking.is_empty()).then_some(thinking))
                        .field("tools", HostValue::Array(tools))
                        .field("time", time)
                        .into(),
                );
            }
            Some("toolResult") => {
                let id = m["toolCallId"].as_str().unwrap_or_default().to_string();
                let mut text = text_of(blocks);
                if text.len() > 4000 {
                    let cut = (0..=4000)
                        .rev()
                        .find(|i| text.is_char_boundary(*i))
                        .unwrap_or(0);
                    text = format!("{}…", &text[..cut]);
                }
                out.push(
                    HostObject::new()
                        .field("role", "tool")
                        .field("text", text)
                        .field("toolName", names.get(&id).cloned())
                        .field("toolId", id)
                        .field("error", m["isError"].as_bool().unwrap_or(false))
                        .field("time", time)
                        .into(),
                );
            }
            _ => {}
        }
    }
    HostValue::Array(out)
}

fn json(v: &Value) -> HostValue {
    match v {
        Value::Null => HostValue::Null,
        Value::Bool(b) => (*b).into(),
        Value::Number(n) => num(n.as_f64().unwrap_or(0.)),
        Value::String(s) => s.clone().into(),
        Value::Array(a) => HostValue::Array(a.iter().map(json).collect()),
        Value::Object(o) => {
            let mut obj = HostObject::new();
            for (k, v) in o {
                obj = obj.field(k.as_str(), json(v));
            }
            obj.into()
        }
    }
}

/// Wake scripts with `{ kind: "pi", what, session, path }` as pi works.
pub(crate) fn watch(id: WindowId, services: &Services, cx: &mut App) -> Vec<Subscription> {
    let root = services.workspace.read(cx).root().to_path_buf();
    let subs = std::rc::Rc::new(std::cell::RefCell::new(
        HashMap::<EntityId, Subscription>::new(),
    ));
    let follow = {
        let subs = subs.clone();
        move |sessions: &Entity<pide_core::Sessions>, cx: &mut App| {
            let all = sessions.read(cx).all().to_vec();
            let mut subs = subs.borrow_mut();
            subs.retain(|e, _| all.iter().any(|a| a.entity_id() == *e));
            for a in all {
                if subs.contains_key(&a.entity_id()) {
                    continue;
                }
                let root = root.clone();
                let session = sid(&a);
                let sub = cx.subscribe(&a, move |_, ev: &AgentEvent, _| {
                    let (what, path) = match ev {
                        AgentEvent::StatusChanged => ("status", None),
                        AgentEvent::UserPrompt { .. } => ("message", None),
                        AgentEvent::AssistantEnd { .. } => ("message", None),
                        AgentEvent::ToolEnd { .. } => ("tool", None),
                        AgentEvent::FileTouched { path, .. } => ("change", Some(rel(&root, path))),
                        AgentEvent::Pin(p) => ("note", Some(rel(&root, &p.path))),
                        AgentEvent::ExplainChange { path, .. } => ("note", Some(rel(&root, path))),
                        AgentEvent::Progress(_) => ("progress", None),
                        AgentEvent::Settled { .. } => ("settled", None),
                        _ => return,
                    };
                    wake_with(
                        id,
                        HostObject::new()
                            .field("kind", "pi")
                            .field("what", what)
                            .field("session", session.clone())
                            .field("path", path)
                            .into(),
                    );
                });
                subs.insert(a.entity_id(), sub);
            }
        }
    };
    follow(&services.sessions, cx);
    let s = cx.subscribe(
        &services.sessions,
        move |sessions, _: &SessionsEvent, cx| {
            follow(&sessions, cx);
            wake_with(
                id,
                HostObject::new()
                    .field("kind", "pi")
                    .field("what", "sessions")
                    .field("session", HostValue::Null)
                    .field("path", HostValue::Null)
                    .into(),
            );
        },
    );
    // The per-session subscriptions live in `follow`, held by this one.
    vec![s]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn reads_a_session_file_into_messages() {
        let file = [
            r#"{"type":"session","id":"s"}"#,
            r#"{"type":"message","id":"1","timestamp":"t1","message":{"role":"user","content":[{"type":"text","text":"Fix the store"}]}}"#,
            r#"{"type":"message","id":"2","parentId":"1","message":{"role":"assistant","content":[{"type":"thinking","thinking":"Start with the store."},{"type":"text","text":"Reading it."},{"type":"toolCall","id":"c1","name":"read","arguments":{"path":"src/store.py"}}]}}"#,
            r#"{"type":"message","id":"3","parentId":"2","message":{"role":"toolResult","toolCallId":"c1","isError":false,"content":[{"type":"text","text":"class Store: ..."}]}}"#,
        ]
        .join("\n");
        let HostValue::Array(msgs) = messages(&file) else {
            panic!()
        };
        assert_eq!(msgs.len(), 3);
        let dbg = format!("{msgs:?}");
        assert!(dbg.contains("Fix the store"), "{dbg}");
        assert!(dbg.contains("src/store.py"), "{dbg}");
        assert!(dbg.contains("\"read\""), "{dbg}");
        assert!(dbg.contains("Start with the store."), "{dbg}");
    }
}
