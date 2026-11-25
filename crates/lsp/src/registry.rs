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
        // C, C++ and CUDA. Needs a compile_commands.json for real results;
        // `extra_args` points it at one in a CMake build folder.
        ServerSpec {
            id: CLANGD,
            language: "C/C++",
            language_id: "cpp",
            extensions: C_FAMILY,
            command: "clangd".into(),
            args: vec![],
            install: if cfg!(windows) {
                Some(vec![
                    "winget".into(),
                    "install".into(),
                    "-e".into(),
                    "--id".into(),
                    "LLVM.LLVM".into(),
                ])
            } else if cfg!(target_os = "macos") {
                None // Xcode's command line tools ship it
            } else {
                None // the distribution's clangd package
            },
            source: if cfg!(windows) {
                "LLVM (winget)"
            } else if cfg!(target_os = "macos") {
                "Xcode command line tools"
            } else {
                "distribution package"
            },
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
                "tsx", "js", "mjs", "cjs", "jsx", "go", "c", "h", "cpp", "cc", "cxx", "hpp",
                "hh", "hxx", "cu", "cuh", "java",
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

/// clangd's server id.
pub const CLANGD: &str = "clangd";

/// C, C++ and CUDA sources and headers.
const C_FAMILY: &[&str] = &[
    "c", "h", "cpp", "cc", "cxx", "c++", "hpp", "hh", "hxx", "h++", "inl", "ipp", "tpp", "ixx",
    "cppm", "cu", "cuh",
];

/// Arguments added when a server starts in `root`, after looking at the
/// project. Blocking (reads a few directories): call off the UI thread.
///
/// clangd only looks for `compile_commands.json` next to the sources and in
/// `build/`; CMake presets put it in `out/build/<preset>/` and CLion in
/// `cmake-build-<type>/`. Point it at the newest one there, unless the user
/// already said where (`--compile-commands-dir` in settings, or `.clangd`).
pub fn extra_args(id: &str, args: &[String], root: &Path) -> Vec<String> {
    if id != CLANGD || args.iter().any(|a| a.contains("compile-commands-dir")) {
        return vec![];
    }
    match compile_commands_dir(root) {
        Some(dir) => {
            log::info!("clangd: using {}", dir.join("compile_commands.json").display());
            vec![format!("--compile-commands-dir={}", dir.display())]
        }
        None => vec![],
    }
}

/// Environment added when a server starts. Blocking the first time: call off
/// the UI thread. clangd on Windows gets Visual Studio's developer environment
/// (`msvc::dev_env`), so cl.exe command lines find ATL/MFC and friends.
/// Its `PATH` stays out: servers get pide's search path.
pub fn extra_env(id: &str) -> Vec<(String, String)> {
    if id != CLANGD {
        return vec![];
    }
    crate::msvc::dev_env()
        .iter()
        .filter(|(k, _)| !k.eq_ignore_ascii_case("PATH"))
        .cloned()
        .collect()
}

/// The folder of the newest `compile_commands.json` in the project's build
/// folders, when clangd wouldn't find one by itself.
fn compile_commands_dir(root: &Path) -> Option<PathBuf> {
    const DB: &str = "compile_commands.json";
    if root.join(DB).is_file() || root.join("build").join(DB).is_file() {
        return None;
    }
    if std::fs::read_to_string(root.join(".clangd"))
        .is_ok_and(|s| s.contains("CompilationDatabase"))
    {
        return None;
    }
    let is_build_dir = |name: &str| {
        let n = name.to_ascii_lowercase();
        n == "out" || n == "_build" || n.starts_with("build") || n.starts_with("cmake-build")
    };
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = vec![];
    // Breadth-first, at most three levels below the root (out/build/Debug).
    let mut level: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| e.file_name().to_str().is_some_and(is_build_dir))
        .map(|e| e.path())
        .collect();
    for _ in 0..3 {
        let mut next = vec![];
        for dir in level {
            let db = dir.join(DB);
            if let Ok(meta) = std::fs::metadata(&db) {
                found.push((meta.modified().unwrap_or(std::time::UNIX_EPOCH), dir.clone()));
            }
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            next.extend(
                entries
                    .flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                    .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
                    // CMake's own scratch folders never hold the database.
                    .filter(|e| e.file_name() != "CMakeFiles")
                    .map(|e| e.path()),
            );
        }
        level = next;
    }
    found.into_iter().max_by_key(|(t, _)| *t).map(|(_, d)| d)
}

