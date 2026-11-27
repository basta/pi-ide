//! CMake profiles, as CLion calls them: one per visible configure preset in
//! `CMakePresets.json` / `CMakeUserPresets.json` (with `inherits`, `include`,
//! `condition` and the `${…}` macros resolved), or, for a project without
//! presets, CLion's default `Debug` in `cmake-build-debug` (and `Release`).
//! Pure: reading files is the caller's job, tested below.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde_json::{Map, Value};

#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    /// The configure preset's name (or `Debug` / `Release`).
    pub name: String,
    /// What the picker shows: the preset's `displayName`, else its name.
    pub display: String,
    /// Absolute build folder.
    pub binary_dir: PathBuf,
    /// Arguments after `cmake` that configure it.
    pub configure: Vec<String>,
    /// The first visible build preset for this configure preset.
    pub build_preset: Option<String>,
    /// `CMAKE_BUILD_TYPE`, or the build preset's `configuration`.
    pub build_type: Option<String>,
    pub from_preset: bool,
}

impl Profile {
    /// Arguments after `cmake` that build `target` (`None`: everything).
    /// `clean_first`: Rebuild.
    pub fn build_args(&self, target: Option<&str>, clean_first: bool) -> Vec<String> {
        let mut args = vec!["--build".to_string()];
        match &self.build_preset {
            Some(p) => args.extend(["--preset".into(), p.clone()]),
            None => {
                args.push(self.binary_dir.to_string_lossy().into_owned());
                // Multi-config generators (Visual Studio) need it; others ignore it.
                if let Some(t) = &self.build_type {
                    args.extend(["--config".into(), t.clone()]);
                }
            }
        }
        if let Some(t) = target {
            args.extend(["--target".into(), t.to_string()]);
        }
        if clean_first {
            args.push("--clean-first".into());
        }
        args
    }

    pub fn is_configured(&self) -> bool {
        self.binary_dir.join("CMakeCache.txt").is_file()
    }
}

/// The files a project's presets come from (both optional).
pub const PRESET_FILES: [&str; 2] = ["CMakePresets.json", "CMakeUserPresets.json"];

/// CLion's defaults for a project without presets. `ninja`: generate Ninja
/// files (fast, and the only way to a `compile_commands.json` on Windows,
/// where CMake's default is a Visual Studio solution).
pub fn default_profiles(root: &Path, ninja: bool) -> Vec<Profile> {
    ["Debug", "Release"]
        .into_iter()
        .map(|ty| {
            let dir = root.join(format!("cmake-build-{}", ty.to_ascii_lowercase()));
            let mut configure = vec![
                "-S".to_string(),
                root.to_string_lossy().into_owned(),
                "-B".to_string(),
                dir.to_string_lossy().into_owned(),
                format!("-DCMAKE_BUILD_TYPE={ty}"),
            ];
            if ninja {
                configure.extend(["-G".into(), "Ninja".into()]);
            }
            Profile {
                name: ty.into(),
                display: ty.into(),
                binary_dir: dir,
                configure,
                build_preset: None,
                build_type: Some(ty.into()),
                from_preset: false,
            }
        })
        .collect()
}

/// Profiles from preset files: `files` are `(path, text)` for the files that
/// exist, the project's first. `read` loads an `include`d file.
pub fn profiles(
    root: &Path,
    files: &[(PathBuf, String)],
    read: &dyn Fn(&Path) -> Option<String>,
) -> Result<Vec<Profile>> {
    let mut configure: Vec<Preset> = Vec::new();
    let mut build: Vec<Preset> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for (path, text) in files {
        load(path, text, read, &mut configure, &mut build, &mut seen)?;
    }
    let env = Env {
        root,
        host: host_system_name(),
    };
    let by_name: HashMap<&str, &Preset> = configure.iter().map(|p| (p.name.as_str(), p)).collect();
    let builds: HashMap<&str, &Preset> = build.iter().map(|p| (p.name.as_str(), p)).collect();
    let mut out = Vec::new();
    for p in &configure {
        if p.raw.get("hidden").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let merged = resolve(&p.name, &by_name, 0)?;
        let vars = Vars::new(&env, &p.name, &merged, &p.file_dir);
        if !condition(merged.get("condition"), &vars) {
            continue;
        }
        let Some(dir) = merged.get("binaryDir").and_then(Value::as_str) else {
            // No binaryDir: CMake refuses `--preset` with it unset (before v3).
            continue;
        };
        let dir = PathBuf::from(vars.expand(dir));
        let binary_dir = if dir.is_absolute() { dir } else { root.join(dir) };
        let mut build_type = merged
            .get("cacheVariables")
            .and_then(|c| c.get("CMAKE_BUILD_TYPE"))
            .and_then(cache_value)
            .map(|v| vars.expand(&v));
        // The first visible build preset that builds this one.
        let mut build_preset = None;
        for b in &build {
            if b.raw.get("hidden").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            let Ok(bm) = resolve(&b.name, &builds, 0) else {
                continue;
            };
            if bm.get("configurePreset").and_then(Value::as_str) == Some(p.name.as_str()) {
                build_preset = Some(b.name.clone());
                if let Some(c) = bm.get("configuration").and_then(Value::as_str) {
                    build_type = Some(c.to_string());
                }
                break;
            }
        }
        out.push(Profile {
            name: p.name.clone(),
            display: p
                .raw
                .get("displayName")
                .and_then(Value::as_str)
                .unwrap_or(&p.name)
                .to_string(),
            binary_dir,
            configure: vec!["--preset".into(), p.name.clone()],
            build_preset,
            build_type,
            from_preset: true,
        });
    }
    Ok(out)
}

