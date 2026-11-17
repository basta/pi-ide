//! A center tab showing what pi changed in one file: the before-snapshot
//! against the file on disk now (spec M2: "click an entry → side-by-side or inline diff").

use std::path::PathBuf;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::diff::{
    Diff, DiffFile, DiffGutterMarker, DiffLinePosition, DiffMode, DiffSide, DiffState,
};
use gpui_kit::component::dock::{
    DockPlacement, PanelEvent, PanelInfo, PanelState, TabGroup, panel_handle, register_panel,
};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::opener::DiffRequest;
use pide_core::{DockPosition, Panel, Services};

use crate::language_for;

pub const PANEL_NAME: &str = "pi_diff";
/// Larger before-texts aren't saved in the layout (the tab won't come back).
const MAX_SAVED_BEFORE: usize = 512 * 1024;

fn restore_request(v: &serde_json::Value) -> Option<DiffRequest> {
    Some(DiffRequest {
        path: PathBuf::from(v.get("path")?.as_str()?),
        before: v.get("before").and_then(|b| b.as_str()).map(str::to_string),
        after: v.get("after").and_then(|b| b.as_str()).map(str::to_string),
        label: v
            .get("label")
            .and_then(|l| l.as_str())
            .unwrap_or("")
            .to_string()
            .into(),
        before_is: v
            .get("before_is")
            .and_then(|l| l.as_str())
            .unwrap_or("before")
            .to_string()
            .into(),
    })
}

#[derive(Default)]
struct OpenDiffs(Vec<WeakEntity<DiffPanel>>);
impl Global for OpenDiffs {}

pub fn init(cx: &mut App) {
    cx.set_global(OpenDiffs::default());
    // Diffs hold in-memory snapshots, so a restored layout can't rebuild them.
    // A diff tab saves its request (path and before-text) in the layout, so it
    // comes back after a restart; one without (too big, or older) can't.
    register_panel(cx, PANEL_NAME, |ctx, window, cx| {
        let req = match ctx.info() {
            PanelInfo::Panel(v) => restore_request(v),
            _ => None,
        };
        panel_handle(cx.new(|cx| match req {
            Some(req) => DiffPanel::new(req, window, cx),
            None => DiffPanel::unavailable(window, cx),
        }))
    });
}

/// Open (or refresh and focus) the diff tab for `req.path` (one per path and
/// label, so a commit's diff doesn't replace the local-changes one).
pub fn open_diff(req: DiffRequest, window: &mut Window, cx: &mut App) {
    let win = window.window_handle().window_id();
    let existing = cx
        .global::<OpenDiffs>()
        .0
        .iter()
        .filter_map(|w| w.upgrade())
        .find(|p| {
            p.read(cx).window_id == win
                && p.read(cx).req.as_ref().is_some_and(|r| {
                    r.path == req.path
                        && r.after.is_some() == req.after.is_some()
                        && (r.after.is_none() || r.label == req.label)
                })
        });
    if let Some(panel) = existing {
        let group = panel.update(cx, |p, cx| {
            p.req = Some(req);
            p.reload(cx);
            p.group.clone()
        });
        crate::select_tab_of(group, panel.entity_id(), window, cx);
        return;
    }
    let Some(dock) = crate::host(window, cx) else {
        return;
    };
    let panel = cx.new(|cx| DiffPanel::new(req, window, cx));
    dock.update(cx, |d, cx| {
        d.add_panel_view(
            panel_handle(panel.clone()),
            DockPlacement::Center,
            None,
            window,
            cx,
        )
    });
}

pub struct DiffPanel {
    req: Option<DiffRequest>,
    /// The project window this tab lives in, and its root.
    window_id: WindowId,
    root: PathBuf,
    state: Entity<DiffState>,
    mode: DiffMode,
    stats: (usize, usize),
    /// Texts the diff shows (before-snapshot, file now), its hunks, and the
    /// diff's file path (for gutter markers): each hunk gets a revert button.
    texts: Option<std::sync::Arc<(String, String)>>,
    hunks: Vec<pide_core::changes::Hunk>,
    diff_path: SharedString,
    error: Option<SharedString>,
    focus: FocusHandle,
    group: Option<WeakEntity<TabGroup>>,
    _load: Option<Task<()>>,
}

impl EventEmitter<PanelEvent> for DiffPanel {}

