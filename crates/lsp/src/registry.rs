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
    /// How to read the version. `None`: `<command> --version`.
    /// pyright's server refuses `--version` (it wants `--stdio`); its CLI answers.
    pub version_command: Option<Vec<String>>,
    /// The server's own organize-imports command, when it has no
    /// `source.organizeImports` code action (pyright runs it without advertising it).
    pub organize_imports_command: Option<&'static str>,
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
            install: Some(vec![
                "rustup".into(),
                "component".into(),
                "add".into(),
                "rust-analyzer".into(),
            ]),
            source: "rustup component",
            overridden: false,
            version_command: None,
            organize_imports_command: None,
        },
        ServerSpec {
            id: "pyright",
            language: "Python",
            language_id: "python",
            extensions: &["py", "pyi"],
            command: "pyright-langserver".into(),
            args: vec!["--stdio".into()],
            install: Some(vec![
                "npm".into(),
                "install".into(),
                "-g".into(),
                "pyright".into(),
            ]),
            source: "npm package",
            overridden: false,
            version_command: Some(vec!["pyright".into(), "--version".into()]),
            organize_imports_command: Some("pyright.organizeimports"),
        },
        ServerSpec {
            id: "typescript-language-server",
            language: "TypeScript",
            language_id: "typescript",
            // One languageId per server: .js / .tsx would need their own.
            extensions: &["ts", "mts", "cts"],
            command: "typescript-language-server".into(),
            args: vec!["--stdio".into()],
            install: Some(vec![
                "npm".into(),
                "install".into(),
                "-g".into(),
                "typescript-language-server".into(),
                "typescript".into(),
            ]),
            source: "npm package",
            overridden: false,
            version_command: None,
            organize_imports_command: None,
        },
        // Completion, references, outline, and the build: the editor asks it to
        // build on save (`textDocument/build`: latexmk -pdf -synctex=1), and
        // errors from the LaTeX log come back as diagnostics.
        ServerSpec {
            id: "texlab",
            language: "LaTeX",
            language_id: "latex",
            extensions: &["tex", "sty", "cls", "ltx", "dtx"],
            command: "texlab".into(),
            args: vec![],
            install: Some(if cfg!(target_os = "macos") {
                vec!["brew".into(), "install".into(), "texlab".into()]
            } else {
                vec![
                    "cargo".into(),
                    "install".into(),
                    "--locked".into(),
                    "texlab".into(),
                ]
            }),
            source: if cfg!(target_os = "macos") {
                "Homebrew"
            } else {
                "cargo install"
            },
            overridden: false,
            version_command: None,
            organize_imports_command: None,
        },
    ]
}

/// Built-in servers with `settings.json`'s `"lsp"` overrides applied.
pub fn specs(lsp_settings: Option<&Value>) -> Vec<ServerSpec> {
    let mut specs = builtin();
    let Some(overrides) = lsp_settings.and_then(Value::as_object) else {
        return specs;
    };
    for spec in &mut specs {
        let Some(o) = overrides.get(spec.id) else {
            continue;
        };
        if let Some(cmd) = o.get("command").and_then(Value::as_str) {
            spec.command = cmd.to_string();
            spec.overridden = true;
            spec.version_command = None; // the built-in check is for the built-in command
        }
        if let Some(args) = o.get("args").and_then(Value::as_array) {
            spec.args = args
                .iter()
                .filter_map(|a| a.as_str().map(str::to_string))
                .collect();
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

pub use pide_runtime::process::{find_command, search_path};

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
        return Installed::No {
            reason: format!("`{}` not found", spec.command),
        };
    };
    // A custom command from settings.json: we don't know how to ask its version.
    if spec.overridden {
        return Installed::Yes {
            path,
            version: "custom command".into(),
        };
    }
    let (program, args) = match &spec.version_command {
        Some(v) => match find_command(&v[0]) {
            Some(p) => (p, v[1..].to_vec()),
            None => {
                return Installed::No {
                    reason: format!("`{}` not found", v[0]),
                };
            }
        },
        None => (path.clone(), vec!["--version".to_string()]),
    };
    let mut child = match std::process::Command::new(&program)
        .args(&args)
        .env(
            "PATH",
            std::env::join_paths(search_path()).unwrap_or_default(),
        )
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return Installed::No {
                reason: format!("can't run {}: {e}", path.display()),
            };
        }
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                return Installed::No {
                    reason: format!("{} --version didn't answer", path.display()),
                };
            }
        }
    }
    let out = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            return Installed::No {
                reason: e.to_string(),
            };
        }
    };
    let first_line = |b: &[u8]| {
        String::from_utf8_lossy(b)
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_string()
    };
    if out.status.success() {
        let v = first_line(&out.stdout);
        let version = if v.is_empty() {
            first_line(&out.stderr)
        } else {
            v
        };
        Installed::Yes { path, version }
    } else {
        Installed::No {
            reason: first_line(&out.stderr),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn overrides_apply_and_lookup_is_by_extension() {
        let specs = specs(Some(
            &json!({ "rust-analyzer": { "command": "/opt/ra", "args": ["--log"] } }),
        ));
        let ra = spec_for(&specs, Path::new("src/main.rs")).unwrap();
        assert_eq!(
            (ra.command.as_str(), ra.args.as_slice(), ra.overridden),
            ("/opt/ra", &["--log".to_string()][..], true)
        );
        let py = spec_for(&specs, Path::new("a/b.pyi")).unwrap();
        assert_eq!((py.id, py.overridden), ("pyright", false));
        assert!(spec_for(&specs, Path::new("README.md")).is_none());
    }

    /// pyright's server rejects `--version`; detection asks its CLI instead.
    #[test]
    fn installed_pyright_is_detected() {
        let py = specs(None).into_iter().find(|s| s.id == "pyright").unwrap();
        if find_command(&py.command).is_none() {
            return; // not installed on this machine
        }
        match detect(&py) {
            Installed::Yes { version, .. } => assert!(version.starts_with("pyright"), "{version}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn missing_command_is_not_installed() {
        let mut s = specs(None).remove(0);
        s.command = "definitely-not-a-real-server-xyz".into();
        assert!(matches!(detect(&s), Installed::No { .. }));
    }
}