struct Preset {
    name: String,
    raw: Map<String, Value>,
    file_dir: PathBuf,
}

fn load(
    path: &Path,
    text: &str,
    read: &dyn Fn(&Path) -> Option<String>,
    configure: &mut Vec<Preset>,
    build: &mut Vec<Preset>,
    seen: &mut Vec<PathBuf>,
) -> Result<()> {
    if seen.iter().any(|s| s == path) {
        return Ok(());
    }
    seen.push(path.to_path_buf());
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    let v: Value = serde_json::from_str(text).with_context(|| format!("{file_name} isn't valid JSON"))?;
    let dir = path.parent().unwrap_or(Path::new("")).to_path_buf();
    // Included files first: their presets can be inherited from.
    for inc in v.get("include").and_then(Value::as_array).into_iter().flatten() {
        let Some(inc) = inc.as_str() else { continue };
        let p = dir.join(inc);
        let text = read(&p).with_context(|| format!("{file_name} includes {inc}, which can't be read"))?;
        load(&p, &text, read, configure, build, seen)?;
    }
    for (key, list) in [("configurePresets", &mut *configure), ("buildPresets", &mut *build)] {
        for p in v.get(key).and_then(Value::as_array).into_iter().flatten() {
            let (Some(raw), Some(name)) = (p.as_object(), p.get("name").and_then(Value::as_str))
            else {
                continue;
            };
            list.push(Preset {
                name: name.to_string(),
                raw: raw.clone(),
                file_dir: dir.clone(),
            });
        }
    }
    Ok(())
}

/// A preset with what it inherits filled in. The earlier parent wins over a
/// later one, the preset itself over both; maps merge key by key.
fn resolve(name: &str, all: &HashMap<&str, &Preset>, depth: usize) -> Result<Map<String, Value>> {
    anyhow::ensure!(depth < 32, "preset {name:?} inherits from itself");
    let p = all
        .get(name)
        .with_context(|| format!("preset {name:?} doesn't exist"))?;
    let parents: Vec<&str> = match p.raw.get("inherits") {
        Some(Value::String(s)) => vec![s.as_str()],
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
        _ => vec![],
    };
    let mut merged = Map::new();
    for parent in parents.iter().rev() {
        let r = resolve(parent, all, depth + 1)?;
        overlay(&mut merged, &r);
    }
    let own: Map<String, Value> = p
        .raw
        .iter()
        .filter(|(k, _)| !matches!(k.as_str(), "inherits" | "hidden"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    overlay(&mut merged, &own);
    Ok(merged)
}

fn overlay(into: &mut Map<String, Value>, from: &Map<String, Value>) {
    for (k, v) in from {
        match (k.as_str(), into.get_mut(k), v) {
            ("cacheVariables" | "environment", Some(Value::Object(dst)), Value::Object(src)) => {
                for (k2, v2) in src {
                    dst.insert(k2.clone(), v2.clone());
                }
            }
            _ => {
                into.insert(k.clone(), v.clone());
            }
        }
    }
}

/// A cache variable's value: `"x"`, `true`, or `{ "type": …, "value": … }`.
fn cache_value(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(if *b { "ON" } else { "OFF" }.into()),
        Value::Object(o) => o.get("value").and_then(cache_value),
        _ => None,
    }
}

fn host_system_name() -> &'static str {
    if cfg!(windows) {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "Darwin"
    } else {
        "Linux"
    }
}

struct Env<'a> {
    root: &'a Path,
    host: &'a str,
}

/// What `${…}` and `$env{…}` expand to for one preset.
struct Vars<'a> {
    env: &'a Env<'a>,
    preset: &'a str,
    generator: String,
    file_dir: &'a Path,
    preset_env: Map<String, Value>,
}

impl<'a> Vars<'a> {
    fn new(env: &'a Env<'a>, preset: &'a str, merged: &Map<String, Value>, file_dir: &'a Path) -> Self {
        Self {
            env,
            preset,
            generator: merged
                .get("generator")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            file_dir,
            preset_env: merged
                .get("environment")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
        }
    }

