//! The `pide` module scripts import: the project they're in, its files,
//! diagnostics, the editor, pide's commands and pi, and a stream of events.
//!
//! Every function finds its project through the window the calling script
//! runs in (`gpui_shell::with_current`), so one registration serves every
//! project window. Paths in and out are relative to the project root; lines
//! and columns are 1-based.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use futures::channel::oneshot;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;
use gpui_shell::{HostError, HostModule, HostObject, HostValue};
use lsp_types::DiagnosticSeverity;
use pide_core::{Services, WorkspaceEvent};
use pide_lsp::{LspEvent, LspStore};

pub const MODULE: &str = "pide";

/// TypeScript for the module. Checked against the registered functions at start-up.
pub const DECLARATIONS: &str = r#"
/** The project root (absolute). Paths elsewhere in this module are relative to it. */
export function root(): string;
/** The file and cursor of the active editor tab, or null. Lines and columns are 1-based. */
export function activeFile(): { path: string, line: number, column: number, selection: string | null } | null;
/** Open a file in an editor tab, optionally at a 1-based line and column. */
export function openFile(path: string, line?: number, column?: number): void;
/** Every diagnostic the language servers report (spell-check hints excluded). */
export function diagnostics(): Diagnostic[];
export interface Diagnostic {
  path: string, line: number, column: number, endLine: number, endColumn: number,
  severity: "error" | "warning" | "info" | "hint", message: string, source: string | null,
}
/** A file's text: the open editor's (with unsaved edits) or the file on disk. */
export function readFile(path: string): Promise<string>;
/** Files in the project, gitignore-aware, at most 20000. */
export function listFiles(): Promise<string[]>;
/** Run a pide command by id (as in the keymap), e.g. "view.problems". */
export function runCommand(id: string): boolean;
/** Put text in the pi box (focused, not sent). */
export function askPi(text: string): void;
/** pide's colors as hex strings, so a panel matches the theme. */
export function colors(): Colors;
export interface Colors {
  background: string, foreground: string, muted: string, border: string, accent: string,
  selection: string, list_hover: string, danger: string, warning: string, info: string,
  success: string, pi: string,
}
/**
 * Resolves with the next thing that happens in the project. Loop on it from a
 * `cx.spawn` task and `cx.notify()` after handling one.
 */
export function nextEvent(): Promise<PideEvent>;
export interface PideEvent {
  kind: "diagnostics" | "saved" | "changed" | "active" | "files" | "git",
  path: string | null,
}
"#;

pub fn module() -> HostModule {
    HostModule::new(MODULE)
        .declarations(DECLARATIONS)
        .function("root", |_| {
            with_project(|s, _, cx| Ok(s.workspace.read(cx).root().display().to_string().into()))
        })
        .function("activeFile", |_| {
            with_project(|s, id, cx| {
                let root = s.workspace.read(cx).root().to_path_buf();
                let Some(path) = s.workspace.read(cx).active().map(|b| b.read(cx).path.clone())
                else {
                    return Ok(HostValue::Null);
                };
                // The cursor needs the window, which is busy if the script runs
                // inside one of its events: then only the path is known.
                let handle = cx.windows().into_iter().find(|w| w.window_id() == id);
                let ctx = handle
                    .and_then(|h| h.update(cx, |_, window, cx| pide_core::context::current(window, cx)).ok())
                    .flatten();
                let mut o = HostObject::new().field("path", rel(&root, &path));
                o = match ctx {
                    Some(c) => o
                        .field("line", num(c.start.0))
                        .field("column", num(c.start.1))
                        .field("selection", c.selected),
                    None => o
                        .field("line", HostValue::Null)
                        .field("column", HostValue::Null)
                        .field("selection", HostValue::Null),
                };
                Ok(o.into())
            })
        })
        .function("openFile", |args| {
            let path = args.string(0)?.to_string();
            let line = args.get(1).and_then(HostValue::as_number);
            let column = args.get(2).and_then(HostValue::as_number);
            with_project(move |s, id, cx| {
                let abs = resolve(s.workspace.read(cx).root(), &path)?;
                // Deferred: opening a tab from inside the script's own call would
                // update the dock while the panel is being updated.
                later(id, cx, move |window, cx| match line {
                    Some(l) => pide_core::opener::open_file_at(
                        abs,
                        (l.max(1.) - 1.) as u32,
                        (column.unwrap_or(1.).max(1.) - 1.) as u32,
                        window,
                        cx,
                    ),
                    None => pide_core::opener::open_file(abs, window, cx),
                });
                Ok(HostValue::Null)
            })
        })
        .function("diagnostics", |_| {
            with_project(|s, _, cx| {
                let root = s.workspace.read(cx).root().to_path_buf();
                let store = LspStore::for_workspace(&s.workspace, cx);
                Ok(diagnostics_value(&root, store.read(cx).diagnostics()))
            })
        })
        .async_function("readFile", |args| {
            let path = args.string(0)?.to_string();
            let (abs, open) = with_project(move |s, _, cx| {
                let ws = s.workspace.read(cx);
                let abs = resolve(ws.root(), &path)?;
                let open = ws.buffer_for(&abs, cx).map(|b| b.read(cx).text().to_string());
                Ok((abs, open))
            })?;
            Ok(async move {
                match open {
                    Some(text) => Ok(HostValue::Str(text)),
                    None => std::fs::read_to_string(&abs)
                        .map(HostValue::Str)
                        .map_err(|e| HostError::new(format!("{}: {e}", abs.display()))),
                }
            })
        })
        .async_function("listFiles", |_| {
            let root = with_project(|s, _, cx| Ok(s.workspace.read(cx).root().to_path_buf()))?;
            Ok(async move { Ok(list_files(&root, 20_000).into()) })
        })
        .function("runCommand", |args| {
            let id = args.string(0)?.to_string();
            with_project(move |_, win, cx| {
                let known = pide_core::CommandRegistry::contains(cx, &id);
                if known {
                    later(win, cx, move |window, cx| {
                        pide_core::CommandRegistry::run(&id, window, cx);
                    });
                }
                Ok(known.into())
            })
        })
        .function("askPi", |args| {
            let text = args.string(0)?.to_string();
            with_project(move |_, id, cx| {
                later(id, cx, move |window, cx| pide_core::opener::ask_agent(text, window, cx));
                Ok(HostValue::Null)
            })
        })
        .function("colors", |_| with_project(|_, _, cx| Ok(colors(cx))))
        .async_function("nextEvent", |_| {
            let (tx, rx) = oneshot::channel();
            with_project(move |_, id, _| {
                WAITERS.with(|w| w.borrow_mut().entry(id).or_default().push(tx));
                Ok(())
            })?;
            Ok(async move {
                rx.await
                    .map_err(|_| HostError::new("the project window closed"))
            })
        })
}

