//! pi's own configuration (spec M3.5), read and changed so the user never edits
//! pi's files by hand. Everything here is blocking file I/O or a short-lived
//! `pi` process: call it off the UI thread.
//!
//! Scopes: **user** (`~/.pi/agent`, or `$PI_CODING_AGENT_DIR`) and **project**
//! (`<root>/.pi`). Writes are read-modify-write of one key and keep every key
//! pide doesn't know. Never reads `auth.json`.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Scope {
    User,
    Project,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Scope::User => "Everyone",
            Scope::Project => "This project",
        }
    }
}

/// Where pi keeps things for one project.
#[derive(Clone, Debug)]
pub struct PiConfig {
    pub agent_dir: PathBuf,
    pub root: PathBuf,
}

impl PiConfig {
    pub fn new(root: &Path) -> Self {
        let agent_dir = std::env::var_os("PI_CODING_AGENT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".pi/agent"));
        Self { agent_dir, root: root.to_path_buf() }
    }

    pub fn dir(&self, scope: Scope) -> PathBuf {
        match scope {
            Scope::User => self.agent_dir.clone(),
            Scope::Project => self.root.join(".pi"),
        }
    }

    pub fn settings_path(&self, scope: Scope) -> PathBuf {
        self.dir(scope).join("settings.json")
    }

    pub fn mcp_path(&self, scope: Scope) -> PathBuf {
        self.dir(scope).join("mcp.json")
    }

    pub fn settings(&self, scope: Scope) -> Result<Map<String, Value>> {
        read_json(&self.settings_path(scope))
    }

    /// Change one scope's `settings.json` (created if missing), keeping other keys.
    pub fn update_settings(&self, scope: Scope, f: impl FnOnce(&mut Map<String, Value>)) -> Result<()> {
        let path = self.settings_path(scope);
        let mut map = read_json(&path)?;
        f(&mut map);
        write_json(&path, &map)
    }

    pub fn update_mcp(&self, scope: Scope, f: impl FnOnce(&mut Map<String, Value>)) -> Result<()> {
        let path = self.mcp_path(scope);
        let mut map = read_json(&path)?;
        let servers = map.entry("mcpServers").or_insert_with(|| Value::Object(Map::new()));
        if let Some(servers) = servers.as_object_mut() {
            f(servers);
        }
        write_json(&path, &map)
    }
}

/// A missing file is an empty object; a broken one is an error (never overwritten).
pub fn read_json(path: &Path) -> Result<Map<String, Value>> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(Map::new()),
        Ok(text) => match serde_json::from_str::<Value>(&text).with_context(|| format!("{} is not valid JSON", path.display()))? {
            Value::Object(m) => Ok(m),
            _ => anyhow::bail!("{} is not a JSON object", path.display()),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

/// Written next to the file, then renamed over it, so a crash never leaves half a file.
pub fn write_json(path: &Path, map: &Map<String, Value>) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.pide-tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(map)? + "\n")?;
    std::fs::rename(&tmp, path).with_context(|| format!("write {}", path.display()))
}

fn string_list(v: Option<&Value>) -> Option<Vec<String>> {
    v.and_then(Value::as_array).map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
}

// ---------------------------------------------------------------------------
// Tools (`defaultTools`)

/// pi's built-in tools; the last two come from built-in extensions, registered inactive.
pub const BUILTIN_TOOLS: &[(&str, &str)] = &[
    ("read", "Read files"),
    ("bash", "Run shell commands"),
    ("edit", "Edit files by replacing text"),
    ("write", "Write whole files"),
    ("grep", "Search file contents"),
    ("find", "Find files by name"),
    ("ls", "List directories"),
    ("powershell", "Run PowerShell commands (instead of bash on Windows)"),
    ("codemode", "Let the model call tools from scripts"),
    ("tool_search", "Find tools by description instead of listing them all"),
];
pub const DEFAULT_TOOLS: &[&str] = &["read", "bash", "edit", "write"];