impl DiffPanel {
    fn new(req: DiffRequest, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self::empty(window, cx);
        this.req = Some(req);
        let weak = cx.weak_entity();
        cx.global_mut::<OpenDiffs>().0.push(weak);
        this.reload(cx);
        this
    }

    fn unavailable(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self::empty(window, cx);
        this.error = Some("This diff was from an earlier run of pide; its before-snapshot is gone. Close this tab.".into());
        this
    }

    fn empty(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            req: None,
            window_id: window.window_handle().window_id(),
            root: Services::of(window, cx)
                .workspace
                .read(cx)
                .root()
                .to_path_buf(),
            state: cx
                .new(|cx| DiffState::new(Vec::<DiffFile>::new(), cx).with_mode(DiffMode::Split)),
            mode: DiffMode::Split,
            stats: (0, 0),
            texts: None,
            hunks: Vec::new(),
            diff_path: SharedString::default(),
            error: None,
            focus: cx.focus_handle(),
            group: None,
            _load: None,
        }
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(req) = self.req.clone() else { return };
        let root = self.root.clone();
        let rel = req
            .path
            .strip_prefix(&root)
            .unwrap_or(&req.path)
            .display()
            .to_string();
        let language = language_for(&req.path);
        let work = cx.background_executor().spawn(async move {
            let now = match &req.after {
                Some(fixed) => Some(fixed.clone()),
                None => pide_core::changes::snapshot(&req.path),
            };
            let before = req.before.clone().unwrap_or_default();
            let after = now.clone().unwrap_or_default();
            let stats = pide_core::changes::line_stats(&before, &after);
            let a = if req.before.is_some() {
                format!("a/{rel}")
            } else {
                "/dev/null".into()
            };
            let b = if now.is_some() {
                format!("b/{rel}")
            } else {
                "/dev/null".into()
            };
            let patch = similar::TextDiff::from_lines(&before, &after)
                .unified_diff()
                .context_radius(3)
                .header(&a, &b)
                .to_string();
            let files = if patch.is_empty() {
                Ok(Vec::new())
            } else {
                DiffFile::parse(&patch).map_err(|e| format!("{e:?}"))
            };
            let hunks = if now.is_some() && req.after.is_none() {
                pide_core::changes::hunks(&before, &after)
            } else {
                Vec::new()
            };
            (files, stats, hunks, (before, after))
        });
        self._load = Some(cx.spawn(async move |this, cx| {
            let (files, stats, hunks, texts) = work.await;
            let _ = this.update(cx, |this, cx| {
                match files {
                    Ok(files) => {
                        let files: Vec<DiffFile> = files
                            .into_iter()
                            .map(|f| f.with_language(language))
                            .collect();
                        this.error = if files.is_empty() {
                            Some("No differences: the file matches its before-snapshot.".into())
                        } else {
                            None
                        };
                        this.diff_path =
                            files.first().map(|f| f.path().clone()).unwrap_or_default();
                        this.state.update(cx, |s, cx| s.set_files(files, cx));
                    }
                    Err(e) => this.error = Some(format!("Can't show this diff: {e}").into()),
                }
                this.stats = stats;
                this.hunks = hunks;
                this.texts = Some(std::sync::Arc::new(texts));
                cx.notify();
            });
        }));
    }

    /// Put one hunk back the way it was before pi (writes the file, like the
    /// whole-file revert; an open, unedited tab reloads).
    fn revert_hunk(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(req), Some(texts), Some(hunk)) = (
            self.req.clone(),
            self.texts.clone(),
            self.hunks.get(ix).cloned(),
        ) else {
            return;
        };
        let write = cx.background_executor().spawn(async move {
            // Only if the file is still what the diff shows.
            let now = pide_core::changes::snapshot(&req.path).unwrap_or_default();
            if now != texts.1 {
                return Err(
                    "the file changed since this diff was shown; refresh and try again".to_string(),
                );
            }
            let text = pide_core::changes::revert_hunk(&texts.0, &texts.1, &hunk);
            std::fs::write(&req.path, text).map_err(|e| e.to_string())
        });
        cx.spawn_in(window, async move |this, cx| {
            let r = write.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Err(e) = r {
                    window.push_notification(Notification::error(format!("Not reverted: {e}")), cx);
                }
                this.reload(cx);
            });
        })
        .detach();
    }

    fn set_mode(&mut self, mode: DiffMode, cx: &mut Context<Self>) {
        self.mode = mode;
        self.state.update(cx, |s, cx| s.set_mode(mode, cx));
        cx.notify();
    }
}

