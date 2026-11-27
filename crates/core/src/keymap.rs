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
        .filter_map(|s| s.platform_key().map(|k| (k, Some(s.id.to_string()))))
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

    let over_terminal: std::collections::HashSet<&'static str> = CommandRegistry::all(cx)
        .into_iter()
        .filter(|s| s.over_terminal)
        .map(|s| s.id)
        .collect();
    let to_shell = map
        .iter()
        .filter_map(|(key, cmd)| Some((key, cmd.as_deref()?)))
        .filter(|(key, cmd)| {
            !contexts.contains_key(cmd) && !over_terminal.contains(cmd) && goes_to_shell(key)
        })
        .filter_map(|(key, _)| {
            KeyBinding::load(
                key,
                Box::new(NoAction),
                Some(std::rc::Rc::new(
                    gpui_kit::KeyBindingContextPredicate::parse(crate::commands::TERMINAL).ok()?,
                )),
                false,
                None,
                cx,
            )
            .ok()
        })
        .collect::<Vec<_>>();

    let base = cx
        .try_global::<BaseBindings>()
        .map(|b| b.0.clone())
        .unwrap_or_default();
    cx.clear_key_bindings();
    cx.bind_keys(base);
    cx.bind_keys(ours);
    // After `ours`: a global binding matches as deep as the terminal's context,
    // and GPUI breaks that tie in favour of the later binding.
    cx.bind_keys(to_shell);

    for e in &errors {
        log::error!("{e}");
    }
    log::info!(
        "keymap applied ({} bindings of ours)",
        map.values().flatten().count()
    );
    errors
}

/// Whether a terminal sends `keys` to its shell rather than to pide. ⌘ never
/// reaches a shell on macOS; elsewhere every key does (Ctrl+R, Ctrl+E, F-keys).
fn goes_to_shell(keys: &str) -> bool {
    let first = keys.split(' ').next().unwrap_or(keys);
    let has_cmd = first.split('-').any(|p| p == "cmd" || p == "secondary");
    !(cfg!(target_os = "macos") && has_cmd)
}

/// The registry's default keys (`key -> command id`). Cheap, UI thread.
pub fn defaults(cx: &App) -> BTreeMap<String, Option<String>> {
    CommandRegistry::all(cx)
        .into_iter()
        .filter_map(|s| s.platform_key().map(|k| (k, Some(s.id.to_string()))))
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

/// `cmd-shift-o` → `ctrl-shift-o` (each keystroke of a chord). Off macOS, pide's
/// JetBrains-style `cmd` defaults go on Ctrl, like JetBrains and VS Code there.
pub fn cmd_to_ctrl(keys: &str) -> String {
    keys.split_whitespace()
        .map(|k| {
            k.split('-')
                .map(|p| {
                    if p == "cmd" || p == "secondary" {
                        "ctrl"
                    } else {
                        p
                    }
                })
                .collect::<Vec<_>>()
                .join("-")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A key for people: `cmd-shift-o` → `⇧⌘O` on macOS (its modifier order),
/// `Ctrl+Shift+O` elsewhere. `secondary` is ⌘ on macOS and Ctrl elsewhere;
/// `cmd` is ⌘ / Super. Chords are separated by a space.
pub fn label(keys: &str) -> String {
    label_for(keys, cfg!(target_os = "macos"))
}

fn label_for(keys: &str, mac: bool) -> String {
    keys.split_whitespace()
        .map(|k| {
            let parts: Vec<&str> = k.split('-').collect();
            let (mods, key) = parts.split_at(parts.len().saturating_sub(1));
            let key = key.first().copied().unwrap_or("");
            let has = |m: &str| mods.contains(&m);
            let ctrl = has("ctrl") || (!mac && has("secondary"));
            let cmd = has("cmd") || has("super") || has("win") || (mac && has("secondary"));
            if mac {
                let mut out = String::new();
                for (on, sym) in [
                    (ctrl, '⌃'),
                    (has("alt"), '⌥'),
                    (has("shift"), '⇧'),
                    (cmd, '⌘'),
                ] {
                    if on {
                        out.push(sym);
                    }
                }
                out + &key_name(key, true)
            } else {
                let mut out: Vec<String> = Vec::new();
                for (on, name) in [
                    (ctrl, "Ctrl"),
                    (has("alt"), "Alt"),
                    (has("shift"), "Shift"),
                    (cmd, "Super"),
                ] {
                    if on {
                        out.push(name.to_string());
                    }
                }
                out.push(key_name(key, false));
                out.join("+")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn key_name(key: &str, mac: bool) -> String {
    match (key, mac) {
        ("enter", true) => "↩".into(),
        ("enter", false) => "Enter".into(),
        ("escape", true) => "⎋".into(),
        ("escape", false) => "Esc".into(),
        ("tab", true) => "⇥".into(),
        ("backspace", true) => "⌫".into(),
        ("up", true) => "↑".into(),
        ("down", true) => "↓".into(),
        ("left", true) => "←".into(),
        ("right", true) => "→".into(),
        (k, _) if k.chars().count() == 1 => k.to_uppercase(),
        (k, _) => {
            let mut c = k.chars();
            match c.next() {
                Some(f) if k.starts_with('f') && k[1..].chars().all(|c| c.is_ascii_digit()) => {
                    f.to_uppercase().collect::<String>() + c.as_str()
                }
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        }
    }
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
            .map(|k| label(&k.unparse(cx.platform_info().os)))
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

    #[::core::prelude::v1::test]
    fn labels_follow_the_platform() {
        assert_eq!(label_for("cmd-shift-o", true), "⇧⌘O");
        assert_eq!(label_for("cmd-shift-f12", true), "⇧⌘F12");
        assert_eq!(label_for("secondary-enter", true), "⌘↩");
        assert_eq!(label_for("ctrl-shift-o", false), "Ctrl+Shift+O");
        assert_eq!(label_for("secondary-enter", false), "Ctrl+Enter");
        assert_eq!(label_for("ctrl-alt-r", false), "Ctrl+Alt+R");
        assert_eq!(label_for("alt-enter", false), "Alt+Enter");
        assert_eq!(cmd_to_ctrl("cmd-k cmd-s"), "ctrl-k ctrl-s");
        assert_eq!(cmd_to_ctrl("cmd-shift-o"), "ctrl-shift-o");
    }
}