    fn expand(&self, s: &str) -> String {
        let mut out = String::new();
        let mut rest = s;
        while let Some(i) = rest.find('$') {
            out.push_str(&rest[..i]);
            let after = &rest[i + 1..];
            let (kind, body) = match after.find('{') {
                Some(b) if after[..b].chars().all(|c| c.is_ascii_alphabetic()) => {
                    (&after[..b], &after[b + 1..])
                }
                _ => {
                    out.push('$');
                    rest = after;
                    continue;
                }
            };
            let Some(end) = body.find('}') else {
                out.push('$');
                rest = after;
                continue;
            };
            let name = &body[..end];
            out.push_str(&self.lookup(kind, name));
            rest = &body[end + 1..];
        }
        out.push_str(rest);
        out
    }

    fn lookup(&self, kind: &str, name: &str) -> String {
        let root = self.env.root;
        match (kind, name) {
            ("", "sourceDir") => root.to_string_lossy().into_owned(),
            ("", "sourceParentDir") => root.parent().unwrap_or(root).to_string_lossy().into_owned(),
            ("", "sourceDirName") => root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            ("", "presetName") => self.preset.to_string(),
            ("", "generator") => self.generator.clone(),
            ("", "hostSystemName") => self.env.host.to_string(),
            ("", "fileDir") => self.file_dir.to_string_lossy().into_owned(),
            ("", "dollar") => "$".into(),
            ("", "pathListSep") => if cfg!(windows) { ";" } else { ":" }.into(),
            ("env", n) => match self.preset_env.get(n).and_then(Value::as_str) {
                // A preset variable may itself use macros (`$penv{PATH}`).
                Some(v) if !v.contains("$env{") => self.expand(v),
                Some(v) => v.to_string(),
                None => std::env::var(n).unwrap_or_default(),
            },
            ("penv", n) => std::env::var(n).unwrap_or_default(),
            _ => String::new(),
        }
    }
}

