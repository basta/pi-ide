//! A script panel's folder and its `panel.json`.
//!
//! ```text
//! .pide/panels/problems/          (or ~/.config/pide/panels/problems/)
//!   panel.json                    title, dock, icon, permissions
//!   main.js                       `export default class … extends View`
//! ```
//!
//! Only pide reads `panel.json`; gpui-shell gets the capabilities pide grants.

use std::path::{Path, PathBuf};

use serde::Deserialize;

pub const MANIFEST: &str = "panel.json";
pub const ENTRY: &str = "main.js";

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Manifest {
    /// Shown on the tab and the stripe button. Defaults to the folder name.
    pub title: Option<String>,
    /// `left`, `right` or `bottom` (default `right`).
    pub dock: Option<String>,
    /// A GPUI Kit icon name in kebab case (`triangle-alert`); default `layout-dashboard`.
    pub icon: Option<String>,
    /// A default key for the toggle command, e.g. `cmd-alt-6`.
    pub key: Option<String>,
    pub permissions: Permissions,
}

/// What the panel asks for beyond reading the project, which every panel may do.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Permissions {
    /// Write files in the project.
    pub write: bool,
    /// Commands it may run (program names, e.g. `cargo`), or `["*"]` for any.
    pub run: Vec<String>,
    /// Hosts it may reach over the network.
    pub network: Vec<String>,
}

impl Permissions {
    /// One line per thing granted, for the consent card.
    pub fn describe(&self) -> Vec<(bool, String)> {
        let mut lines = vec![(false, "Read files in this project".to_string())];
        if self.write {
            lines.push((true, "Write files in this project".into()));
        }
        if self.run.iter().any(|c| c == "*") {
            lines.push((true, "Run any command".into()));
        } else if !self.run.is_empty() {
            lines.push((true, format!("Run: {}", self.run.join(", "))));
        }
        if !self.network.is_empty() {
            lines.push((true, format!("Reach: {}", self.network.join(", "))));
        }
        lines
    }
}

/// One panel found on disk.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    /// The folder name; with the scope it makes the tool window id.
    pub name: String,
    pub dir: PathBuf,
    /// Project panels come from `.pide/panels` in that project, yours from the config dir.
    pub project: Option<PathBuf>,
    /// Shipped with pide (crates/panels/scripts/builtin): trusted, no consent card.
    pub builtin: bool,
    pub manifest: Manifest,
    /// Why `panel.json` couldn't be read, if it couldn't.
    pub error: Option<String>,
}

impl Found {
    pub fn title(&self) -> String {
        self.manifest
            .title
            .clone()
            .unwrap_or_else(|| self.name.clone())
    }

    /// Stable id: the tool window's `panel_name`, so saved layouts find it again.
    pub fn id(&self) -> String {
        format!("script:{}/{}", self.scope(), self.name)
    }

    /// `builtin`, `project` or `user`.
    pub fn scope(&self) -> &'static str {
        match (&self.project, self.builtin) {
            (_, true) => "builtin",
            (Some(_), _) => "project",
            (None, _) => "user",
        }
    }
}

/// Panels under `dir` (each subfolder with a `main.js`), sorted by name.
pub fn scan(dir: &Path, project: Option<&Path>) -> Vec<Found> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut found: Vec<Found> = entries
        .flatten()
        .filter(|e| e.path().join(ENTRY).is_file())
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            if !valid_name(&name) {
                log::warn!("script panel folder {name:?} skipped: use a-z, 0-9, - and _");
                return None;
            }
            let dir = e.path();
            let (manifest, error) = match std::fs::read_to_string(dir.join(MANIFEST)) {
                Ok(text) => match serde_json::from_str(&text) {
                    Ok(m) => (m, None),
                    Err(err) => (Manifest::default(), Some(format!("{MANIFEST}: {err}"))),
                },
                Err(_) => (Manifest::default(), None),
            };
            Some(Found {
                name,
                dir,
                project: project.map(Path::to_path_buf),
                builtin: false,
                manifest,
                error,
            })
        })
        .collect();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_panels_and_their_manifests() {
        let root = std::env::temp_dir().join(format!("pide-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (name, manifest) in [
            (
                "problems",
                Some(r#"{"title":"Problems","dock":"bottom","permissions":{"run":["cargo"]}}"#),
            ),
            ("bare", None),
            ("broken", Some("{nope")),
        ] {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(ENTRY), "").unwrap();
            if let Some(m) = manifest {
                std::fs::write(dir.join(MANIFEST), m).unwrap();
            }
        }
        std::fs::create_dir_all(root.join("no-entry")).unwrap();
        std::fs::create_dir_all(root.join("Bad Name")).unwrap();
        std::fs::write(root.join("Bad Name").join(ENTRY), "").unwrap();

        let found = scan(&root, None);
        let names: Vec<_> = found.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["bare", "broken", "problems"]);
        assert_eq!(found[0].title(), "bare");
        assert!(found[1].error.is_some());
        assert_eq!(found[2].manifest.permissions.run, ["cargo"]);
        assert_eq!(found[2].id(), "script:user/problems");
        assert_eq!(
            found[2].manifest.permissions.describe()[1],
            (true, "Run: cargo".to_string())
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
