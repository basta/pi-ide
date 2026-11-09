//! The workspace: project root, open buffers, the file watcher.
//!
//! It is the source of truth for file contents. Editors display buffers and
//! push their edits here with [`Workspace::edited`]; the workspace saves,
//! reloads and reports conflicts.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use gpui_kit::component::input::Rope;
use gpui_kit::*;

use crate::buffer::{Buffer, hash_bytes};
use crate::events::WorkspaceEvent;
use crate::fswatch;

pub struct Workspace {
    root: PathBuf,
    buffers: Vec<Entity<Buffer>>,
    active: Option<Entity<Buffer>>,
    _watcher: Option<fswatch::Watcher>,
    _watch_task: Option<Task<()>>,
}

impl EventEmitter<WorkspaceEvent> for Workspace {}

/// Canonical form used as the buffer key (FSEvents reports canonical paths,
/// e.g. `/private/tmp` for `/tmp`).
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

impl Workspace {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let root = canonical(&root);
        let (watcher, task) = match fswatch::watch(&root, true) {
            Ok((watcher, mut rx)) => {
                let task = cx.spawn(async move |this: WeakEntity<Self>, cx| {
                    while let Some(changes) = rx.recv().await {
                        if this
                            .update(cx, |this, cx| this.on_disk_changes(changes, cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                });
                (Some(watcher), Some(task))
            }
            Err(e) => {
                log::error!("can't watch {}: {e:#}", root.display());
                (None, None)
            }
        };
        Self {
            root,
            buffers: Vec::new(),
            active: None,
            _watcher: watcher,
            _watch_task: task,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn buffers(&self) -> &[Entity<Buffer>] {
        &self.buffers
    }

    pub fn buffer_for(&self, path: &Path, cx: &App) -> Option<Entity<Buffer>> {
        let path = canonical(path);
        self.buffers
            .iter()
            .find(|b| b.read(cx).path == path)
            .cloned()
    }

    pub fn active(&self) -> Option<&Entity<Buffer>> {
        self.active.as_ref()
    }

    pub fn set_active(&mut self, buffer: Option<Entity<Buffer>>, cx: &mut Context<Self>) {
        if self.active.as_ref().map(|b| b.entity_id()) != buffer.as_ref().map(|b| b.entity_id()) {
            self.active = buffer;
            cx.emit(WorkspaceEvent::ActiveBufferChanged);
        }
    }

    /// Open a file (or return the already-open buffer). Reads off the UI thread.
    pub fn open(
        &mut self,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<Entity<Buffer>>> {
        if let Some(b) = self.buffer_for(&path, cx) {
            return Task::ready(Ok(b));
        }
        let read = cx.background_executor().spawn(async move {
            let path = canonical(&path);
            let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
            anyhow::Ok((path, bytes))
        });
        cx.spawn(async move |this, cx| {
            let (path, bytes) = read.await?;
            this.update(cx, |this, cx| {
                // Opened concurrently by someone else meanwhile?
                if let Some(b) = this.buffer_for(&path, cx) {
                    return b;
                }
                let buffer = cx.new(|_| Buffer::from_bytes(path, &bytes));
                this.buffers.push(buffer.clone());
                cx.emit(WorkspaceEvent::BufferOpened(buffer.clone()));
                buffer
            })
        })
    }

    pub fn close(&mut self, buffer: &Entity<Buffer>, cx: &mut Context<Self>) {
        let path = buffer.read(cx).path.clone();
        self.buffers.retain(|b| b != buffer);
        if self.active.as_ref() == Some(buffer) {
            self.active = None;
            cx.emit(WorkspaceEvent::ActiveBufferChanged);
        }
        cx.emit(WorkspaceEvent::BufferClosed(path));
    }

    /// An editor changed the text.
    pub fn edited(&mut self, buffer: &Entity<Buffer>, text: Rope, cx: &mut Context<Self>) {
        buffer.update(cx, |b, cx| {
            b.text = text;
            b.dirty = true;
            cx.notify();
        });
        cx.emit(WorkspaceEvent::BufferChanged(buffer.clone()));
    }

    /// Replace the text from outside the editor; open editors follow (undoably).
    pub fn replace_text(&mut self, buffer: &Entity<Buffer>, text: Rope, cx: &mut Context<Self>) {
        buffer.update(cx, |b, cx| {
            b.text = text;
            b.dirty = true;
            cx.notify();
        });
        cx.emit(WorkspaceEvent::TextReplaced(buffer.clone()));
        cx.emit(WorkspaceEvent::BufferChanged(buffer.clone()));
    }

    /// Save to disk. Read-only buffers are refused.
    pub fn save(
        &mut self,
        buffer: &Entity<Buffer>,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<()>> {
        let b = buffer.read(cx);
        if let Some(reason) = &b.read_only {
            return Task::ready(Err(anyhow::anyhow!("{reason}")));
        }
        let path = b.path.clone();
        let bytes = b.encoded();
        let hash = hash_bytes(&bytes);
        let buffer = buffer.clone();
        let write = cx.background_executor().spawn(async move {
            std::fs::write(&path, &bytes).with_context(|| format!("write {}", path.display()))
        });
        cx.spawn(async move |this, cx| {
            write.await?;
            this.update(cx, |_, cx| {
                buffer.update(cx, |b, cx| {
                    b.disk_hash = hash;
                    b.dirty = false;
                    b.conflict = false;
                    b.notice = None;
                    cx.notify();
                });
                cx.emit(WorkspaceEvent::BufferSaved(buffer));
            })
        })
    }

    /// Replace the buffer with what's on disk, dropping unsaved edits (conflict → Reload).
    pub fn reload(
        &mut self,
        buffer: &Entity<Buffer>,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<()>> {
        let path = buffer.read(cx).path.clone();
        let buffer = buffer.clone();
        let read = cx
            .background_executor()
            .spawn(async move { std::fs::read(&path) });
        cx.spawn(async move |this, cx| {
            let bytes = read.await?;
            this.update(cx, |this, cx| this.apply_disk_bytes(&buffer, &bytes, cx))
        })
    }

    /// Conflict → Keep mine: the next save overwrites the disk version.
    pub fn keep_mine(&mut self, buffer: &Entity<Buffer>, cx: &mut Context<Self>) {
        buffer.update(cx, |b, cx| {
            b.conflict = false;
            cx.notify();
        });
    }

    /// What's on disk right now (for the conflict diff). Read off the UI thread.
    pub fn read_disk(&self, buffer: &Entity<Buffer>, cx: &App) -> Task<anyhow::Result<String>> {
        let path = buffer.read(cx).path.clone();
        cx.background_executor().spawn(async move {
            let bytes = std::fs::read(&path)?;
            Ok(crate::buffer::decode(&bytes)
                .map(|d| d.text)
                .unwrap_or_else(|_| String::from_utf8_lossy(&bytes).into_owned()))
        })
    }

    fn apply_disk_bytes(&mut self, buffer: &Entity<Buffer>, bytes: &[u8], cx: &mut Context<Self>) {
        let path = buffer.read(cx).path.clone();
        let fresh = Buffer::from_bytes(path, bytes);
        buffer.update(cx, |b, cx| {
            b.text = fresh.text;
            b.line_ending = fresh.line_ending;
            b.bom = fresh.bom;
            b.read_only = fresh.read_only;
            b.notice = fresh.notice;
            b.disk_hash = fresh.disk_hash;
            b.dirty = false;
            b.conflict = false;
            b.revision += 1;
            cx.notify();
        });
        cx.emit(WorkspaceEvent::FileChangedOnDisk {
            buffer: buffer.clone(),
            reloaded: true,
        });
    }

    fn on_disk_changes(&mut self, changes: fswatch::Changes, cx: &mut Context<Self>) {
        let relevant: Vec<PathBuf> = changes
            .paths
            .into_iter()
            .filter(|p| !p.components().any(|c| c.as_os_str() == ".git"))
            .collect();
        if relevant.is_empty() {
            return;
        }
        if changes.structure_changed {
            cx.emit(WorkspaceEvent::TreeChanged);
        }
        for path in relevant {
            let Some(buffer) = self.buffer_for(&path, cx) else {
                continue;
            };
            let known = buffer.read(cx).disk_hash;
            let read_path = buffer.read(cx).path.clone();
            let read = cx
                .background_executor()
                .spawn(async move { std::fs::read(&read_path) });
            cx.spawn(async move |this, cx| {
                let result = read.await;
                let _ = this.update(cx, |this, cx| match result {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        cx.emit(WorkspaceEvent::FileDeleted(buffer.clone()));
                    }
                    Err(e) => log::warn!("re-read {}: {e}", path.display()),
                    Ok(bytes) => {
                        if hash_bytes(&bytes) == known {
                            return; // our own save, or a touch
                        }
                        if buffer.read(cx).dirty {
                            buffer.update(cx, |b, cx| {
                                b.conflict = true;
                                cx.notify();
                            });
                            cx.emit(WorkspaceEvent::FileChangedOnDisk {
                                buffer: buffer.clone(),
                                reloaded: false,
                            });
                        } else {
                            this.apply_disk_bytes(&buffer, &bytes, cx);
                        }
                    }
                });
            })
            .detach();
        }
    }
}