impl Focusable for DiffPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl gpui_kit::component::dock::BasePanel for DiffPanel {
    fn panel_name(&self) -> &'static str {
        PANEL_NAME
    }
    fn on_added_to(&mut self, group: WeakEntity<TabGroup>, _: &mut Window, _: &mut Context<Self>) {
        self.group = Some(group);
    }
    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let me = cx.entity_id();
        cx.global_mut::<OpenDiffs>()
            .0
            .retain(|w| w.upgrade().is_some_and(|p| p.entity_id() != me));
    }
    fn dump(&self, _: &App) -> PanelState {
        let mut state = PanelState::new(PANEL_NAME);
        if let Some(r) = &self.req
            && r.before.as_ref().map_or(0, String::len) + r.after.as_ref().map_or(0, String::len)
                <= MAX_SAVED_BEFORE
        {
            state.info = PanelInfo::panel(serde_json::json!({
                "path": r.path,
                "before": r.before,
                "after": r.after,
                "label": r.label.as_ref(),
                "before_is": r.before_is.as_ref(),
            }));
        }
        state
    }
}

impl gpui_kit::component::dock::Panel for DiffPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_1()
            .child(div().text_color(cx.theme().primary).child("Δ"))
            .child(self.title_text(cx))
    }
}

impl Panel for DiffPanel {
    fn title_text(&self, _: &App) -> SharedString {
        let Some(r) = &self.req else {
            return "diff".into();
        };
        let name = r
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "diff".into());
        if r.after.is_some() {
            format!("{name} @ {}", r.label).into()
        } else {
            name.into()
        }
    }
    fn default_dock(&self) -> DockPosition {
        DockPosition::Center
    }
}

impl DiffPanel {
    /// pi's reason and hunk notes for this file (collaborator mode), from the
    /// session that changed it (the active one first). Live diffs only.
    fn render_pi_notes(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let req = self.req.as_ref()?;
        if req.after.is_some() {
            return None;
        }
        let services = pide_core::Services::for_window(self.window_id, cx)?;
        let sessions = services.sessions.read(cx);
        let active = sessions.active();
        let mut agents = vec![active.clone()];
        agents.extend(sessions.all().iter().filter(|a| **a != active).cloned());
        let (agent, change) = agents.into_iter().find_map(|a| {
            let c = a
                .read(cx)
                .changes
                .iter()
                .find(|c| c.path == req.path && (c.summary.is_some() || !c.notes.is_empty()))
                .cloned()?;
            Some((a, c))
        })?;
        let stops: Vec<pide_core::collab::Stop> = agent
            .read(cx)
            .stops()
            .into_iter()
            .filter(|s| {
                s.path == req.path && matches!(s.at, pide_core::collab::StopRef::Hunk { .. })
            })
            .collect();
        let theme = cx.theme();
        let color = pide_core::colors::pi_note(cx);
        let hunk_of = |line: usize| {
            self.hunks.iter().position(|h| {
                h.after.contains(&(line.saturating_sub(1)))
                    || (h.after.is_empty() && h.after.start + 1 == line)
            })
        };
        Some(
            v_flex()
                .gap_0p5()
                .px_3()
                .py_1p5()
                .border_b_1()
                .border_color(theme.border)
                .bg(color.opacity(0.06))
                .text_sm()
                .when_some(change.summary.clone(), |this, s| {
                    this.child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .flex_none()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(color)
                                    .child("pi"),
                            )
                            .child(div().child(s)),
                    )
                })
                .children(stops.into_iter().enumerate().map(|(i, st)| {
                    let label = match hunk_of(st.line) {
                        Some(h) => format!("#{} · line {}", h + 1, st.line),
                        None => format!("line {}", st.line),
                    };
                    let agent = agent.clone();
                    let note = st.note.clone();
                    h_flex()
                        .id(("pi-hunk-note", i))
                        .gap_2()
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.list_hover))
                        .on_click(move |_, window, cx| {
                            pide_core::collab::go_to(&agent, st.clone(), window, cx)
                        })
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .font_family(theme.mono_font_family.clone())
                                .text_color(theme.muted_foreground)
                                .child(label),
                        )
                        .child(div().child(note))
                }))
                .into_any_element(),
        )
    }
}

