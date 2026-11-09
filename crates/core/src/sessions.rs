//! Several pi sessions at once (spec M2). Each open session is an
//! `Entity<Agent>` with its own backend (its own pi process while it runs).
//! The app installs a factory, so `core` never depends on `agent-pi`.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::SystemTime;

use gpui_kit::*;

use crate::{Agent, AgentBackend, AgentEvent};

/// Builds a backend for a session; `resume` = an existing pi session file.
pub type BackendFactory = Rc<dyn Fn(Option<PathBuf>) -> Box<dyn AgentBackend>>;

#[derive(Clone, Debug)]
pub enum SessionsEvent {
    /// Sessions were added/removed, or the active one changed.
    Changed,
}

pub struct Sessions {
    list: Vec<Entity<Agent>>,
    active: usize,
    factory: BackendFactory,
    _subs: Vec<Subscription>,
}

impl EventEmitter<SessionsEvent> for Sessions {}

impl Sessions {
    /// Starts with one empty session (no process until its first prompt).
    pub fn new(factory: BackendFactory, cx: &mut Context<Self>) -> Self {
        let mut this = Self { list: Vec::new(), active: 0, factory, _subs: Vec::new() };
        this.add(None, None, cx);
        this
    }

    pub fn all(&self) -> &[Entity<Agent>] {
        &self.list
    }

    pub fn active(&self) -> Entity<Agent> {
        self.list[self.active.min(self.list.len() - 1)].clone()
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    /// Open a session: a fresh one, or `resume` a saved pi session file.
    pub fn add(&mut self, resume: Option<PathBuf>, name: Option<String>, cx: &mut Context<Self>) -> Entity<Agent> {
        if let Some(path) = &resume {
            if let Some(i) = self.list.iter().position(|a| a.read(cx).session_file.as_ref() == Some(path)) {
                self.set_active(i, cx);
                return self.list[i].clone();
            }
        }
        let backend = (self.factory)(resume.clone());
        let agent = cx.new(|_| {
            let mut a = Agent::new(backend);
            a.session_file = resume;
            if let Some(n) = name {
                a.name = n.into();
            }
            a
        });
        // A session that finishes while you look elsewhere is marked unread.
        self._subs.push(cx.subscribe(&agent, |this, agent, ev: &AgentEvent, cx| {
            if matches!(ev, AgentEvent::Settled { .. }) && this.active() != agent {
                agent.update(cx, |a, cx| {
                    a.unread = true;
                    cx.notify();
                });
            }
        }));
        self._subs.push(cx.observe(&agent, |_, _, cx| cx.notify()));
        self.list.push(agent.clone());
        self.active = self.list.len() - 1;
        cx.emit(SessionsEvent::Changed);
        cx.notify();
        agent
    }

    pub fn set_active(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.list.len() {
            return;
        }
        self.active = index;
        self.list[index].update(cx, |a, cx| {
            a.unread = false;
            cx.notify();
        });
        cx.emit(SessionsEvent::Changed);
        cx.notify();
    }

    pub fn activate(&mut self, agent: &Entity<Agent>, cx: &mut Context<Self>) {
        if let Some(i) = self.list.iter().position(|a| a == agent) {
            self.set_active(i, cx);
        }
    }

    /// Close a session: stops its process; the pi session file stays on disk.
    /// The last session is replaced by a fresh one rather than leaving none.
    pub fn close(&mut self, agent: &Entity<Agent>, cx: &mut Context<Self>) {
        let Some(i) = self.list.iter().position(|a| a == agent) else { return };
        agent.update(cx, |a, cx| a.restart(cx));
        self.list.remove(i);
        if self.list.is_empty() {
            self.add(None, None, cx);
            return;
        }
        if self.active >= i && self.active > 0 {
            self.active -= 1;
        }
        self.set_active(self.active, cx);
    }

    pub fn cycle(&mut self, delta: isize, cx: &mut Context<Self>) {
        let n = self.list.len() as isize;
        let next = ((self.active as isize + delta) % n + n) % n;
        self.set_active(next as usize, cx);
    }

    /// Reopen sessions saved in a layout. Replaces the untouched startup session.
    pub fn restore(&mut self, files: Vec<(PathBuf, Option<String>)>, active: usize, cx: &mut Context<Self>) {
        if files.is_empty() {
            return;
        }
        let untouched = self.list.len() == 1 && {
            let a = self.list[0].read(cx);
            a.session_file.is_none() && a.name.as_ref() == crate::agent::UNNAMED && !a.is_busy()
        };
        let first_new = self.list.len();
        for (file, name) in files {
            if file.exists() {
                self.add(Some(file), name, cx);
            }
        }
        if untouched && self.list.len() > first_new {
            self.list.remove(0);
        }
        let n = self.list.len();
        self.set_active(active.min(n - 1), cx);
    }

    /// Session files of open sessions (for saving the layout).
    pub fn open_files(&self, cx: &App) -> Vec<PathBuf> {
        self.list.iter().filter_map(|a| a.read(cx).session_file.clone()).collect()
    }
}

/// A pi session saved on disk for this project.
#[derive(Clone, Debug)]
pub struct SavedSession {
    pub path: PathBuf,
    pub name: Option<String>,
    pub preview: String,
    pub modified: SystemTime,
}

/// `~/.pi/agent/sessions/--<cwd without leading /, separators as ->--/`
/// (format: pi `docs/session-format.md`).
pub fn pi_session_dir(cwd: &Path) -> PathBuf {
    let s = cwd.to_string_lossy();
    let enc: String = s.trim_start_matches(['/', '\\']).chars().map(|c| if matches!(c, '/' | '\\' | ':') { '-' } else { c }).collect();
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join(".pi/agent/sessions").join(format!("--{enc}--"))
}

/// Saved sessions for `cwd`, newest first. Blocking: call on a background thread.
pub fn list_saved(cwd: &Path) -> Vec<SavedSession> {
    let Ok(dir) = std::fs::read_dir(pi_session_dir(cwd)) else { return Vec::new() };
    let mut out: Vec<SavedSession> = dir
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| {
            let path = e.path();
            let modified = e.metadata().and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
            let (name, preview) = summarize(&path)?;
            Some(SavedSession { path, name, preview, modified })
        })
        .collect();
    out.sort_by(|a, b| b.modified.cmp(&a.modified));
    out
}

