//! A center tab showing what pi changed in one file: the before-snapshot
//! against the file on disk now (spec M2: "click an entry → side-by-side or inline diff").

use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::diff::{Diff, DiffFile, DiffMode, DiffState};
use gpui_kit::component::dock::{DockPlacement, PanelEvent, PanelState, TabGroup, panel_handle, register_panel};
use gpui_kit::component::{ActiveTheme, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::opener::DiffRequest;
use pide_core::{DockPosition, Panel, Services};

use crate::language_for;

pub const PANEL_NAME: &str = "pi_diff";

#[derive(Default)]
struct OpenDiffs(Vec<WeakEntity<DiffPanel>>);
impl Global for OpenDiffs {}

pub fn init(cx: &mut App) {
    cx.set_global(OpenDiffs::default());
    // Diffs hold in-memory snapshots, so a restored layout can't rebuild them.
    register_panel(cx, PANEL_NAME, |_, window, cx| {
        panel_handle(cx.new(|cx| DiffPanel::unavailable(window, cx)))
    });
}

/// Open (or refresh and focus) the diff tab for `req.path`.
pub fn open_diff(req: DiffRequest, window: &mut Window, cx: &mut App) {
    let win = window.window_handle().window_id();
    let existing = cx
        .global::<OpenDiffs>()
        .0
        .iter()
        .filter_map(|w| w.upgrade())
        .find(|p| p.read(cx).window_id == win && p.read(cx).req.as_ref().is_some_and(|r| r.path == req.path));
    if let Some(panel) = existing {
        let group = panel.update(cx, |p, cx| {
            p.req = Some(req);
            p.reload(cx);
            p.group.clone()
        });
        crate::select_tab_of(group, panel.entity_id(), window, cx);
        return;
    }
    let Some(dock) = crate::host(window, cx) else { return };
    let panel = cx.new(|cx| DiffPanel::new(req, window, cx));
    dock.update(cx, |d, cx| d.add_panel_view(panel_handle(panel.clone()), DockPlacement::Center, None, window, cx));
}

pub struct DiffPanel {
    req: Option<DiffRequest>,
    /// The project window this tab lives in, and its root.
    window_id: WindowId,
    root: PathBuf,
    state: Entity<DiffState>,
    mode: DiffMode,
    stats: (usize, usize),
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
            root: Services::of(window, cx).workspace.read(cx).root().to_path_buf(),
            state: cx.new(|cx| DiffState::new(Vec::<DiffFile>::new(), cx).with_mode(DiffMode::Split)),
            mode: DiffMode::Split,
            stats: (0, 0),
            error: None,
            focus: cx.focus_handle(),
            group: None,
            _load: None,
        }
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(req) = self.req.clone() else { return };
        let root = self.root.clone();
        let rel = req.path.strip_prefix(&root).unwrap_or(&req.path).display().to_string();
        let language = language_for(&req.path);
        let work = cx.background_executor().spawn(async move {
            let now = pide_core::changes::snapshot(&req.path);
            let before = req.before.clone().unwrap_or_default();
            let after = now.clone().unwrap_or_default();
            let stats = pide_core::changes::line_stats(&before, &after);
            let a = if req.before.is_some() { format!("a/{rel}") } else { "/dev/null".into() };
            let b = if now.is_some() { format!("b/{rel}") } else { "/dev/null".into() };
            let patch = similar::TextDiff::from_lines(&before, &after).unified_diff().context_radius(3).header(&a, &b).to_string();
            let files = if patch.is_empty() { Ok(Vec::new()) } else { DiffFile::parse(&patch).map_err(|e| format!("{e:?}")) };
            (files, stats)
        });
        self._load = Some(cx.spawn(async move |this, cx| {
            let (files, stats) = work.await;
            let _ = this.update(cx, |this, cx| {
                match files {
                    Ok(files) => {
                        let files: Vec<DiffFile> = files.into_iter().map(|f| f.with_language(language)).collect();
                        this.error = if files.is_empty() { Some("No differences: the file matches its before-snapshot.".into()) } else { None };
                        this.state.update(cx, |s, cx| s.set_files(files, cx));
                    }
                    Err(e) => this.error = Some(format!("Can't show this diff: {e}").into()),
                }
                this.stats = stats;
                cx.notify();
            });
        }));
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
        cx.global_mut::<OpenDiffs>().0.retain(|w| w.upgrade().is_some_and(|p| p.entity_id() != me));
    }
    fn dump(&self, _: &App) -> PanelState {
        PanelState::new(PANEL_NAME)
    }
}

impl gpui_kit::component::dock::Panel for DiffPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex().gap_1().child(div().text_color(cx.theme().primary).child("Δ")).child(self.title_text(cx))
    }
}

impl Panel for DiffPanel {
    fn title_text(&self, _: &App) -> SharedString {
        self.req
            .as_ref()
            .and_then(|r| r.path.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_else(|| "diff".into())
            .into()
    }
    fn default_dock(&self) -> DockPosition {
        DockPosition::Center
    }
}

impl Render for DiffPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let header = self.req.as_ref().map(|r| {
            let root = &self.root;
            let rel: PathBuf = r.path.strip_prefix(&root).unwrap_or(&r.path).to_path_buf();
            (rel.display().to_string(), r.label.clone(), r.before.is_none())
        });
        let split = self.mode == DiffMode::Split;
        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .when_some(header, |this, (rel, label, created)| {
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
                                .child(div().text_xs().text_color(theme.muted_foreground).child(format!(
                                    "{} · {}{} · before = when pi first touched it",
                                    label,
                                    if created { "created by pi · " } else { "" },
                                    format_args!("+{} −{}", self.stats.0, self.stats.1)
                                ))),
                        )
                        .child(
                            h_flex()
                                .gap_1()
                                .child(Button::new("split").small().label("Side by side").when(split, |b| b.primary()).on_click(
                                    cx.listener(|this, _, _, cx| this.set_mode(DiffMode::Split, cx)),
                                ))
                                .child(Button::new("inline").small().label("Inline").when(!split, |b| b.primary()).on_click(
                                    cx.listener(|this, _, _, cx| this.set_mode(DiffMode::Unified, cx)),
                                ))
                                .child(Button::new("refresh").small().ghost().label("Refresh").on_click(
                                    cx.listener(|this, _, _, cx| this.reload(cx)),
                                )),
                        ),
                )
            })
            .when_some(self.error.clone(), |this, e| {
                this.child(div().p_3().text_sm().text_color(theme.muted_foreground).child(e))
            })
            .when(self.error.is_none(), |this| {
                this.child(div().flex_1().overflow_hidden().child(Diff::new(&self.state).line_number(true).syntax_highlight(true)))
            })
    }
}
