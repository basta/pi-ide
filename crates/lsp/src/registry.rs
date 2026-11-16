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
    /// Runs alongside a file's main server (spell checking): it adds
    /// diagnostics and code actions; completion, hover and the rest stay with
    /// the main one. Its `languageId` comes from the file's extension.
    pub addon: bool,
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
            addon: false,
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
            addon: false,
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
            addon: false,
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
            addon: false,
        },
        // Typst: completion, hover, outline, diagnostics as you type, and the
        // PDF: the editor asks it to export on save (`tinymist.exportPdf`).
        ServerSpec {
            id: "tinymist",
            language: "Typst",
            language_id: "typst",
            extensions: &["typ"],
            command: "tinymist".into(),
            args: vec!["lsp".into()],
            // Not on crates.io as a binary; elsewhere it's a GitHub release download.
            install: cfg!(target_os = "macos")
                .then(|| vec!["brew".into(), "install".into(), "tinymist".into()]),
            source: if cfg!(target_os = "macos") {
                "Homebrew"
            } else {
                "GitHub releases"
            },
            overridden: false,
            version_command: None,
            organize_imports_command: None,
            addon: false,
        },
        // Spell checking (Codebook): prose in LaTeX, Typst and Markdown, and in
        // code only comments, strings and the names you define. Alongside each
        // file's main server; "spellcheck.enabled" turns it off.
        ServerSpec {
            id: CODEBOOK,
            language: "Spelling",
            language_id: "plaintext",
            extensions: &[
                "tex", "typ", "md", "markdown", "txt", "rs", "py", "pyi", "ts", "mts", "cts",
                "tsx", "js", "mjs", "cjs", "jsx", "go", "c", "h", "cpp", "cc", "hpp", "java",
                "rb", "lua", "zig", "swift", "php", "sh", "bash", "toml", "yaml", "yml",
                "html", "css",
            ],
            command: "codebook-lsp".into(),
            args: vec!["serve".into()],
            install: Some(if cfg!(target_os = "macos") {
                vec!["brew".into(), "install".into(), "codebook-lsp".into()]
            } else {
                vec![
                    "cargo".into(),
                    "install".into(),
                    "--locked".into(),
                    "codebook-lsp".into(),
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
            addon: true,
        },
    ]
}

/// The spell checker's server id.
pub const CODEBOOK: &str = "codebook";

/// The `languageId` an add-on server gets for a file (Codebook picks its
/// parser by it): VS Code's ids, by extension.
pub fn language_id_for(spec: &ServerSpec, path: &Path) -> &'static str {
    if !spec.addon {
        return spec.language_id;
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    match ext {
        "tex" => "latex",
        "typ" => "typst",
        "md" | "markdown" => "markdown",
        "rs" => "rust",
        "py" | "pyi" => "python",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "go" => "go",
        "c" | "h" => "c",
        "cpp" | "cc" | "hpp" => "cpp",
        "java" => "java",
        "rb" => "ruby",
        "lua" => "lua",
        "zig" => "zig",
        "swift" => "swift",
        "php" => "php",
        "sh" | "bash" => "shellscript",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "html" => "html",
        "css" => "css",
        _ => spec.language_id,
    }
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

/// The main server for a file, by extension.
pub fn spec_for<'a>(specs: &'a [ServerSpec], path: &Path) -> Option<&'a ServerSpec> {
    let ext = path.extension()?.to_str()?;
    specs.iter().find(|s| !s.addon && s.extensions.contains(&ext))
}

/// The add-on servers for a file (spell checking), by extension.
pub fn addons_for<'a>(specs: &'a [ServerSpec], path: &Path) -> Vec<&'a ServerSpec> {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return vec![];
    };
    specs
        .iter()
        .filter(|s| s.addon && s.extensions.contains(&ext))
        .collect()
}

pub use pide_runtime::process::{find_command, search_path};

#[derive(Clone, Debug, PartialEq)]
pub enum Installed {
    /// Found and `--version` worked.
    Yes { path: PathBuf, version: String },
    /// Not on the search path, or a stub that fails (rustup's proxy without the component).
    No { reason: String },
}

/// The version from `--version` output: its first line, unless that is only
/// the name (tinymist prints `tinymist`, then `Build Git Describe: v0.15.12`,
/// …, `Typst Version: 0.15.1`): then the name and the first version-ish field.
fn version_text(out: &str) -> String {
    let mut lines = out.lines().map(str::trim).filter(|l| !l.is_empty());
    let Some(first) = lines.next() else {
        return String::new();
    };
    if first.chars().any(|c| c.is_ascii_digit()) {
        return first.to_string();
    }
    let field = lines.find_map(|l| {
        let (key, value) = l.split_once(':')?;
        let key = key.to_ascii_lowercase();
        (key.contains("describe") || key.contains("version"))
            .then(|| value.trim())
            .filter(|v| !v.is_empty())
    });
    match field {
        Some(v) => format!("{first} {v}"),
        None => first.to_string(),
    }
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
        let v = version_text(&String::from_utf8_lossy(&out.stdout));
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
    fn version_lines() {
        assert_eq!(
            version_text("rust-analyzer 1.91.0 (f8297e3 2025-10-28)\n"),
            "rust-analyzer 1.91.0 (f8297e3 2025-10-28)"
        );
        assert_eq!(
            version_text(
                "tinymist\nBuild Timestamp: 2026-10-01\nBuild Git Describe: v0.15.12\nTypst Version: 0.15.1\n"
            ),
            "tinymist v0.15.12"
        );
        assert_eq!(version_text("tinymist\n"), "tinymist");
        assert_eq!(version_text(""), "");
    }

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

    #[test]
    fn spelling_runs_alongside() {
        let specs = specs(None);
        // The main server stays the main one; Codebook comes on top.
        assert_eq!(spec_for(&specs, Path::new("a.typ")).unwrap().id, "tinymist");
        let addons = addons_for(&specs, Path::new("a.typ"));
        assert_eq!(addons.iter().map(|s| s.id).collect::<Vec<_>>(), [CODEBOOK]);
        assert_eq!(language_id_for(addons[0], Path::new("a.typ")), "typst");
        assert_eq!(language_id_for(addons[0], Path::new("x.tex")), "latex");
        // Markdown has no main server, only spelling.
        assert!(spec_for(&specs, Path::new("README.md")).is_none());
        assert_eq!(addons_for(&specs, Path::new("README.md")).len(), 1);
        assert!(addons_for(&specs, Path::new("a.png")).is_empty());
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