/// pi's rule: plain names replace the selection; `+name` / `-name` then apply in order.
pub fn apply_tool_list(mut base: Vec<String>, list: &[String]) -> Vec<String> {
    let plain: Vec<String> = list.iter().filter(|e| !e.starts_with(['+', '-'])).cloned().collect();
    if !plain.is_empty() {
        base = plain;
    }
    for e in list {
        if let Some(n) = e.strip_prefix('+') {
            if !base.iter().any(|b| b == n) {
                base.push(n.to_string());
            }
        } else if let Some(n) = e.strip_prefix('-') {
            base.retain(|b| b != n);
        }
    }
    base
}

/// The tools on at startup: defaults, then the user's list, then the project's.
pub fn resolve_tools(user: Option<&[String]>, project: Option<&[String]>) -> Vec<String> {
    let mut tools: Vec<String> = DEFAULT_TOOLS.iter().map(|s| s.to_string()).collect();
    for list in [user, project].into_iter().flatten() {
        // An explicitly empty list disables every built-in tool.
        tools = if list.is_empty() { vec![] } else { apply_tool_list(tools, list) };
    }
    tools
}

/// New `defaultTools` for `scope` so that `tool` is `on`, given what it inherits.
/// Lists of plain names are edited in place; otherwise a `+name`/`-name` entry is
/// added only when it differs from what the scope inherits.
pub fn set_tool(list: Option<&[String]>, inherited: &[String], tool: &str, on: bool) -> Option<Vec<String>> {
    let mut list: Vec<String> = list.map(<[String]>::to_vec).unwrap_or_default();
    let has_plain = list.iter().any(|e| !e.starts_with(['+', '-']));
    list.retain(|e| e.trim_start_matches(['+', '-']) != tool);
    if has_plain {
        if on {
            list.push(tool.to_string());
        }
    } else {
        let inherited_on = apply_tool_list(inherited.to_vec(), &list).iter().any(|t| t == tool);
        if inherited_on != on {
            list.push(format!("{}{tool}", if on { '+' } else { '-' }));
        }
    }
    (!list.is_empty()).then_some(list)
}

// ---------------------------------------------------------------------------
// Built-in extensions (`extensions: ["-builtin:mcp"]`)

pub const BUILTIN_EXTENSIONS: &[(&str, &str)] = &[
    ("mcp", "MCP servers (mcp.json)"),
    ("codemode", "Code mode: tools callable from scripts"),
    ("tool-search", "Tool search for large tool sets"),
    ("llama.cpp", "Local models through llama.cpp"),
];

/// On unless a scope says otherwise; the project's word wins.
pub fn builtin_extension_on(name: &str, user: Option<&[String]>, project: Option<&[String]>) -> bool {
    let mut on = true;
    for list in [user, project].into_iter().flatten() {
        for e in list {
            if e == &format!("-builtin:{name}") {
                on = false;
            } else if e == &format!("+builtin:{name}") {
                on = true;
            }
        }
    }
    on
}

/// New `extensions` list for a scope with `builtin:name` set `on` (relative to `inherited_on`).
pub fn set_builtin_extension(list: Option<&[String]>, name: &str, on: bool, inherited_on: bool) -> Option<Vec<String>> {
    let mut list: Vec<String> = list.map(<[String]>::to_vec).unwrap_or_default();
    list.retain(|e| e != &format!("-builtin:{name}") && e != &format!("+builtin:{name}"));
    if on != inherited_on {
        list.push(format!("{}builtin:{name}", if on { '+' } else { '-' }));
    }
    (!list.is_empty()).then_some(list)
}

/// Write a list setting, removing the key when it's `None`.
pub fn put_list(map: &mut Map<String, Value>, key: &str, list: Option<Vec<String>>) {
    match list {
        Some(l) => {
            map.insert(key.into(), Value::Array(l.into_iter().map(Value::String).collect()));
        }
        None => {
            map.remove(key);
        }
    }
}

pub fn list_setting(map: &Map<String, Value>, key: &str) -> Option<Vec<String>> {
    string_list(map.get(key))
}

// ---------------------------------------------------------------------------
// Packages

