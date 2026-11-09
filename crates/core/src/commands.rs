//! The command registry. Every action is a named command; every keybinding
//! points at one; the palette lists them all.

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::*;

/// The one action every keybinding dispatches. The app's root view handles it
/// by looking the id up in [`CommandRegistry`].
#[derive(Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = pide, no_json)]
pub struct RunCommand {
    pub id: SharedString,
}

impl RunCommand {
    pub fn new(id: impl Into<SharedString>) -> Self {
        Self { id: id.into() }
    }
}

#[derive(Clone)]
pub struct CommandSpec {
    /// Stable id, `area.verb` (`editor.save`, `agent.abort`).
    pub id: &'static str,
    pub title: SharedString,
    /// Default keystrokes in GPUI syntax (`cmd-s`, `cmd-shift-a`). JetBrains-style on macOS.
    pub default_key: Option<&'static str>,
    /// Where its key works, as a GPUI context predicate (`None`: everywhere).
    /// Editor-only commands use [`EDITOR`] so their keys don't shadow the same
    /// keys elsewhere (Alt+Enter in the pi composer queues a follow-up).
    pub key_context: Option<&'static str>,
}

/// GPUI Kit's code editor (`Input mode=editor`).
pub const EDITOR: &str = "Input && mode == editor";

impl CommandSpec {
    pub fn new(id: &'static str, title: impl Into<SharedString>) -> Self {
        Self { id, title: title.into(), default_key: None, key_context: None }
    }

    pub fn key(mut self, keystrokes: &'static str) -> Self {
        self.default_key = Some(keystrokes);
        self
    }

    /// Only bind the key inside this context (see [`EDITOR`]).
    pub fn in_context(mut self, predicate: &'static str) -> Self {
        self.key_context = Some(predicate);
        self
    }
}

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(Default)]
pub struct CommandRegistry {
    specs: Vec<CommandSpec>,
    handlers: HashMap<&'static str, Handler>,
}

impl Global for CommandRegistry {}

impl CommandRegistry {
    fn get(cx: &mut App) -> &mut Self {
        if !cx.has_global::<Self>() {
            cx.set_global(Self::default());
        }
        cx.global_mut::<Self>()
    }

    /// Register (or replace) a command. Handlers run on the UI thread with the
    /// active window; they must not block.
    pub fn register(
        cx: &mut App,
        spec: CommandSpec,
        handler: impl Fn(&mut Window, &mut App) + 'static,
    ) {
        let this = Self::get(cx);
        if let Some(existing) = this.specs.iter_mut().find(|s| s.id == spec.id) {
            log::warn!("command {} registered twice; replacing", spec.id);
            *existing = spec.clone();
        } else {
            this.specs.push(spec.clone());
        }
        this.handlers.insert(spec.id, Rc::new(handler));
    }

    /// Run a command by id. Returns false (and logs) if it doesn't exist.
    pub fn run(id: &str, window: &mut Window, cx: &mut App) -> bool {
        let handler = cx
            .try_global::<Self>()
            .and_then(|r| r.handlers.get(id).cloned());
        match handler {
            Some(h) => {
                log::debug!("command {id}");
                h(window, cx);
                true
            }
            None => {
                log::error!("unknown command: {id}");
                false
            }
        }
    }

    pub fn contains(cx: &App, id: &str) -> bool {
        cx.try_global::<Self>().is_some_and(|r| r.handlers.contains_key(id))
    }

    /// All commands, in registration order.
    pub fn all(cx: &App) -> Vec<CommandSpec> {
        cx.try_global::<Self>().map(|r| r.specs.clone()).unwrap_or_default()
    }
}
