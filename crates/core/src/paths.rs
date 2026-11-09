//! Config and state locations. XDG-style on every platform (a developer tool;
//! matches pi and most CLIs), with the usual fallbacks.

use std::path::PathBuf;

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(fallback))
        .join("pide")
}

/// `$XDG_CONFIG_HOME/pide` (default `~/.config/pide`): keymap, settings.
pub fn config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config")
}

/// `$XDG_STATE_HOME/pide` (default `~/.local/state/pide`): logs, layout, pi RPC log.
pub fn state_dir() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state")
}

pub fn keymap_file() -> PathBuf {
    config_dir().join("keymap.json")
}
