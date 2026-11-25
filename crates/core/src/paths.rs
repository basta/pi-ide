//! Config and state locations. XDG-style on every platform (a developer tool;
//! matches pi and most CLIs), with the usual fallbacks.

use std::path::{Path, PathBuf};

/// `std::fs::canonicalize`, minus the `\\?\` Windows puts on drive paths: git, language
/// servers and pi's session folders all expect the plain `D:\…` form.
pub fn canonicalize(path: &Path) -> std::io::Result<PathBuf> {
    let p = std::fs::canonicalize(path)?;
    if cfg!(windows)
        && let Some(plain) = p.to_str().and_then(|s| s.strip_prefix(r"\\?\"))
        && plain.as_bytes().get(1) == Some(&b':')
    {
        return Ok(PathBuf::from(plain));
    }
    Ok(p)
}

/// The user's home: `HOME`, else (Windows, started from Explorer) `USERPROFILE`.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

fn home() -> PathBuf {
    home_dir().unwrap_or_else(|| PathBuf::from("."))
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

/// `$XDG_DATA_HOME/pide` (default `~/.local/share/pide`): things pide installs (pdfium).
pub fn data_dir() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share")
}

pub fn keymap_file() -> PathBuf {
    config_dir().join("keymap.json")
}
