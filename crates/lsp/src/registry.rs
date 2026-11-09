//! The language servers pide knows, how to find them, and how to install them.
//!
//! `settings.json` can override a server's command and arguments:
//! `"lsp": { "rust-analyzer": { "command": "/opt/ra", "args": [] } }`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub struct ServerSpec {
    /// Stable id, also the `settings.json` key.
    pub id: &'static str,
    /// Shown in the UI.
    pub language: &'static str,
    /// LSP `languageId`.
    pub language_id: &'static str,
    pub extensions: &'static [&'static str],
    pub command: String,
    pub args: Vec<String>,
    /// Shell-free install command (program, args...), run on request.
    pub install: Option<Vec<String>>,
    /// Where it comes from, for the Language Servers tab.
    pub source: &'static str,
    /// From `settings.json` rather than the built-in default.
    pub overridden: bool,
}

fn builtin() -> Vec<ServerSpec> {
    vec![
        ServerSpec {
            id: "rust-analyzer",
            language: "Rust",
            language_id: "rust",
            extensions: &["rs"],
            command: "rust-analyzer".into(),
            args: vec![],
            install: Some(vec!["rustup".into(), "component".into(), "add".into(), "rust-analyzer".into()]),
            source: "rustup component",
            overridden: false,
        },
        ServerSpec {
            id: "pyright",
            language: "Python",
            language_id: "python",
            extensions: &["py", "pyi"],
            command: "pyright-langserver".into(),
            args: vec!["--stdio".into()],
            install: Some(vec!["npm".into(), "install".into(), "-g".into(), "pyright".into()]),
            source: "npm package",
            overridden: false,
        },
    ]
}

/// Built-in servers with `settings.json`'s `"lsp"` overrides applied.
pub fn specs(lsp_settings: Option<&Value>) -> Vec<ServerSpec> {
    let mut specs = builtin();
    let Some(overrides) = lsp_settings.and_then(Value::as_object) else { return specs };
    for spec in &mut specs {
        let Some(o) = overrides.get(spec.id) else { continue };
        if let Some(cmd) = o.get("command").and_then(Value::as_str) {
            spec.command = cmd.to_string();
            spec.overridden = true;
        }
        if let Some(args) = o.get("args").and_then(Value::as_array) {
            spec.args = args.iter().filter_map(|a| a.as_str().map(str::to_string)).collect();
            spec.overridden = true;
        }
    }
    specs
}

/// The server for a file, by extension.
pub fn spec_for<'a>(specs: &'a [ServerSpec], path: &Path) -> Option<&'a ServerSpec> {
    let ext = path.extension()?.to_str()?;
    specs.iter().find(|s| s.extensions.contains(&ext))
}

/// Directories searched for commands: PATH, plus where rustup and npm put things
/// (an app started from Finder doesn't get the shell's PATH).
pub fn search_path() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        for extra in [".cargo/bin", ".local/bin", ".npm-global/bin"] {
            dirs.push(home.join(extra));
        }
    }
    for extra in ["/opt/homebrew/bin", "/usr/local/bin"] {
        dirs.push(PathBuf::from(extra));
    }
    dirs.dedup();
    dirs
}

/// `command` as an absolute path, if it exists (a path, or a name on the search path).
pub fn find_command(command: &str) -> Option<PathBuf> {
    let p = Path::new(command);
    if p.components().count() > 1 {
        return p.is_file().then(|| p.to_path_buf());
    }
    search_path().into_iter().map(|d| d.join(command)).find(|c| c.is_file())
}

#[derive(Clone, Debug, PartialEq)]
pub enum Installed {
    /// Found and `--version` worked.
    Yes { path: PathBuf, version: String },
    /// Not on the search path, or a stub that fails (rustup's proxy without the component).
    No { reason: String },
}

/// Blocking (runs the server with `--version`, ≤ 5 s): call off the UI thread.
pub fn detect(spec: &ServerSpec) -> Installed {
    let Some(path) = find_command(&spec.command) else {
        return Installed::No { reason: format!("`{}` not found", spec.command) };
    };
    let mut child = match std::process::Command::new(&path)
        .arg("--version")
        .env("PATH", std::env::join_paths(search_path()).unwrap_or_default())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return Installed::No { reason: format!("can't run {}: {e}", path.display()) },
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                return Installed::No { reason: format!("{} --version didn't answer", path.display()) };
            }
        }
    }
    let out = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => return Installed::No { reason: e.to_string() },
    };
    let first_line = |b: &[u8]| String::from_utf8_lossy(b).lines().next().unwrap_or("").trim().to_string();
    if out.status.success() {
        let v = first_line(&out.stdout);
        let version = if v.is_empty() { first_line(&out.stderr) } else { v };
        Installed::Yes { path, version }
    } else {
        Installed::No { reason: first_line(&out.stderr) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn overrides_apply_and_lookup_is_by_extension() {
        let specs = specs(Some(&json!({ "rust-analyzer": { "command": "/opt/ra", "args": ["--log"] } })));
        let ra = spec_for(&specs, Path::new("src/main.rs")).unwrap();
        assert_eq!((ra.command.as_str(), ra.args.as_slice(), ra.overridden), ("/opt/ra", &["--log".to_string()][..], true));
        let py = spec_for(&specs, Path::new("a/b.pyi")).unwrap();
        assert_eq!((py.id, py.overridden), ("pyright", false));
        assert!(spec_for(&specs, Path::new("README.md")).is_none());
    }

    #[test]
    fn missing_command_is_not_installed() {
        let mut s = specs(None).remove(0);
        s.command = "definitely-not-a-real-server-xyz".into();
        assert!(matches!(detect(&s), Installed::No { .. }));
    }
}