fn num(n: impl Into<f64>) -> HostValue {
    HostValue::Number(n.into())
}

/// Runs `f` with the calling script's project.
///
/// gpui-shell 0.10 gives a host function the `App` but not the caller's window,
/// so the project is found through the calling panel's grant: every script
/// panel may read its project root, and that root names the window. (Stopgap
/// until gpui-shell exposes the caller's window; see docs/GAPS.md.)
fn with_project<R>(
    f: impl FnOnce(&Services, WindowId, &mut App) -> Result<R, HostError>,
) -> Result<R, HostError> {
    let grant = format!("{:?}", gpui_shell::current_capabilities());
    gpui_shell::with_current_app(|cx| {
        let (id, _) = Services::open_projects(cx)
            .into_iter()
            .filter(|(_, root)| grant.contains(&format!("{root:?}")))
            .max_by_key(|(_, root)| root.as_os_str().len())
            .ok_or_else(|| HostError::new("pide: this panel isn't in a project window"))?;
        let services =
            Services::for_window(id, cx).ok_or_else(|| HostError::new("the project closed"))?;
        f(&services, id, cx)
    })
    .ok_or_else(|| HostError::new("pide: called outside a script call"))?
}

/// Run `f` in the project's window once the current script call is done.
fn later(id: WindowId, cx: &mut App, f: impl FnOnce(&mut Window, &mut App) + 'static) {
    cx.defer(move |cx| {
        if let Some(handle) = cx.windows().into_iter().find(|w| w.window_id() == id) {
            let _ = handle.update(cx, |_, window, cx| f(window, cx));
        }
    });
}

/// A project-relative path, refusing anything that leaves the project.
fn resolve(root: &Path, path: &str) -> Result<PathBuf, HostError> {
    let p = Path::new(path);
    if p.is_absolute() {
        return if p.starts_with(root) {
            Ok(p.to_path_buf())
        } else {
            Err(HostError::new(format!("{path} is outside the project")))
        };
    }
    if p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err(HostError::new(format!("{path} leaves the project")));
    }
    Ok(root.join(p))
}

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).display().to_string()
}

pub fn diagnostics_value(
    root: &Path,
    diags: &std::collections::BTreeMap<PathBuf, pide_lsp::FileDiagnostics>,
) -> HostValue {
    let mut out = vec![];
    for (path, file) in diags {
        for d in &file.items {
            let severity = match d.severity {
                Some(DiagnosticSeverity::ERROR) => "error",
                Some(DiagnosticSeverity::WARNING) => "warning",
                Some(DiagnosticSeverity::HINT) => "hint",
                _ => "info",
            };
            out.push(HostValue::from(
                HostObject::new()
                    .field("path", rel(root, path))
                    .field("line", num(d.range.start.line + 1))
                    .field("column", num(d.range.start.character + 1))
                    .field("endLine", num(d.range.end.line + 1))
                    .field("endColumn", num(d.range.end.character + 1))
                    .field("severity", severity)
                    .field("message", d.message.clone())
                    .field("source", d.source.clone()),
            ));
        }
    }
    HostValue::Array(out)
}

