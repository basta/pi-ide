//! Debounced filesystem watching, delivered to the UI as batches of paths.

use std::path::{Path, PathBuf};
use std::time::Duration;

use notify_debouncer_full::notify::{EventKind, RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, Debouncer, RecommendedCache, new_debouncer};
use tokio::sync::mpsc;

pub type Watcher = Debouncer<RecommendedWatcher, RecommendedCache>;

/// One debounced batch: every path that was created, modified or removed.
#[derive(Debug, Default, Clone)]
pub struct Changes {
    pub paths: Vec<PathBuf>,
    pub structure_changed: bool,
}

/// Watch `path`. Keep the returned watcher alive; read batches from the receiver
/// (works from a GPUI `cx.spawn` task). 100 ms debounce keeps reloads well under 500 ms.
pub fn watch(path: &Path, recursive: bool) -> anyhow::Result<(Watcher, mpsc::UnboundedReceiver<Changes>)> {
    let (tx, rx) = mpsc::unbounded_channel();
    let mut debouncer = new_debouncer(
        Duration::from_millis(100),
        None,
        move |result: DebounceEventResult| match result {
            Ok(events) => {
                let mut changes = Changes::default();
                for e in events {
                    if matches!(e.kind, EventKind::Access(_)) {
                        continue;
                    }
                    if matches!(e.kind, EventKind::Create(_) | EventKind::Remove(_))
                        || matches!(e.kind, EventKind::Modify(notify_debouncer_full::notify::event::ModifyKind::Name(_)))
                    {
                        changes.structure_changed = true;
                    }
                    for p in &e.paths {
                        if !changes.paths.contains(p) {
                            changes.paths.push(p.clone());
                        }
                    }
                }
                if !changes.paths.is_empty() {
                    let _ = tx.send(changes);
                }
            }
            Err(errors) => {
                for e in errors {
                    log::warn!("file watcher: {e}");
                }
            }
        },
    )?;
    let mode = if recursive { RecursiveMode::Recursive } else { RecursiveMode::NonRecursive };
    debouncer.watch(path, mode)?;
    Ok((debouncer, rx))
}