/// (session_info name, first user message). `None` if the file has no user message.
fn summarize(path: &Path) -> Option<(Option<String>, String)> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    let mut name = None;
    let mut preview = None;
    for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
        // Cheap pre-filter: most lines are large assistant/tool messages.
        if line.contains("\"session_info\"") {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                if let Some(n) = v.get("name").and_then(|n| n.as_str()) {
                    name = Some(n.to_string());
                }
            }
        } else if preview.is_none() && line.contains("\"role\":\"user\"") {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                preview = crate::sessions::user_text(&v["message"]);
            }
        }
    }
    Some((name, preview?))
}

/// Text of a user message whose `content` is a string or text blocks.
pub fn user_text(message: &serde_json::Value) -> Option<String> {
    match message.get("content")? {
        serde_json::Value::String(t) => Some(t.clone()),
        serde_json::Value::Array(blocks) => Some(
            blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn session_dir_matches_pi_encoding() {
        // Observed on disk: /Users/ja/Projects -> --Users-ja-Projects--
        let d = pi_session_dir(Path::new("/Users/ja/Projects"));
        assert!(d.ends_with(".pi/agent/sessions/--Users-ja-Projects--"));
    }

    #[::core::prelude::v1::test]
    fn names_from_prompts() {
        assert_eq!(crate::agent::name_from_prompt("fix the flaky replay test please now"), "fix the flaky replay test");
        assert_eq!(crate::agent::name_from_prompt("   "), crate::agent::UNNAMED);
    }
}