impl Render for DiffPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let header = self.req.as_ref().map(|r| {
            let root = &self.root;
            let rel: PathBuf = r.path.strip_prefix(root).unwrap_or(&r.path).to_path_buf();
            (
                rel.display().to_string(),
                r.label.clone(),
                r.before.is_none(),
                r.before_is.clone(),
            )
        });
        let split = self.mode == DiffMode::Split;
        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .when_some(header, |this, (rel, label, created, before_is)| {
                this.child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .py_2()
                        .border_b_1()
                        .border_color(theme.border)
                        .child(
                            v_flex()
                                .child(div().font_weight(FontWeight::SEMIBOLD).child(rel))
                                .child(div().text_xs().text_color(theme.muted_foreground).child(
                                    format!(
                                        "{} · {}{} · before = {}",
                                        label,
                                        if created { "new file · " } else { "" },
                                        format_args!("+{} −{}", self.stats.0, self.stats.1),
                                        before_is
                                    ),
                                )),
                        )
                        .child(
                            h_flex()
                                .gap_1()
                                .child(
                                    Button::new("split")
                                        .small()
                                        .label("Side by side")
                                        .map(|b| pide_core::colors::toggle(b, split, cx))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.set_mode(DiffMode::Split, cx)
                                        })),
                                )
                                .child(
                                    Button::new("inline")
                                        .small()
                                        .label("Inline")
                                        .map(|b| pide_core::colors::toggle(b, !split, cx))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.set_mode(DiffMode::Unified, cx)
                                        })),
                                )
                                .child(
                                    Button::new("refresh")
                                        .small()
                                        .ghost()
                                        .label("Refresh")
                                        .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                                ),
                        ),
                )
            })
            .children(self.render_pi_notes(cx))
            .when_some(self.error.clone(), |this, e| {
                this.child(
                    div()
                        .p_3()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(e),
                )
            })
            .when(self.error.is_none(), |this| {
                this.child(
                    div().flex_1().overflow_hidden().child(
                        Diff::new(&self.state)
                            .line_number(true)
                            .syntax_highlight(true)
                            // Wrap to the column: unwrapped, both sides take the
                            // longest line's width and the tab scrolls sideways.
                            .soft_wrap(true)
                            .when(!self.hunks.is_empty(), |d| {
                                // A revert button on each hunk's first line: the
                                // new side, or the old side for a pure deletion.
                                let markers: Vec<DiffGutterMarker> = self
                                    .hunks
                                    .iter()
                                    .enumerate()
                                    .map(|(i, h)| {
                                        let pos = if h.after.is_empty() {
                                            DiffLinePosition::new(
                                                self.diff_path.clone(),
                                                DiffSide::Original,
                                                h.before.start + 1,
                                            )
                                        } else {
                                            DiffLinePosition::new(
                                                self.diff_path.clone(),
                                                DiffSide::Modified,
                                                h.after.start + 1,
                                            )
                                        };
                                        DiffGutterMarker::new(("revert-hunk", i), pos)
                                            .with_label("Revert this change")
                                    })
                                    .collect();
                                let index: std::rc::Rc<Vec<(DiffSide, usize)>> = std::rc::Rc::new(
                                    markers
                                        .iter()
                                        .map(|m| (m.position().side(), m.position().line()))
                                        .collect(),
                                );
                                let panel = cx.weak_entity();
                                d.gutter_markers(markers).render_gutter_marker(
                                    move |marker, _, _| {
                                        let key =
                                            (marker.position().side(), marker.position().line());
                                        let ix = index.iter().position(|k| *k == key).unwrap_or(0);
                                        let panel = panel.clone();
                                        Button::new(marker.id().clone())
                                            .ghost()
                                            .xsmall()
                                            // Numbered, so pi's hunk notes above can say which.
                                            .label(format!("{} ↶", ix + 1))
                                            .tooltip(marker.label().clone())
                                            .on_click(move |_, window, cx| {
                                                let panel = panel.clone();
                                                // Not inside the diff's own update.
                                                window.defer(cx, move |window, cx| {
                                                    let _ = panel.update(cx, |p, cx| {
                                                        p.revert_hunk(ix, window, cx)
                                                    });
                                                });
                                            })
                                    },
                                )
                            }),
                    ),
                )
            })
    }
}