#[derive(Clone, Debug, PartialEq)]
pub struct Package {
    pub scope: Scope,
    /// As written in settings (`npm:pi-search-hub`, `git:github.com/a/b@v1`, `./local`).
    pub source: String,
    pub name: String,
    pub kind: &'static str,
    pub installed_at: Option<PathBuf>,
    pub version: Option<String>,
    pub description: Option<String>,
    /// What it contributes, by type (relative paths), as declared or discovered.
    pub resources: Vec<(&'static str, String)>,
}

/// `npm:@scope/pkg@1.2` -> (`npm`, `@scope/pkg`); `git:github.com/a/b@v1` -> (`git`, `github.com/a/b`).
pub fn package_identity(source: &str) -> (&'static str, String) {
    if let Some(rest) = source.strip_prefix("npm:") {
        let name = match rest.strip_prefix('@') {
            Some(scoped) => format!("@{}", scoped.split_once('@').map(|(n, _)| n).unwrap_or(scoped)),
            None => rest.split_once('@').map(|(n, _)| n).unwrap_or(rest).to_string(),
        };
        return ("npm", name);
    }
    let git = source.strip_prefix("git:").or_else(|| source.strip_prefix("https://"));
    if let Some(rest) = git {
        let rest = rest.trim_end_matches(".git");
        let name = rest.rsplit_once('@').filter(|(_, r)| !r.contains('/')).map(|(n, _)| n).unwrap_or(rest);
        return ("git", name.to_string());
    }
    ("local", source.to_string())
}

pub fn packages(cfg: &PiConfig, scope: Scope, settings: &Map<String, Value>) -> Vec<Package> {
    let Some(list) = settings.get("packages").and_then(Value::as_array) else { return vec![] };
    list.iter()
        .filter_map(|p| match p {
            Value::String(s) => Some(s.clone()),
            Value::Object(o) => o.get("source").and_then(Value::as_str).map(str::to_string),
            _ => None,
        })
        .map(|source| {
            let (kind, name) = package_identity(&source);
            let dir = cfg.dir(scope);
            let installed_at = match kind {
                "npm" => Some(dir.join("npm/node_modules").join(&name)),
                "local" => Some(if Path::new(&name).is_absolute() { PathBuf::from(&name) } else { dir.join(&name) }),
                _ => None,
            }
            .filter(|p| p.exists());
            let manifest = installed_at.as_ref().and_then(|p| read_json(&p.join("package.json")).ok()).unwrap_or_default();
            let resources = installed_at.as_ref().map(|p| package_resources(p, &manifest)).unwrap_or_default();
            Package {
                scope,
                version: manifest.get("version").and_then(Value::as_str).map(str::to_string),
                description: manifest.get("description").and_then(Value::as_str).map(str::to_string),
                source,
                name,
                kind,
                installed_at,
                resources,
            }
        })
        .collect()
}

/// Resources from the `pi` manifest, or pi's conventional folders.
fn package_resources(dir: &Path, manifest: &Map<String, Value>) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for kind in ["extensions", "skills", "prompts", "themes"] {
        match manifest.get("pi").and_then(|p| p.get(kind)).and_then(Value::as_array) {
            Some(list) => out.extend(list.iter().filter_map(Value::as_str).map(|s| (kind, s.trim_start_matches("./").to_string()))),
            None => {
                if let Ok(entries) = std::fs::read_dir(dir.join(kind)) {
                    let mut names: Vec<String> =
                        entries.flatten().map(|e| format!("{kind}/{}", e.file_name().to_string_lossy())).collect();
                    names.sort();
                    out.extend(names.into_iter().map(|n| (kind, n)));
                }
            }
        }
    }
    out
}

/// Extension files you put in `<scope dir>/extensions/` yourself.
pub fn local_extensions(cfg: &PiConfig, scope: Scope) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(cfg.dir(scope).join("extensions")) else { return vec![] };
    let mut v: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() || matches!(p.extension().and_then(|e| e.to_str()), Some("ts" | "js" | "mjs")))
        .collect();
    v.sort();
    v
}

// ---------------------------------------------------------------------------
// MCP servers

#[derive(Clone, Debug, PartialEq)]
pub struct McpServer {
    pub scope: Scope,
    pub name: String,
    /// `command args…` or the URL.
    pub target: String,
    pub http: bool,
    pub enabled: bool,
    pub description: Option<String>,
}