/// The `languageId` an add-on server gets for a file (Codebook picks its
/// parser by it): VS Code's ids, by extension.
pub fn language_id_for(spec: &ServerSpec, path: &Path) -> &'static str {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if spec.id == CLANGD {
        return match ext {
            "c" => "c",
            "cu" | "cuh" => "cuda-cpp",
            _ => "cpp",
        };
    }
    if !spec.addon {
        return spec.language_id;
    }
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
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "cu" | "cuh" => "cuda-cpp",
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
/// …, `Typst Version: 0.15.1`): then the name and the first version field.
fn version_text(out: &str) -> String {
    let mut lines = out.lines().map(str::trim).filter(|l| !l.is_empty());
    let Some(first) = lines.next() else {
        return String::new();
    };
    if first.chars().any(|c| c.is_ascii_digit()) {
        return first.to_string();
    }
    // Only values with a digit: Homebrew's build says `VERGEN_IDEMPOTENT_OUTPUT`.
    let field = lines.find_map(|l| {
        let (key, value) = l.split_once(':')?;
        let (key, value) = (key.trim(), value.trim());
        let k = key.to_ascii_lowercase();
        ((k.contains("describe") || k.contains("version"))
            && value.chars().any(|c| c.is_ascii_digit()))
        .then(|| {
            if k.contains("describe") {
                value.to_string()
            } else {
                format!("({key} {value})")
            }
        })
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
    let mut child = match pide_runtime::process::command_for(&program)
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
                let _ = child.wait(); // no zombie left behind
                // It's there but slow (a rustup proxy while everything else
                // starts up): treat it as installed rather than raising a false
                // "not installed"; starting it will tell if it really works.
                log::warn!("{} --version didn't answer in 5 s", path.display());
                return Installed::Yes {
                    path,
                    version: "version unknown (slow to answer)".into(),
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
        assert_eq!(
            version_text(
                "tinymist\nBuild Git Describe: VERGEN_IDEMPOTENT_OUTPUT\nTypst Version: 0.15.1\n"
            ),
            "tinymist (Typst Version 0.15.1)"
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

    #[test]
    fn c_family_goes_to_clangd() {
        let specs = specs(None);
        for f in ["a.c", "a.h", "a.cpp", "a.hpp", "k.cu", "k.cuh", "x.inl"] {
            assert_eq!(spec_for(&specs, Path::new(f)).unwrap().id, CLANGD, "{f}");
        }
        let clangd = spec_for(&specs, Path::new("a.c")).unwrap();
        assert_eq!(language_id_for(clangd, Path::new("a.c")), "c");
        assert_eq!(language_id_for(clangd, Path::new("a.h")), "cpp");
        assert_eq!(language_id_for(clangd, Path::new("k.cu")), "cuda-cpp");
    }

    #[test]
    fn compile_commands_in_a_cmake_preset_folder() {
        let root = std::env::temp_dir().join(format!("pide-cc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let debug = root.join("out").join("build").join("Debug");
        std::fs::create_dir_all(&debug).unwrap();
        std::fs::create_dir_all(root.join("src").join("build")).unwrap();
        // Nothing yet: no argument.
        assert!(extra_args(CLANGD, &[], &root).is_empty());
        std::fs::write(debug.join("compile_commands.json"), "[]").unwrap();
        assert_eq!(
            extra_args(CLANGD, &[], &root),
            [format!("--compile-commands-dir={}", debug.display())]
        );
        // Only clangd, and not when the user chose one.
        assert!(extra_args("rust-analyzer", &[], &root).is_empty());
        let mine = ["--compile-commands-dir=x".to_string()];
        assert!(extra_args(CLANGD, &mine, &root).is_empty());
        // One where clangd looks anyway wins.
        std::fs::create_dir_all(root.join("build")).unwrap();
        std::fs::write(root.join("build").join("compile_commands.json"), "[]").unwrap();
        assert!(extra_args(CLANGD, &[], &root).is_empty());
        let _ = std::fs::remove_dir_all(&root);
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
