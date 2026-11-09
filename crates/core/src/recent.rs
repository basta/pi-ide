//! Recent projects (spec M2.5): `$XDG_STATE_HOME/pide/recent.json`, newest first.
//!
//! Small (at most `MAX` entries). Read once at startup; writes go through
//! `save`, which the app runs on the background executor.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// How many projects are remembered.
pub const MAX: usize = 20;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecentProject {
    pub path: PathBuf,
    /// Seconds since the Unix epoch.
    pub opened: u64,
}

impl RecentProject {
    /// Folder name, for lists.
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Recent {
    pub projects: Vec<RecentProject>,
}

pub fn file() -> PathBuf {
    crate::paths::state_dir().join("recent.json")
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Recent {
    /// Missing or unreadable file = no recent projects.
    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// `path` was just opened: move it to the front.
    pub fn touch(&mut self, path: &Path, at: u64) {
        self.projects.retain(|p| p.path != path);
        self.projects.insert(
            0,
            RecentProject {
                path: path.to_path_buf(),
                opened: at,
            },
        );
        self.projects.truncate(MAX);
    }

    pub fn remove(&mut self, path: &Path) {
        self.projects.retain(|p| p.path != path);
    }

    /// The most recent project whose folder still exists.
    pub fn last_existing(&self) -> Option<&Path> {
        self.projects
            .iter()
            .map(|p| p.path.as_path())
            .find(|p| p.is_dir())
    }
}

/// "just now", "5 min ago", "3 h ago", "2 days ago".
pub fn ago(then: u64, now: u64) -> String {
    let s = now.saturating_sub(then);
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", s / 60),
        3600..86400 => format!("{} h ago", s / 3600),
        86400..172800 => "yesterday".into(),
        _ => format!("{} days ago", s / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn touch_moves_to_front_without_duplicates_and_caps() {
        let mut r = Recent::default();
        r.touch(Path::new("/a"), 1);
        r.touch(Path::new("/b"), 2);
        r.touch(Path::new("/a"), 3);
        assert_eq!(
            r.projects
                .iter()
                .map(|p| p.path.to_str().unwrap())
                .collect::<Vec<_>>(),
            ["/a", "/b"]
        );
        assert_eq!(r.projects[0].opened, 3);
        for i in 0..30 {
            r.touch(Path::new(&format!("/p{i}")), 10 + i);
        }
        assert_eq!(r.projects.len(), MAX);
        assert_eq!(r.projects[0].path, Path::new("/p29"));
        r.remove(Path::new("/p29"));
        assert_eq!(r.projects[0].path, Path::new("/p28"));
    }

    #[::core::prelude::v1::test]
    fn round_trips_and_tolerates_a_missing_file() {
        let dir = std::env::temp_dir().join(format!("pide-recent-{}", std::process::id()));
        let file = dir.join("recent.json");
        assert_eq!(Recent::load_from(&file), Recent::default());
        let mut r = Recent::default();
        r.touch(&dir, 42);
        r.save_to(&file).unwrap();
        let back = Recent::load_from(&file);
        assert_eq!(back, r);
        assert_eq!(back.last_existing(), Some(dir.as_path()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[::core::prelude::v1::test]
    fn ago_labels() {
        assert_eq!(ago(100, 100), "just now");
        assert_eq!(ago(0, 300), "5 min ago");
        assert_eq!(ago(0, 7200), "2 h ago");
        assert_eq!(ago(0, 90000), "yesterday");
        assert_eq!(ago(0, 86400 * 5), "5 days ago");
    }
}