/// A preset's `condition`, as far as it can be decided here; what can't
/// (`matches` without regex support for its syntax) counts as true.
fn condition(c: Option<&Value>, vars: &Vars) -> bool {
    let Some(c) = c else { return true };
    let Some(o) = c.as_object() else {
        return c.as_bool().unwrap_or(true);
    };
    let s = |k: &str| o.get(k).and_then(Value::as_str).map(|v| vars.expand(v));
    match o.get("type").and_then(Value::as_str) {
        Some("const") => o.get("value").and_then(Value::as_bool).unwrap_or(true),
        Some("equals") => s("lhs") == s("rhs"),
        Some("notEquals") => s("lhs") != s("rhs"),
        Some(t @ ("inList" | "notInList")) => {
            let needle = s("string");
            let found = o
                .get("list")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .any(|x| Some(vars.expand(x)) == needle);
            found == (t == "inList")
        }
        Some(t @ ("matches" | "notMatches")) => {
            let (Some(text), Some(re)) = (s("string"), s("regex")) else {
                return true;
            };
            match regex::Regex::new(&re) {
                Ok(re) => re.is_match(&text) == (t == "matches"),
                Err(_) => true,
            }
        }
        Some("anyOf") => o
            .get("conditions")
            .and_then(Value::as_array)
            .is_none_or(|cs| cs.iter().any(|c| condition(Some(c), vars))),
        Some("allOf") => o
            .get("conditions")
            .and_then(Value::as_array)
            .is_none_or(|cs| cs.iter().all(|c| condition(Some(c), vars))),
        Some("not") => !condition(o.get("condition"), vars),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"D:\proj")
        } else {
            PathBuf::from("/proj")
        }
    }

    /// The vrg project's presets, trimmed: a hidden base, absolute and
    /// relative binary dirs, a build preset per configure preset.
    const VRG: &str = r#"{
      "version": 3,
      "configurePresets": [
        { "name": "x64-Base", "hidden": true, "generator": "Ninja",
          "cacheVariables": { "CMAKE_POLICY_DEFAULT_CMP0091": "NEW" } },
        { "name": "Debug", "inherits": "x64-Base", "displayName": "Debug",
          "binaryDir": "${sourceDir}\\out\\build\\${presetName}",
          "cacheVariables": { "CMAKE_BUILD_TYPE": "Debug" } },
        { "name": "Release", "inherits": "x64-Base",
          "binaryDir": "out/build/${presetName}",
          "cacheVariables": { "CMAKE_BUILD_TYPE": { "type": "STRING", "value": "Release" } } },
        { "name": "Mac", "inherits": "x64-Base", "binaryDir": "b",
          "condition": { "type": "equals", "lhs": "${hostSystemName}", "rhs": "Darwin" } }
      ],
      "buildPresets": [
        { "name": "Debug", "configurePreset": "Debug" },
        { "name": "Release-build", "configurePreset": "Release", "configuration": "Release" }
      ]
    }"#;

    #[::core::prelude::v1::test]
    fn presets_become_profiles() {
        let root = root();
        let files = [(root.join("CMakePresets.json"), VRG.to_string())];
        let ps = profiles(&root, &files, &|_| None).unwrap();
        let names: Vec<&str> = ps.iter().map(|p| p.name.as_str()).collect();
        if cfg!(target_os = "macos") {
            assert_eq!(names, ["Debug", "Release", "Mac"]);
        } else {
            assert_eq!(names, ["Debug", "Release"]);
        }
        let debug = &ps[0];
        assert_eq!(debug.configure, ["--preset", "Debug"]);
        assert_eq!(debug.build_preset.as_deref(), Some("Debug"));
        assert_eq!(debug.build_type.as_deref(), Some("Debug"));
        assert_eq!(
            debug.binary_dir.to_string_lossy().replace('\\', "/"),
            root.join("out/build/Debug").to_string_lossy().replace('\\', "/")
        );
        let release = &ps[1];
        assert_eq!(release.display, "Release");
        assert_eq!(release.binary_dir, root.join("out/build/Release"));
        assert_eq!(release.build_preset.as_deref(), Some("Release-build"));
        assert_eq!(
            release.build_args(Some("app"), false),
            ["--build", "--preset", "Release-build", "--target", "app"]
        );
    }

    #[::core::prelude::v1::test]
    fn inherits_merges_maps_and_the_first_parent_wins() {
        let text = r#"{ "version": 3, "configurePresets": [
            { "name": "a", "hidden": true, "binaryDir": "from-a",
              "cacheVariables": { "X": "a", "A": "1" } },
            { "name": "b", "hidden": true, "binaryDir": "from-b",
              "cacheVariables": { "X": "b", "B": "1", "CMAKE_BUILD_TYPE": "RelWithDebInfo" } },
            { "name": "c", "inherits": ["a", "b"] }
        ] }"#;
        let root = root();
        let ps = profiles(&root, &[(root.join("CMakePresets.json"), text.into())], &|_| None).unwrap();
        assert_eq!(ps.len(), 1, "hidden ones aren't profiles");
        assert_eq!(ps[0].binary_dir, root.join("from-a"));
        assert_eq!(ps[0].build_type.as_deref(), Some("RelWithDebInfo"));
    }

    #[::core::prelude::v1::test]
    fn user_presets_and_includes() {
        let root = root();
        let project = r#"{ "version": 4, "include": ["more.json"], "configurePresets": [
            { "name": "base", "hidden": true, "binaryDir": "${sourceDir}/build/${presetName}" } ] }"#;
        let more = r#"{ "version": 4, "configurePresets": [
            { "name": "inc", "inherits": "base", "displayName": "Included" } ] }"#;
        let user = r#"{ "version": 4, "configurePresets": [
            { "name": "mine", "inherits": "base",
              "environment": { "OUT": "o" }, "binaryDir": "$env{OUT}/${presetName}" } ] }"#;
        let files = [
            (root.join("CMakePresets.json"), project.to_string()),
            (root.join("CMakeUserPresets.json"), user.to_string()),
        ];
        let read = |p: &Path| (p.file_name()? == "more.json").then(|| more.to_string());
        let ps = profiles(&root, &files, &read).unwrap();
        let got: Vec<(&str, &str)> = ps.iter().map(|p| (p.name.as_str(), p.display.as_str())).collect();
        assert_eq!(got, [("inc", "Included"), ("mine", "mine")]);
        assert_eq!(ps[1].binary_dir, root.join("o/mine"));
    }

    #[::core::prelude::v1::test]
    fn bad_json_and_unknown_parents_are_errors() {
        let root = root();
        assert!(profiles(&root, &[(root.join("CMakePresets.json"), "{".into())], &|_| None).is_err());
        let text = r#"{ "configurePresets": [ { "name": "x", "inherits": "nope", "binaryDir": "b" } ] }"#;
        assert!(profiles(&root, &[(root.join("CMakePresets.json"), text.into())], &|_| None).is_err());
    }

    #[::core::prelude::v1::test]
    fn defaults_like_clion() {
        let root = root();
        let ps = default_profiles(&root, true);
        assert_eq!(ps[0].binary_dir, root.join("cmake-build-debug"));
        assert!(ps[0].configure.ends_with(&["-G".to_string(), "Ninja".to_string()]));
        assert_eq!(
            ps[0].build_args(None, true),
            [
                "--build".to_string(),
                root.join("cmake-build-debug").to_string_lossy().into_owned(),
                "--config".into(),
                "Debug".into(),
                "--clean-first".into()
            ]
        );
    }
}
