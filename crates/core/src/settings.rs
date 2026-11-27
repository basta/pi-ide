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
    /// `"lsp"`: per-server overrides (`{"rust-analyzer": {"command": ..., "args": [...]}}`), read by pide-lsp.
    pub lsp: Option<Value>,
    /// `"agent.notify"`: a desktop notification when pi finishes or asks while
    /// pide isn't the active app. Default on.
    pub notify: bool,
    /// `"agent.save_before_send"`: save unsaved files before a prompt goes to the
    /// agent, which reads files from disk. Default on.
    pub save_before_send: bool,
    /// `"editor.format_on_save"`: reformat through the language server before saving. Default off.
    pub format_on_save: bool,
    /// `"agent.collab"`: load pide's pi extension (collaborator mode: pins,
    /// explanations, tours, diagnostics) into the pi processes pide starts. Default on.
    pub collab: bool,
    /// `"typst.pdf_on_type"` (experimental): export a Typst document's PDF after
    /// every change, from the editor's text (no save needed). Default on.
    pub typst_pdf_on_type: bool,
    /// `"typst.pdf_follow_edits"`: after an export, scroll the PDF to what
    /// changed. Default on.
    pub typst_pdf_follow: bool,
    /// `"spellcheck.enabled"`: spell check through Codebook (`codebook-lsp`),
    /// alongside each file's language server. Default on (nothing happens
    /// until it's installed).
    pub spellcheck: bool,
    /// `"tool_windows.disabled"`: tool windows turned off everywhere (by id,
    /// e.g. `git_log`, `script:user/todos`). A project can turn one back on
    /// (see [`ProjectSettings`]).
    pub tool_windows_off: Vec<String>,
    /// `"terminal.shell"`: the terminal's shell, `"bash"` or `["bash", "-l"]`. Default:
    /// pwsh (else Windows PowerShell) on Windows, `$SHELL -l` elsewhere.
    pub terminal_shell: Option<Vec<String>>,
    /// `"terminal.font_family"`: default the editor's mono font. A Nerd Font here
    /// draws the glyphs prompt themes (oh-my-posh, starship) use.
    pub terminal_font_family: Option<String>,
    /// `"terminal.font_size"`: default the editor's.
    pub terminal_font_size: Option<f32>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeSetting::default(),
            send_context: true,
            lsp: None,
            notify: true,
            save_before_send: true,
            format_on_save: false,
            collab: true,
            typst_pdf_on_type: true,
            typst_pdf_follow: true,
            spellcheck: true,
            tool_windows_off: Vec::new(),
            terminal_shell: None,
            terminal_font_family: None,
            terminal_font_size: None,
        }
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
    let send_context = raw
        .get("agent.send_context")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    Settings {
        theme,
        send_context,
        lsp: raw.get("lsp").cloned(),
        notify: raw
            .get("agent.notify")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        save_before_send: raw
            .get("agent.save_before_send")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        format_on_save: raw
            .get("editor.format_on_save")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        collab: raw
            .get("agent.collab")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        typst_pdf_on_type: raw
            .get("typst.pdf_on_type")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        typst_pdf_follow: raw
            .get("typst.pdf_follow_edits")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        spellcheck: raw
            .get("spellcheck.enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        tool_windows_off: string_list(raw.get(TOOL_WINDOWS_OFF)),
        terminal_shell: raw.get("terminal.shell").and_then(command_line),
        terminal_font_family: raw
            .get("terminal.font_family")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        terminal_font_size: raw
            .get("terminal.font_size")
            .and_then(|v| v.as_f64())
            .map(|v| v as f32)
            .filter(|v| (6.0..=72.0).contains(v)),
    }
}

pub const TOOL_WINDOWS_OFF: &str = "tool_windows.disabled";
pub const TOOL_WINDOWS_ON: &str = "tool_windows.enabled";

fn string_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// This project: `<project>/.pide/settings.json`. Only what can differ per
// project lives there (for now, which tool windows are on).

/// One project's own settings, overriding the user's.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProjectSettings {
    /// Tool windows off in this project, and ones on here though off everywhere.
    pub tool_windows_off: Vec<String>,
    pub tool_windows_on: Vec<String>,
}

pub fn project_file(root: &std::path::Path) -> std::path::PathBuf {
    root.join(".pide").join("settings.json")
}

fn read_project_raw(root: &std::path::Path) -> Map<String, Value> {
    std::fs::read_to_string(project_file(root))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// A project's settings. Tiny; read when its window opens.
pub fn load_project(root: &std::path::Path) -> ProjectSettings {
    let raw = read_project_raw(root);
    ProjectSettings {
        tool_windows_off: string_list(raw.get(TOOL_WINDOWS_OFF)),
        tool_windows_on: string_list(raw.get(TOOL_WINDOWS_ON)),
    }
}

/// Persist one key of a project's settings (an empty list removes the key, and
/// an empty file isn't written). Call off the UI thread.
pub fn save_project_key(root: &std::path::Path, key: &str, value: Value) -> std::io::Result<()> {
    let mut raw = read_project_raw(root);
    let empty = value.as_array().is_some_and(|a| a.is_empty()) || value.is_null();
    if empty {
        raw.remove(key);
    } else {
        raw.insert(key.to_string(), value);
    }
    let path = project_file(root);
    if raw.is_empty() {
        // Nothing set for this project any more: no file either.
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        path,
        serde_json::to_string_pretty(&Value::Object(raw))? + "\n",
    )
}

/// `"program"` or `["program", "arg", …]`.
fn command_line(v: &Value) -> Option<Vec<String>> {
    let parts: Vec<String> = match v {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a.iter().map(|x| x.as_str().map(str::to_string)).collect::<Option<_>>()?,
        _ => return None,
    };
    parts.first().is_some_and(|p| !p.is_empty()).then_some(parts)
}

/// Persist one key, keeping everything else in the file. Call off the UI thread.
pub fn save_key(key: &str, value: Value) -> std::io::Result<()> {
    let mut raw = read_raw();
    raw.insert(key.to_string(), value);
    let path = settings_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        path,
        serde_json::to_string_pretty(&Value::Object(raw))? + "\n",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_setting_is_a_string_or_a_list() {
        assert_eq!(command_line(&serde_json::json!("bash")), Some(vec!["bash".into()]));
        assert_eq!(
            command_line(&serde_json::json!(["bash", "-l"])),
            Some(vec!["bash".into(), "-l".into()])
        );
        assert_eq!(command_line(&serde_json::json!("")), None);
        assert_eq!(command_line(&serde_json::json!(["bash", 1])), None);
        assert_eq!(command_line(&serde_json::json!([])), None);
    }
}
