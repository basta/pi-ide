//! `~/.config/pide/settings.json`. Small, typed, and tolerant: unknown keys are
//! kept on save, a broken file falls back to defaults (and is reported).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemeSetting {
    #[default]
    Dark,
    Light,
    /// Follow the macOS appearance.
    System,
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub theme: ThemeSetting,
    /// Prepend the active file / selection to prompts (spec: "send editor context"). Default on.
    pub send_context: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { theme: ThemeSetting::default(), send_context: true }
    }
}

impl gpui_kit::Global for Settings {}

pub fn settings_file() -> std::path::PathBuf {
    paths::config_dir().join("settings.json")
}

fn read_raw() -> Map<String, Value> {
    std::fs::read_to_string(settings_file())
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// Load settings. Called once at startup (a tiny file, before the window exists).
pub fn load() -> Settings {
    let raw = read_raw();
    let theme = raw
        .get("theme")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let send_context = raw.get("agent.send_context").and_then(|v| v.as_bool()).unwrap_or(true);
    Settings { theme, send_context }
}

/// Persist one key, keeping everything else in the file. Call off the UI thread.
pub fn save_key(key: &str, value: Value) -> std::io::Result<()> {
    let mut raw = read_raw();
    raw.insert(key.to_string(), value);
    let path = settings_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&Value::Object(raw))? + "\n")
}
