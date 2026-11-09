//! Keybindings: command defaults + `~/.config/pide/keymap.json`, hot-reloaded.
//!
//! The file is a JSON object mapping keystrokes (GPUI syntax) to a command id,
//! or to `null` to remove a default:
//!
//! ```json
//! { "cmd-shift-a": "palette.open", "cmd-alt-l": null }
//! ```
//!
//! Every entry must name a registered command; anything else is logged as an error.

use std::collections::BTreeMap;

use gpui_kit::*;

use crate::commands::{CommandRegistry, RunCommand};
use crate::paths;

/// GPUI Kit's own bindings, snapshotted once so a reload can clear and re-add them.
struct BaseBindings(Vec<KeyBinding>);
impl Global for BaseBindings {}

/// Call once, after `gpui_kit::init` and every crate's `init` (so all base
/// bindings and commands exist). Applies the keymap and returns its errors.
pub fn init(cx: &mut App) -> Vec<String> {
    let base: Vec<KeyBinding> = cx.key_bindings().borrow().bindings().cloned().collect();
    cx.set_global(BaseBindings(base));
    apply(cx)
}

/// Re-read the keymap file and rebuild all bindings. Returns errors (also logged).
pub fn apply(cx: &mut App) -> Vec<String> {
    let mut errors = Vec::new();

    // Defaults from the registry.
    let mut map: BTreeMap<String, Option<String>> = CommandRegistry::all(cx)
        .into_iter()
        .filter_map(|s| {
            s.default_key
                .map(|k| (k.to_string(), Some(s.id.to_string())))
        })
        .collect();

    // User overrides.
    let path = paths::keymap_file();
    match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<BTreeMap<String, Option<String>>>(&text) {
            Ok(user) => {
                for (key, cmd) in user {
                    if let Some(cmd) = &cmd
                        && !CommandRegistry::contains(cx, cmd)
                    {
                        errors.push(format!("keymap: \"{key}\" -> unknown command \"{cmd}\""));
                        continue;
                    }
                    map.insert(key, cmd);
                }
            }
            Err(e) => errors.push(format!("keymap: {} is not valid JSON: {e}", path.display())),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => errors.push(format!("keymap: can't read {}: {e}", path.display())),
    }

    let contexts: BTreeMap<&'static str, &'static str> = CommandRegistry::all(cx)
        .into_iter()
        .filter_map(|s| s.key_context.map(|c| (s.id, c)))
        .collect();
    let mut ours = Vec::new();
    for (key, cmd) in &map {
        let Some(cmd) = cmd else { continue };
        let predicate = contexts
            .get(cmd.as_str())
            .and_then(|c| gpui_kit::KeyBindingContextPredicate::parse(c).ok())
            .map(std::rc::Rc::new);
        match KeyBinding::load(
            key,
            Box::new(RunCommand::new(cmd.clone())),
            predicate,
            false,
            None,
            cx,
        ) {
            Ok(b) => ours.push(b),
            Err(e) => errors.push(format!("keymap: bad keystroke \"{key}\": {e:?}")),
        }
    }

    let base = cx
        .try_global::<BaseBindings>()
        .map(|b| b.0.clone())
        .unwrap_or_default();
    cx.clear_key_bindings();
    cx.bind_keys(base);
    cx.bind_keys(ours);

    for e in &errors {
        log::error!("{e}");
    }
    log::info!(
        "keymap applied ({} bindings of ours)",
        map.values().flatten().count()
    );
    errors
}

/// The keystrokes currently bound to a command (for palette hints).
pub fn keys_for(cx: &App, id: &str) -> Option<String> {
    let action = RunCommand::new(id.to_string());
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    let binding = keymap.bindings_for_action(&action).last()?;
    Some(
        binding
            .keystrokes()
            .iter()
            .map(|k| k.display(cx.platform_info().os).to_string())
            .collect::<Vec<_>>()
            .join(" "),
    )
}
