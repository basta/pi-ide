//! Targets through the CMake File API (what CLion uses): before configuring,
//! a query file in the build folder asks for the code model; CMake writes the
//! reply next to it, and we read the targets, their kinds and artifacts from
//! there. Blocking file I/O: call off the UI thread.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde_json::Value;

const CLIENT: &str = "client-pide";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Executable,
    SharedLibrary,
    StaticLibrary,
    ModuleLibrary,
    ObjectLibrary,
    InterfaceLibrary,
    Utility,
}

impl Kind {
    fn parse(s: &str) -> Kind {
        match s {
            "EXECUTABLE" => Kind::Executable,
            "SHARED_LIBRARY" => Kind::SharedLibrary,
            "STATIC_LIBRARY" => Kind::StaticLibrary,
            "MODULE_LIBRARY" => Kind::ModuleLibrary,
            "OBJECT_LIBRARY" => Kind::ObjectLibrary,
            "INTERFACE_LIBRARY" => Kind::InterfaceLibrary,
            _ => Kind::Utility,
        }
    }

    pub fn is_library(self) -> bool {
        matches!(
            self,
            Kind::SharedLibrary
                | Kind::StaticLibrary
                | Kind::ModuleLibrary
                | Kind::ObjectLibrary
                | Kind::InterfaceLibrary
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub name: String,
    pub kind: Kind,
    /// The program to run (executables only), absolute.
    pub program: Option<PathBuf>,
    /// Where its CMakeLists.txt is, relative to the source root (`.` = top).
    pub source_dir: String,
}

/// Ask CMake for the code model on the next configure of `build`.
pub fn write_query(build: &Path) -> Result<()> {
    let dir = build.join(".cmake/api/v1/query").join(CLIENT);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::write(
        dir.join("query.json"),
        r#"{ "requests": [ { "kind": "codemodel", "version": 2 } ] }"#,
    )?;
    Ok(())
}

/// The targets from the newest reply in `build`, for configuration `config`
/// (multi-config generators have several; `None` or no match: the first).
/// `Ok(None)`: no reply yet (not configured with the query).
pub fn read_targets(build: &Path, config: Option<&str>) -> Result<Option<Vec<Target>>> {
    let reply = build.join(".cmake/api/v1/reply");
    let Ok(entries) = std::fs::read_dir(&reply) else {
        return Ok(None);
    };
    // index-<timestamp>.json: the newest sorts last.
    let Some(index) = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("index-") && n.ends_with(".json"))
        .max()
    else {
        return Ok(None);
    };
    let read = |name: &str| -> Result<Value> {
        let text = std::fs::read_to_string(reply.join(name))
            .with_context(|| format!("reading {name}"))?;
        Ok(serde_json::from_str(&text).with_context(|| format!("parsing {name}"))?)
    };
    let index = read(&index)?;
    let Some(codemodel) = index
        .get("objects")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|o| o.get("kind").and_then(Value::as_str) == Some("codemodel"))
        .and_then(|o| o.get("jsonFile").and_then(Value::as_str))
    else {
        return Ok(None);
    };
    let model = read(codemodel)?;
    let configs = model
        .get("configurations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let chosen = config
        .and_then(|c| {
            configs
                .iter()
                .find(|x| x.get("name").and_then(Value::as_str) == Some(c))
        })
        .or(configs.first());
    let Some(chosen) = chosen else {
        return Ok(Some(vec![]));
    };
    let dirs: Vec<String> = chosen
        .get("directories")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|d| d.get("source").and_then(Value::as_str).unwrap_or(".").to_string())
        .collect();
    let mut out = Vec::new();
    for t in chosen.get("targets").and_then(Value::as_array).into_iter().flatten() {
        let Some(file) = t.get("jsonFile").and_then(Value::as_str) else {
            continue;
        };
        let Ok(detail) = read(file) else { continue };
        // ALL_BUILD, ZERO_CHECK and friends.
        if detail.get("isGeneratorProvided").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let name = t.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
        let kind = Kind::parse(detail.get("type").and_then(Value::as_str).unwrap_or(""));
        let program = (kind == Kind::Executable)
            .then(|| {
                detail
                    .get("artifacts")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|a| a.get("path").and_then(Value::as_str))
                    .find(|p| !p.ends_with(".pdb") && !p.ends_with(".ilk"))
                    .map(|p| {
                        // Rebuilt from components: native separators (`bin/x.exe`
                        // under `D:\b` would otherwise read `D:\b\bin/x.exe`).
                        let p: PathBuf = Path::new(p).components().collect();
                        if p.is_absolute() { p } else { build.join(p) }
                    })
            })
            .flatten();
        let source_dir = t
            .get("directoryIndex")
            .and_then(Value::as_u64)
            .and_then(|i| dirs.get(i as usize).cloned())
            .unwrap_or_else(|| ".".into());
        out.push(Target {
            name,
            kind,
            program,
            source_dir,
        });
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reply laid out the way CMake 3.2x writes it.
    #[::core::prelude::v1::test]
    fn reads_targets_from_a_reply() {
        let build = std::env::temp_dir().join(format!("pide-fileapi-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&build);
        let reply = build.join(".cmake/api/v1/reply");
        std::fs::create_dir_all(&reply).unwrap();
        assert!(read_targets(&build, None).unwrap().is_none());
        let w = |name: &str, v: Value| std::fs::write(reply.join(name), v.to_string()).unwrap();
        w("index-2026-01-01T00-00-00-0000.json", serde_json::json!({"objects": []}));
        w(
            "index-2026-11-25T10-00-00-0000.json",
            serde_json::json!({"objects": [{"kind": "codemodel", "version": {"major": 2}, "jsonFile": "codemodel-v2-1.json"}]}),
        );
        w(
            "codemodel-v2-1.json",
            serde_json::json!({"configurations": [{
                "name": "Debug",
                "directories": [{"source": "."}, {"source": "tools/runner"}],
                "targets": [
                    {"name": "runner", "directoryIndex": 1, "jsonFile": "target-runner.json"},
                    {"name": "core", "directoryIndex": 0, "jsonFile": "target-core.json"},
                    {"name": "ALL_BUILD", "directoryIndex": 0, "jsonFile": "target-all.json"}
                ]
            }]}),
        );
        w(
            "target-runner.json",
            serde_json::json!({"name": "runner", "type": "EXECUTABLE",
                "artifacts": [{"path": "bin/runner.pdb"}, {"path": "bin/runner.exe"}]}),
        );
        w("target-core.json", serde_json::json!({"name": "core", "type": "SHARED_LIBRARY"}));
        w("target-all.json", serde_json::json!({"name": "ALL_BUILD", "type": "UTILITY", "isGeneratorProvided": true}));

        let ts = read_targets(&build, Some("Debug")).unwrap().unwrap();
        assert_eq!(ts.len(), 2);
        assert_eq!((ts[0].name.as_str(), ts[0].kind), ("core", Kind::SharedLibrary));
        assert_eq!(ts[1].kind, Kind::Executable);
        assert_eq!(ts[1].program.as_deref(), Some(build.join("bin/runner.exe").as_path()));
        assert_eq!(ts[1].source_dir, "tools/runner");
        let _ = std::fs::remove_dir_all(&build);
    }
}