fn list_files(root: &Path, cap: usize) -> Vec<String> {
    ignore::WalkBuilder::new(root)
        .hidden(true)
        .build()
        .flatten()
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .take(cap)
        .map(|e| rel(root, e.path()))
        .collect()
}

fn hex(c: Hsla) -> String {
    let c = c.to_rgb();
    format!(
        "#{:02x}{:02x}{:02x}",
        (c.r * 255.).round() as u8,
        (c.g * 255.).round() as u8,
        (c.b * 255.).round() as u8
    )
}

fn colors(cx: &App) -> HostValue {
    let t = cx.theme();
    HostObject::new()
        .field("background", hex(t.background))
        .field("foreground", hex(t.foreground))
        .field("muted", hex(t.muted_foreground))
        .field("border", hex(t.border))
        .field("accent", hex(t.primary))
        .field("selection", hex(t.selection))
        .field("list_hover", hex(t.list_hover))
        .field("danger", hex(t.danger))
        .field("warning", hex(t.warning))
        .field("info", hex(t.info))
        .field("success", hex(t.success))
        .field("pi", hex(pide_core::colors::pi_note(cx)))
        .into()
}

thread_local! {
    /// Scripts waiting in `nextEvent`, per project window.
    static WAITERS: RefCell<HashMap<WindowId, Vec<oneshot::Sender<HostValue>>>> =
        RefCell::new(HashMap::new());
}

/// Windows whose project events already feed `nextEvent`.
#[derive(Default)]
struct Watched(HashMap<WindowId, Vec<Subscription>>);
impl Global for Watched {}

/// Start passing this window's project events to waiting scripts (once per window).
pub fn watch_window(window: &Window, cx: &mut App) {
    let id = window.window_handle().window_id();
    if cx.default_global::<Watched>().0.contains_key(&id) {
        return;
    }
    let Some(services) = Services::for_window(id, cx) else {
        return;
    };
    let root = services.workspace.read(cx).root().to_path_buf();
    let store = LspStore::for_workspace(&services.workspace, cx);
    let r = root.clone();
    let subs = vec![
        cx.subscribe(&services.workspace, move |_, ev: &WorkspaceEvent, cx| {
            let path = |b: &Entity<pide_core::Buffer>| Some(rel(&r, &b.read(cx).path));
            let (kind, path) = match ev {
                WorkspaceEvent::BufferSaved(b) => ("saved", path(b)),
                WorkspaceEvent::BufferChanged(b) | WorkspaceEvent::FileChangedOnDisk { buffer: b, .. } => {
                    ("changed", path(b))
                }
                WorkspaceEvent::ActiveBufferChanged => ("active", None),
                WorkspaceEvent::TreeChanged => ("files", None),
                WorkspaceEvent::GitChanged => ("git", None),
                _ => return,
            };
            wake(id, kind, path);
        }),
        cx.subscribe(&store, move |_, ev: &LspEvent, _| {
            if let LspEvent::Diagnostics { path } = ev {
                wake(id, "diagnostics", Some(rel(&root, path)));
            }
        }),
    ];
    cx.default_global::<Watched>().0.insert(id, subs);
}

/// Stop for a closed window: its waiting scripts get an error.
pub fn forget_window(id: WindowId, cx: &mut App) {
    if cx.has_global::<Watched>() {
        cx.global_mut::<Watched>().0.remove(&id);
    }
    WAITERS.with(|w| w.borrow_mut().remove(&id));
}

fn wake(id: WindowId, kind: &str, path: Option<String>) {
    let waiting = WAITERS.with(|w| w.borrow_mut().remove(&id)).unwrap_or_default();
    for tx in waiting {
        let _ = tx.send(
            HostObject::new()
                .field("kind", kind)
                .field("path", path.clone())
                .into(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn paths_stay_in_the_project() {
        let root = Path::new("/p");
        assert_eq!(resolve(root, "src/a.rs").unwrap(), Path::new("/p/src/a.rs"));
        assert!(resolve(root, "../etc/passwd").is_err());
        assert!(resolve(root, "/etc/passwd").is_err());
        assert_eq!(resolve(root, "/p/x").unwrap(), Path::new("/p/x"));
    }

    #[::core::prelude::v1::test]
    fn declarations_match_the_functions() {
        // export_module checks the declared names against the registered ones.
        gpui_shell::export_module(module()).unwrap();
        gpui_shell::clear_exported_modules();
    }
}