pub fn mcp_servers(scope: Scope, file: &Map<String, Value>) -> Vec<McpServer> {
    let Some(servers) = file.get("mcpServers").and_then(Value::as_object) else { return vec![] };
    servers
        .iter()
        .map(|(name, v)| {
            let url = v.get("url").and_then(Value::as_str);
            let target = match url {
                Some(u) => u.to_string(),
                None => {
                    let mut parts = vec![v.get("command").and_then(Value::as_str).unwrap_or("").to_string()];
                    parts.extend(string_list(v.get("args")).unwrap_or_default());
                    parts.join(" ").trim().to_string()
                }
            };
            McpServer {
                scope,
                name: name.clone(),
                http: url.is_some(),
                target,
                enabled: v.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                description: v.get("description").and_then(Value::as_str).map(str::to_string),
            }
        })
        .collect()
}

/// A new server entry from what the user typed: a URL, or a command line
/// (split on spaces, with "double quotes" keeping spaces together).
pub fn mcp_entry(target: &str) -> Value {
    let t = target.trim();
    if t.starts_with("http://") || t.starts_with("https://") {
        return serde_json::json!({ "url": t });
    }
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in t.chars() {
        match c {
            '"' => quoted = !quoted,
            ' ' if !quoted => {
                if !cur.is_empty() {
                    words.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    let command = if words.is_empty() { String::new() } else { words.remove(0) };
    serde_json::json!({ "command": command, "args": words })
}

/// Server names pi accepts.
pub fn valid_mcp_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

// ---------------------------------------------------------------------------
// Models

#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    pub provider: String,
    pub id: String,
    pub context: String,
    pub thinking: bool,
    /// Why pi can't use it right now (a custom provider whose key isn't set).
    pub note: Option<String>,
}

/// Parse `pi --list-models` (a whitespace table under a `provider model …` header).
pub fn parse_models(out: &str) -> Vec<Model> {
    let mut lines = out.lines().skip_while(|l| !l.trim_start().starts_with("provider"));
    if lines.next().is_none() {
        return vec![];
    }
    lines
        .filter_map(|l| {
            let cols: Vec<&str> = l.split_whitespace().collect();
            (cols.len() >= 5).then(|| Model {
                provider: cols[0].into(),
                id: cols[1].into(),
                context: cols[2].into(),
                thinking: cols[4] == "yes",
                note: None,
            })
        })
        .collect()
}

/// Models from custom providers in `models.json`. pi lists only models whose key
/// it can find, so these fill the gap with a note on what's missing.
pub fn custom_models(models_json: &Map<String, Value>, env: impl Fn(&str) -> bool) -> Vec<Model> {
    let Some(providers) = models_json.get("providers").and_then(Value::as_object) else { return vec![] };
    let mut out = Vec::new();
    for (provider, p) in providers {
        let key = p.get("apiKey").and_then(Value::as_str).unwrap_or("");
        let note = key.strip_prefix('$').filter(|var| !env(var)).map(|var| format!("needs ${var}"));
        for m in p.get("models").and_then(Value::as_array).into_iter().flatten() {
            let Some(id) = m.get("id").and_then(Value::as_str) else { continue };
            let context = m.get("contextWindow").and_then(Value::as_u64).map(|c| format!("{}K", c / 1000)).unwrap_or_default();
            out.push(Model {
                provider: provider.clone(),
                id: id.to_string(),
                context,
                thinking: m.get("reasoning").and_then(Value::as_bool).unwrap_or(false),
                note: note.clone(),
            });
        }
    }
    out
}

pub const THINKING_LEVELS: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// The `pi` binary (PATH, then the usual npm-global places).
pub fn find_pi() -> Option<PathBuf> {
    pide_runtime::process::find_command("pi")
}

/// Blocking (a few seconds): every model pi can use now.
pub fn list_models(cwd: &Path) -> Result<Vec<Model>> {
    let pi = find_pi().context("pi isn't installed")?;
    let out = std::process::Command::new(pi)
        .arg("--list-models")
        .current_dir(cwd)
        .env("PATH", std::env::join_paths(pide_runtime::process::search_path()).unwrap_or_default())
        .output()?;
    let mut models = parse_models(&String::from_utf8_lossy(&out.stdout));
    // Custom providers pi left out (no key in this environment).
    let custom = read_json(&PiConfig::new(cwd).agent_dir.join("models.json")).unwrap_or_default();
    for m in custom_models(&custom, |var| std::env::var_os(var).is_some()) {
        if !models.iter().any(|x| x.provider == m.provider && x.id == m.id) {
            models.insert(0, m);
        }
    }
    Ok(models)
}

// ---------------------------------------------------------------------------
// Skills, prompts and extension commands (from a throwaway `pi --mode rpc --no-session`)

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct PiCommand {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// `extension`, `prompt` or `skill`.
    pub source: String,
    #[serde(rename = "sourceInfo")]
    pub source_info: SourceInfo,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct SourceInfo {
    pub path: PathBuf,
    /// How pi found it: `local`, `auto`, `cli`, or the package source (`npm:pi-search-hub`).
    pub source: String,
    /// `user`, `project` or `temporary`.
    pub scope: String,
    /// `top-level` or `package`.
    pub origin: String,
    #[serde(rename = "baseDir", default)]
    pub base_dir: Option<PathBuf>,
}

pub fn parse_get_commands(line: &str) -> Result<Vec<PiCommand>> {
    let v: Value = serde_json::from_str(line)?;
    anyhow::ensure!(v["success"].as_bool() == Some(true), "get_commands failed: {}", v["error"]);
    Ok(serde_json::from_value(v["data"]["commands"].clone())?)
}

/// Blocking (pi's startup, a few seconds): the commands pi would load in `cwd`.
pub fn list_commands(cwd: &Path) -> Result<Vec<PiCommand>> {
    use std::io::{BufRead, Write};
    let pi = find_pi().context("pi isn't installed")?;
    let mut child = std::process::Command::new(pi)
        .args(["--mode", "rpc", "--no-session"])
        .current_dir(cwd)
        .env("PATH", std::env::join_paths(pide_runtime::process::search_path()).unwrap_or_default())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let mut stdin = child.stdin.take().context("stdin")?;
    writeln!(stdin, r#"{{"type":"get_commands","id":"pide-config"}}"#)?;
    stdin.flush()?;
    let stdout = std::io::BufReader::new(child.stdout.take().context("stdout")?);
    let mut result = Err(anyhow::anyhow!("pi exited without answering"));
    for line in stdout.split(b'\n').map_while(|l| l.ok()) {
        let line = String::from_utf8_lossy(&line);
        if line.contains("\"pide-config\"") {
            result = parse_get_commands(&line);
            break;
        }
    }
    drop(stdin); // closing stdin asks pi to exit
    let _ = child.kill();
    let _ = child.wait();
    result
}

/// Turn a skill or prompt off (or back on) at a scope: an exact `-path` entry in
/// that scope's `skills` / `prompts` list.
pub fn set_resource(list: Option<&[String]>, path: &Path, on: bool) -> Option<Vec<String>> {
    let p = path.display().to_string();
    let mut list: Vec<String> = list.map(<[String]>::to_vec).unwrap_or_default();
    list.retain(|e| e != &format!("-{p}") && e != &format!("+{p}"));
    if !on {
        list.push(format!("-{p}"));
    }
    (!list.is_empty()).then_some(list)
}

/// Resources a scope has switched off (`-path` entries), for listing them.
pub fn disabled_resources(list: Option<&[String]>) -> Vec<PathBuf> {
    list.unwrap_or_default().iter().filter_map(|e| e.strip_prefix('-')).map(PathBuf::from).collect()
}

// ---------------------------------------------------------------------------
// Instructions

#[derive(Clone, Debug, PartialEq)]
pub struct InstructionFile {
    pub scope: Scope,
    pub path: PathBuf,
    pub exists: bool,
    /// What it does, in a few words.
    pub role: &'static str,
}

pub fn instruction_files(cfg: &PiConfig) -> Vec<InstructionFile> {
    let entries: [(Scope, PathBuf, &str); 6] = [
        (Scope::User, cfg.agent_dir.join("AGENTS.md"), "Your instructions for every project"),
        (Scope::User, cfg.agent_dir.join("APPEND_SYSTEM.md"), "Added to pi's system prompt everywhere"),
        (Scope::User, cfg.agent_dir.join("SYSTEM.md"), "Replaces pi's system prompt everywhere"),
        (Scope::Project, cfg.root.join("AGENTS.md"), "Instructions for this project (also read by other agents)"),
        (Scope::Project, cfg.root.join(".pi/APPEND_SYSTEM.md"), "Added to the system prompt in this project"),
        (Scope::Project, cfg.root.join(".pi/SYSTEM.md"), "Replaces the system prompt in this project"),
    ];
    entries.into_iter().map(|(scope, path, role)| InstructionFile { scope, exists: path.is_file(), path, role }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn tools_follow_pis_rules() {
        assert_eq!(resolve_tools(None, None), v(&["read", "bash", "edit", "write"]));
        assert_eq!(resolve_tools(Some(&v(&["+grep"])), None), v(&["read", "bash", "edit", "write", "grep"]));
        assert_eq!(resolve_tools(Some(&v(&["-bash", "+powershell", "+grep"])), None), v(&["read", "edit", "write", "powershell", "grep"]));
        assert_eq!(resolve_tools(Some(&v(&["read", "ls"])), Some(&v(&["+grep"]))), v(&["read", "ls", "grep"]));
        assert_eq!(resolve_tools(Some(&v(&[])), None), Vec::<String>::new());
    }

    #[test]
    fn toggling_a_tool_writes_the_smallest_change() {
        let inherited = resolve_tools(None, None);
        assert_eq!(set_tool(None, &inherited, "grep", true), Some(v(&["+grep"])));
        assert_eq!(set_tool(Some(&v(&["+grep"])), &inherited, "grep", false), None);
        assert_eq!(set_tool(None, &inherited, "bash", false), Some(v(&["-bash"])));
        // A plain list is edited in place.
        assert_eq!(set_tool(Some(&v(&["read", "ls"])), &inherited, "grep", true), Some(v(&["read", "ls", "grep"])));
        assert_eq!(set_tool(Some(&v(&["read", "ls"])), &inherited, "ls", false), Some(v(&["read"])));
    }

    #[test]
    fn builtin_extensions_project_wins() {
        assert!(builtin_extension_on("mcp", None, None));
        assert!(!builtin_extension_on("mcp", Some(&v(&["-builtin:mcp"])), None));
        assert!(builtin_extension_on("mcp", Some(&v(&["-builtin:mcp"])), Some(&v(&["+builtin:mcp"]))));
        assert_eq!(set_builtin_extension(Some(&v(&["./x.ts"])), "mcp", false, true), Some(v(&["./x.ts", "-builtin:mcp"])));
        assert_eq!(set_builtin_extension(Some(&v(&["-builtin:mcp"])), "mcp", true, true), None);
    }

    #[test]
    fn package_names() {
        assert_eq!(package_identity("npm:pi-search-hub"), ("npm", "pi-search-hub".into()));
        assert_eq!(package_identity("npm:@scope/tools@1.2.3"), ("npm", "@scope/tools".into()));
        assert_eq!(package_identity("git:github.com/a/b@v1"), ("git", "github.com/a/b".into()));
        assert_eq!(package_identity("https://github.com/a/b.git"), ("git", "github.com/a/b".into()));
        assert_eq!(package_identity("./pkg"), ("local", "./pkg".into()));
    }

    #[test]
    fn mcp_parse_and_new_entries() {
        let file = json!({ "mcpServers": {
            "fs": { "command": "npx", "args": ["-y", "server-fs", "."] },
            "docs": { "url": "https://example.com/mcp", "enabled": false, "description": "Docs" }
        }});
        let servers = mcp_servers(Scope::User, file.as_object().unwrap());
        let fs = servers.iter().find(|s| s.name == "fs").unwrap();
        assert_eq!((fs.target.as_str(), fs.http, fs.enabled), ("npx -y server-fs .", false, true));
        let docs = servers.iter().find(|s| s.name == "docs").unwrap();
        assert_eq!((docs.http, docs.enabled), (true, false));
        assert_eq!(mcp_entry("https://x.dev/mcp"), json!({ "url": "https://x.dev/mcp" }));
        assert_eq!(mcp_entry(r#"npx -y "my server" ."#), json!({ "command": "npx", "args": ["-y", "my server", "."] }));
        assert!(valid_mcp_name("my-server_2") && !valid_mcp_name("my server"));
    }

    #[test]
    fn models_table() {
        let out = "Warning: something\nprovider    model           context  max-out  thinking  images\n\
                   openrouter  ~anthropic/x    1M       128K     yes       yes\n\
                   e-infra     glm-5.2         200K     32K      no        no\n";
        let m = parse_models(out);
        assert_eq!(m.len(), 2);
        assert_eq!((m[1].provider.as_str(), m[1].id.as_str(), m[1].thinking), ("e-infra", "glm-5.2", false));
    }

    #[test]
    fn custom_provider_models_note_a_missing_key() {
        let file = json!({ "providers": {
            "e-infra": { "apiKey": "$EINFRA_API_KEY", "models": [{ "id": "glm-5.2", "contextWindow": 200000, "reasoning": true }] },
            "local": { "apiKey": "literal", "models": [{ "id": "m" }] }
        }});
        let m = custom_models(file.as_object().unwrap(), |_| false);
        let glm = m.iter().find(|m| m.id == "glm-5.2").unwrap();
        assert_eq!((glm.context.as_str(), glm.thinking, glm.note.as_deref()), ("200K", true, Some("needs $EINFRA_API_KEY")));
        assert_eq!(m.iter().find(|m| m.id == "m").unwrap().note, None);
        assert!(custom_models(file.as_object().unwrap(), |_| true).iter().all(|m| m.note.is_none()));
    }

    #[test]
    fn recorded_get_commands_response_parses() {
        let line = include_str!("../tests/fixtures/get_commands_response.json");
        let cmds = parse_get_commands(line.trim()).unwrap();
        let search = cmds.iter().find(|c| c.name == "search-setup").unwrap();
        assert_eq!((search.source.as_str(), search.source_info.origin.as_str()), ("extension", "package"));
        assert_eq!(search.source_info.source, "npm:pi-search-hub");
    }

    #[test]
    fn settings_round_trip_keeps_unknown_keys() {
        let dir = std::env::temp_dir().join(format!("pide-picfg-{}", std::process::id()));
        let cfg = PiConfig { agent_dir: dir.join("agent"), root: dir.join("proj") };
        std::fs::create_dir_all(&cfg.agent_dir).unwrap();
        std::fs::write(cfg.settings_path(Scope::User), r#"{ "theme": "dark", "lastChangelogVersion": "1.0" }"#).unwrap();
        cfg.update_settings(Scope::User, |m| put_list(m, "defaultTools", Some(v(&["+grep"])))).unwrap();
        let back = cfg.settings(Scope::User).unwrap();
        assert_eq!(back["theme"], "dark");
        assert_eq!(back["lastChangelogVersion"], "1.0");
        assert_eq!(back["defaultTools"], json!(["+grep"]));
        // Project scope: created on first write.
        cfg.update_mcp(Scope::Project, |s| {
            s.insert("x".into(), mcp_entry("https://x.dev/mcp"));
        })
        .unwrap();
        assert!(cfg.mcp_path(Scope::Project).is_file());
        // A broken file is reported, never overwritten.
        std::fs::write(cfg.settings_path(Scope::User), "{ nope").unwrap();
        assert!(cfg.update_settings(Scope::User, |_| {}).is_err());
        assert_eq!(std::fs::read_to_string(cfg.settings_path(Scope::User)).unwrap(), "{ nope");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Against the installed pi (read-only). `cargo test -p pide-agent-pi -- --ignored live`
    #[test]
    #[ignore]
    fn live_models_and_commands() {
        let cwd = std::env::temp_dir();
        let models = list_models(&cwd).unwrap();
        assert!(!models.is_empty());
        let cmds = list_commands(&cwd).unwrap();
        eprintln!("{} models, {} commands", models.len(), cmds.len());
        let cfg = PiConfig::new(&cwd);
        let user = cfg.settings(Scope::User).unwrap();
        eprintln!("packages: {:?}", packages(&cfg, Scope::User, &user).iter().map(|p| (&p.name, &p.version, p.resources.len())).collect::<Vec<_>>());
    }
}
