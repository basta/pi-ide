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

/// The registry's default keys (`key -> command id`). Cheap, UI thread.
pub fn defaults(cx: &App) -> BTreeMap<String, Option<String>> {
    CommandRegistry::all(cx)
        .into_iter()
        .filter_map(|s| {
            s.default_key
                .map(|k| (k.to_string(), Some(s.id.to_string())))
        })
        .collect()
}

/// The keystrokes (GPUI syntax) bound to `id` now: the user keymap over
/// `defaults`. Reads `keymap.json`: call off the UI thread.
pub fn key_of(mut defaults: BTreeMap<String, Option<String>>, id: &str) -> Option<String> {
    if let Ok(text) = std::fs::read_to_string(paths::keymap_file())
        && let Ok(user) = serde_json::from_str::<BTreeMap<String, Option<String>>>(&text)
    {
        defaults.extend(user);
    }
    defaults
        .into_iter()
        .find(|(_, c)| c.as_deref() == Some(id))
        .map(|(k, _)| k)
}

/// Change `id`'s shortcut in a user keymap: unbind `old` (with `null`, which also
/// removes a default) and bind `new`. `new: None` only removes.
pub fn rebind(
    user: &mut BTreeMap<String, Option<String>>,
    id: &str,
    old: Option<&str>,
    new: Option<&str>,
) {
    if let Some(old) = old
        && Some(old) != new
    {
        user.insert(old.to_string(), None);
    }
    if let Some(new) = new {
        user.insert(new.to_string(), Some(id.to_string()));
    }
}

/// Whether `keys` is valid GPUI keystroke syntax (`cmd-shift-k`, or a chord `cmd-k cmd-s`).
pub fn valid_keys(keys: &str) -> bool {
    let os = if cfg!(target_os = "macos") {
        OperatingSystem::Mac
    } else if cfg!(target_os = "windows") {
        OperatingSystem::Windows
    } else {
        OperatingSystem::Linux
    };
    !keys.trim().is_empty()
        && keys
            .split_whitespace()
            .all(|k| Keystroke::parse_with_os(k, os).is_ok())
}

/// Write the rebinding to `keymap.json` (keeping other entries). Blocking: call
/// off the UI thread. The file watcher then reloads the keymap.
pub fn save_rebind(id: String, old: Option<String>, new: Option<String>) -> std::io::Result<()> {
    let path = paths::keymap_file();
    let mut user: BTreeMap<String, Option<String>> = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).map_err(std::io::Error::other)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(e) => return Err(e),
    };
    rebind(&mut user, &id, old.as_deref(), new.as_deref());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&user).map_err(std::io::Error::other)? + "\n",
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn rebinding_unbinds_the_old_key() {
        let mut user = BTreeMap::new();
        rebind(&mut user, "file.goto", Some("cmd-shift-o"), Some("cmd-p"));
        assert_eq!(user.get("cmd-shift-o"), Some(&None));
        assert_eq!(user.get("cmd-p"), Some(&Some("file.goto".to_string())));
        rebind(&mut user, "file.goto", Some("cmd-p"), None);
        assert_eq!(user.get("cmd-p"), Some(&None));
        assert!(valid_keys("cmd-shift-k"));
        assert!(valid_keys("cmd-k cmd-s"));
        assert!(!valid_keys(""));
    }
}
