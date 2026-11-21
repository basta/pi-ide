//! The Review tab: everything pi changed and said in the open sessions, in one
//! place (docs/COLLAB.md, "Review"). One GPUI Kit `Diff` holds every changed
//! file, session by session; pi's reason sits under each file's header and its
//! notes under the lines they're about, with their threads and a reply box.
//! On the left, an index: what needs you, then each session's files.
//!
//! The data is [`pide_core::review`], built off the UI thread from the
//! sessions' change lists; actions go back through the sessions' `Agent`s.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::diff::{
    Diff, DiffAnnotation, DiffFile, DiffGutterMarker, DiffLinePosition, DiffMode, DiffSide,
    DiffState,
};
use gpui_kit::component::dock::{
    DockPlacement, PanelEvent, PanelState, TabGroup, panel_handle, register_panel,
};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::component::{ActiveTheme, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::collab::{Stop, StopRef};
use pide_core::review::{NeedKind, NoteAt, Review, SessionInput};
use pide_core::{Agent, CommandRegistry, CommandSpec, DockPosition, Panel, Services};

use crate::language_for;

pub const PANEL_NAME: &str = "pi_review";

#[derive(Default)]
struct OpenReviews(Vec<WeakEntity<ReviewPanel>>);
impl Global for OpenReviews {}

pub fn init(cx: &mut App) {
    cx.set_global(OpenReviews::default());
    register_panel(cx, PANEL_NAME, |_, window, cx| {
        panel_handle(cx.new(|cx| ReviewPanel::new(window, cx)))
    });
    CommandRegistry::register(
        cx,
        CommandSpec::new("agent.review", "pi: Review Changes").key("cmd-shift-r"),
        open_review,
    );
}

/// Open the window's Review tab (or bring it forward and refresh it).
pub fn open_review(window: &mut Window, cx: &mut App) {
    let win = window.window_handle().window_id();
    let existing = cx
        .global::<OpenReviews>()
        .0
        .iter()
        .filter_map(|w| w.upgrade())
        .find(|p| p.read(cx).window_id == win);
    if let Some(panel) = existing {
        let group = panel.update(cx, |p, cx| {
            p.rebuild(cx);
            p.group.clone()
        });
        crate::select_tab_of(group, panel.entity_id(), window, cx);
        return;
    }
    let Some(dock) = crate::host(window, cx) else {
        return;
    };
    let panel = cx.new(|cx| ReviewPanel::new(window, cx));
    dock.update(cx, |d, cx| {
        d.add_panel_view(panel_handle(panel), DockPlacement::Center, None, window, cx)
    });
}

/// What one annotation shows.
#[derive(Clone)]
enum Ann {
    /// Under a file's header: pi's reason (or that there's none) and the notes
    /// that don't sit on a line shown in the diff.
    File { key: String },
    /// Under a line: one note and its thread.
    Note { key: String, note: usize },
}

/// Everything the diff's render callbacks need, rebuilt with the review.
struct View {
    review: Review,
    /// The diff's file path (as `DiffFile::parse` read it) → review key.
    by_diff_path: HashMap<SharedString, String>,
    review_to_diff: HashMap<String, SharedString>,
    anns: HashMap<String, Ann>,
    /// Session (index into `review.sessions`) → its agent.
    agents: Vec<Entity<Agent>>,
    root: PathBuf,
    /// Reply boxes and pi's words as markdown, by annotation id / slot.
    replies: HashMap<String, Entity<TextareaState>>,
    markdown: HashMap<String, Entity<TextViewState>>,
    panel: WeakEntity<ReviewPanel>,
}

pub struct ReviewPanel {
    window_id: WindowId,
    root: PathBuf,
    sessions: Entity<pide_core::Sessions>,
    state: Entity<DiffState>,
    mode: DiffMode,
    view: Option<Rc<View>>,
    loading: bool,
    /// Kept across rebuilds so a half-written reply survives pi's next edit.
    replies: HashMap<String, Entity<TextareaState>>,
    markdown: HashMap<String, Entity<TextViewState>>,
    /// What the last build saw: idle sessions' revisions and who was busy.
    seen_sig: Vec<(EntityId, u64, usize, usize, bool)>,
    focus: FocusHandle,
    group: Option<WeakEntity<TabGroup>>,
    _subs: Vec<Subscription>,
    _agent_subs: Vec<Subscription>,
    _load: Option<Task<()>>,
    _debounce: Option<Task<()>>,
}

impl EventEmitter<PanelEvent> for ReviewPanel {}

impl ReviewPanel {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let services = Services::of(window, cx);
        let root = services.workspace.read(cx).root().to_path_buf();
        let sessions = services.sessions.clone();
        let subs = vec![cx.observe(&sessions, |this: &mut Self, _, cx| {
            this.watch_agents(cx);
            this.maybe_rebuild(cx);
        })];
        let mut this = Self {
            window_id: window.window_handle().window_id(),
            root,
            sessions,
            state: cx
                .new(|cx| DiffState::new(Vec::<DiffFile>::new(), cx).with_mode(DiffMode::Unified)),
            mode: DiffMode::Unified,
            view: None,
            loading: true,
            replies: HashMap::new(),
            markdown: HashMap::new(),
            seen_sig: Vec::new(),
            focus: cx.focus_handle(),
            group: None,
            _subs: subs,
            _agent_subs: Vec::new(),
            _load: None,
            _debounce: None,
        };
        let weak = cx.weak_entity();
        cx.global_mut::<OpenReviews>().0.push(weak);
        this.watch_agents(cx);
        this.rebuild(cx);
        this
    }

    fn watch_agents(&mut self, cx: &mut Context<Self>) {
        let agents = self.sessions.read(cx).all().to_vec();
        self._agent_subs = agents
            .iter()
            .map(|a| cx.observe(a, |this: &mut Self, _, cx| this.maybe_rebuild(cx)))
            .collect();
    }

    /// The sessions' state as far as the review cares. A busy session's notes
    /// are left out until it settles, so the page doesn't move while you read.
    fn signature(&self, cx: &App) -> Vec<(EntityId, u64, usize, usize, bool)> {
        self.sessions
            .read(cx)
            .all()
            .iter()
            .map(|a| {
                let r = a.read(cx);
                if r.is_busy() {
                    (a.entity_id(), 0, 0, 0, true)
                } else {
                    (
                        a.entity_id(),
                        r.notes_rev,
                        r.changes.len(),
                        r.pins.len(),
                        false,
                    )
                }
            })
            .collect()
    }

    fn maybe_rebuild(&mut self, cx: &mut Context<Self>) {
        if self.signature(cx) == self.seen_sig {
            return;
        }
        self._debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(250))
                .await;
            let _ = this.update(cx, |this, cx| this.rebuild(cx));
        }));
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.seen_sig = self.signature(cx);
        let agents = self.sessions.read(cx).all().to_vec();
        // Snapshot on the UI thread; diffing and reading files happen off it.
        let inputs: Vec<(usize, String, bool, Vec<_>, Vec<_>)> = agents
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let r = a.read(cx);
                (
                    i,
                    r.name.to_string(),
                    r.is_busy(),
                    r.changes.clone(),
                    r.pins.clone(),
                )
            })
            .filter(|(_, _, _, c, p)| !c.is_empty() || !p.is_empty())
            .collect();
        let root = self.root.clone();
        let work = cx.background_executor().spawn(async move {
            let sessions: Vec<SessionInput> = inputs
                .iter()
                .map(|(i, name, busy, changes, pins)| SessionInput {
                    index: *i,
                    name: name.clone(),
                    busy: *busy,
                    changes,
                    pins,
                })
                .collect();
            let review = pide_core::review::build(&sessions, &root, pide_core::changes::snapshot);
            let mut files = Vec::new();
            for (_, f) in review.files() {
                if f.patch.is_empty() {
                    continue;
                }
                match DiffFile::parse(&f.patch) {
                    Ok(parsed) => {
                        for d in parsed {
                            files.push((f.key.clone(), d.with_language(language_for(&f.path))));
                        }
                    }
                    Err(e) => log::warn!("review: can't show {}: {e:?}", f.rel),
                }
            }
            (review, files)
        });
        self._load = Some(cx.spawn(async move |this, cx| {
            let (review, files) = work.await;
            let _ = this.update(cx, |this, cx| this.loaded(review, files, agents, cx));
        }));
    }

    fn loaded(
        &mut self,
        review: Review,
        files: Vec<(String, DiffFile)>,
        agents: Vec<Entity<Agent>>,
        cx: &mut Context<Self>,
    ) {
        let mut by_diff_path = HashMap::new();
        let mut review_to_diff = HashMap::new();
        for (key, f) in &files {
            by_diff_path.insert(f.path().clone(), key.clone());
            review_to_diff.insert(key.clone(), f.path().clone());
        }
        let mut anns = HashMap::new();
        for (_, f) in review.files() {
            anns.insert(format!("file:{}", f.key), Ann::File { key: f.key.clone() });
            for (i, n) in f.notes.iter().enumerate() {
                if n.in_diff {
                    anns.insert(
                        note_id(&f.key, i),
                        Ann::Note {
                            key: f.key.clone(),
                            note: i,
                        },
                    );
                }
            }
        }
        let session_agents: Vec<Entity<Agent>> = review
            .sessions
            .iter()
            .filter_map(|s| agents.get(s.index).cloned())
            .collect();
        // Markdown views for pi's words (the notes, reasons and pi's replies).
        for (_, f) in review.files() {
            if let Some(s) = &f.summary {
                self.md(format!("sum:{}", f.key), s, cx);
            }
            for (i, n) in f.notes.iter().enumerate() {
                self.md(format!("{}#0", note_id(&f.key, i)), &n.stop.note, cx);
                for (j, r) in n.stop.thread.iter().enumerate().filter(|(_, r)| r.pi) {
                    self.md(format!("{}#{}", note_id(&f.key, i), j + 1), &r.text, cx);
                }
            }
        }
        log::info!(
            "review: {} files in the diff, {} sessions",
            files.len(),
            review.sessions.len()
        );
        self.state.update(cx, |s, cx| {
            s.set_files(files.into_iter().map(|(_, f)| f), cx)
        });
        self.loading = false;
        self.view = Some(Rc::new(View {
            review,
            by_diff_path,
            review_to_diff,
            anns,
            agents: session_agents,
            root: self.root.clone(),
            replies: HashMap::new(),
            markdown: self.markdown.clone(),
            panel: cx.weak_entity(),
        }));
        cx.notify();
    }

    fn md(&mut self, slot: String, text: &str, cx: &mut Context<Self>) {
        let text = pide_core::math::normalize_delimiters(text);
        let view = self
            .markdown
            .entry(slot)
            .or_insert_with(|| cx.new(|cx| TextViewState::markdown("", cx)))
            .clone();
        view.update(cx, |v, cx| v.set_text(&text, cx));
    }

    /// The reply box for a note (made on first render; kept across rebuilds).
    fn reply_box(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TextareaState> {
        if let Some(r) = self.replies.get(id) {
            return r.clone();
        }
        let r = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 6)
                .submit_on_enter(true)
                .placeholder("Reply to pi")
        });
        let id_owned = id.to_string();
        cx.subscribe_in(
            &r,
            window,
            move |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::PressEnter { shift: false, .. } => {
                    this.send_reply(&id_owned, window, cx)
                }
                InputEvent::Change => cx.notify(),
                _ => {}
            },
        )
        .detach();
        self.replies.insert(id.to_string(), r.clone());
        r
    }

    fn find_note(&self, id: &str) -> Option<(Entity<Agent>, NoteAt)> {
        let view = self.view.as_ref()?;
        let Some(Ann::Note { key, note }) = view.anns.get(id) else {
            return find_any_note(view, id);
        };
        let (s, f) = view.review.file(key)?;
        let si = view
            .review
            .sessions
            .iter()
            .position(|x| x.index == s.index)?;
        Some((view.agents.get(si)?.clone(), f.notes.get(*note)?.clone()))
    }

    fn send_reply(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (Some((agent, note)), Some(reply)) =
            (self.find_note(id), self.replies.get(id).cloned())
        else {
            return;
        };
        let text = reply.read(cx).value().to_string();
        let root = self.root.clone();
        let sent = agent.update(cx, |a, cx| a.reply_to_stop(&note.stop, &root, text, cx));
        if sent {
            reply.update(cx, |r, cx| r.set_value("", window, cx));
            agent.update(cx, |a, cx| a.visit_stop(&note.stop.at, cx));
        }
    }

    fn set_mode(&mut self, mode: DiffMode, cx: &mut Context<Self>) {
        self.mode = mode;
        self.state.update(cx, |s, cx| s.set_mode(mode, cx));
        cx.notify();
    }

    fn scroll_to(&mut self, key: &str, line: Option<usize>, cx: &mut Context<Self>) {
        let Some(view) = self.view.clone() else {
            return;
        };
        let Some(path) = view.review_to_diff.get(key).cloned() else {
            return;
        };
        self.state.update(cx, |s, cx| match line {
            Some(l) => s.scroll_to_line(DiffLinePosition::new(path, DiffSide::Modified, l), cx),
            None => s.scroll_to_file(path.as_ref(), cx),
        });
    }

    /// Put one hunk back the way it was before pi (only if the file is still
    /// what the review shows).
    fn revert_hunk(&mut self, key: &str, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.view.clone() else {
            return;
        };
        let Some((_, f)) = view.review.file(key) else {
            return;
        };
        let Some(hunk) = f.hunks.get(ix).cloned() else {
            return;
        };
        let (path, before, now) = (f.path.clone(), f.before.clone(), f.now.clone());
        let write = cx.background_executor().spawn(async move {
            let disk = pide_core::changes::snapshot(&path).unwrap_or_default();
            if disk != now {
                return Err("the file changed since the review was shown; try again".to_string());
            }
            let text = pide_core::changes::revert_hunk(&before, &now, &hunk);
            std::fs::write(&path, text).map_err(|e| e.to_string())
        });
        cx.spawn_in(window, async move |this, cx| {
            let r = write.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Err(e) = r {
                    window.push_notification(Notification::error(format!("Not reverted: {e}")), cx);
                }
                this.rebuild(cx);
            });
        })
        .detach();
    }
}

fn note_id(key: &str, i: usize) -> String {
    format!("note:{key}:{i}")
}

/// A note in a file annotation (not on a diff line): id "note:<key>:<i>" too.
fn find_any_note(view: &View, id: &str) -> Option<(Entity<Agent>, NoteAt)> {
    let rest = id.strip_prefix("note:")?;
    let (key, i) = rest.rsplit_once(':')?;
    let (s, f) = view.review.file(key)?;
    let si = view
        .review
        .sessions
        .iter()
        .position(|x| x.index == s.index)?;
    Some((
        view.agents.get(si)?.clone(),
        f.notes.get(i.parse::<usize>().ok()?)?.clone(),
    ))
}

impl Focusable for ReviewPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl gpui_kit::component::dock::BasePanel for ReviewPanel {
    fn panel_name(&self) -> &'static str {
        PANEL_NAME
    }
    fn on_added_to(&mut self, group: WeakEntity<TabGroup>, _: &mut Window, _: &mut Context<Self>) {
        self.group = Some(group);
    }
    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let me = cx.entity_id();
        cx.global_mut::<OpenReviews>()
            .0
            .retain(|w| w.upgrade().is_some_and(|p| p.entity_id() != me));
    }
    fn dump(&self, _: &App) -> PanelState {
        PanelState::new(PANEL_NAME)
    }
}

impl gpui_kit::component::dock::Panel for ReviewPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_1p5()
            .child(diamond(pide_core::colors::pi_note(cx), 7.))
            .child(self.title_text(cx))
    }
}

impl Panel for ReviewPanel {
    fn title_text(&self, _: &App) -> SharedString {
        "Review".into()
    }
    fn default_dock(&self) -> DockPosition {
        DockPosition::Center
    }
}

/// pi's mark: a small rotated square.
fn diamond(color: Hsla, size: f32) -> Div {
    div()
        .flex_none()
        .size(px(size))
        .bg(color)
        .with_transformation_fallback()
}

trait Rotate {
    fn with_transformation_fallback(self) -> Self;
}
impl Rotate for Div {
    // GPUI can't rotate a div; a "◆" glyph would be font-dependent. A small
    // rounded square reads as pi's mark next to the serif.
    fn with_transformation_fallback(self) -> Self {
        self.rounded(px(1.5))
    }
}

/// pi's words: markdown in pi's typeface.
fn voice(view: &View, slot: &str, size: f32, fallback: &str) -> AnyElement {
    let body = match view.markdown.get(slot) {
        Some(v) => TextView::new(v)
            .markdown_extensions(pide_core::math::markdown_extensions())
            .selectable(true)
            .into_any_element(),
        None => div().child(fallback.to_string()).into_any_element(),
    };
    div()
        .w_full()
        .font_family(pide_core::colors::VOICE)
        .text_size(px(size))
        .child(body)
        .into_any_element()
}

impl ReviewPanel {
    fn render_index(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let ochre = pide_core::colors::pi_note(cx);
        let Some(view) = self.view.clone() else {
            return div().into_any_element();
        };
        let review = &view.review;
        let files_n = review.files().filter(|(_, f)| !f.reverted).count();
        let sessions_n = review
            .sessions
            .iter()
            .filter(|s| !s.files.is_empty())
            .count();
        let (seen, notes) = review.seen_counts();
        let (reviewed, all_files) = review.reviewed_counts();
        let subtitle = match (files_n, sessions_n) {
            (0, _) => "No changed files in the open sessions".to_string(),
            (f, 1) => format!("{f} changed file{}", if f == 1 { "" } else { "s" }),
            (f, s) => format!("{f} changed files in {s} sessions"),
        };
        let needs = review.needs.iter().enumerate().map(|(i, n)| {
            let n = n.clone();
            let dot = match n.kind {
                NeedKind::Question => ochre,
                NeedKind::NoReason => theme.muted_foreground,
            };
            let agent = view
                .review
                .sessions
                .iter()
                .position(|s| s.index == view.review.sessions[n.session].index)
                .and_then(|si| view.agents.get(si).cloned());
            let label = match n.kind {
                NeedKind::Question => n.text.lines().next().unwrap_or("").to_string(),
                NeedKind::NoReason => n.text.clone(),
            };
            let place = format!("{}:{}  {}", n.rel, n.line, n.session_name);
            h_flex()
                .id(("need", i))
                .items_start()
                .gap_2()
                .px_2()
                .py_1p5()
                .rounded_md()
                .cursor_pointer()
                .hover(|s| s.bg(theme.list_hover))
                .child(div().mt(px(6.)).child(diamond(dot, 6.)))
                .child(
                    v_flex()
                        .min_w_0()
                        .child(
                            div()
                                .when(n.kind == NeedKind::Question, |d| {
                                    d.font_family(pide_core::colors::VOICE).text_size(px(14.5))
                                })
                                .line_clamp(2)
                                .child(label),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .truncate()
                                .child(place),
                        ),
                )
                .on_click(
                    cx.listener(move |this, _, window, cx| match (&n.key, &n.stop) {
                        (Some(key), _) => this.scroll_to(key, Some(n.line), cx),
                        (None, Some(stop)) => {
                            if let Some(a) = &agent {
                                pide_core::collab::go_to(a, stop.clone(), window, cx);
                            }
                        }
                        _ => {}
                    }),
                )
        });
        let sessions = review.sessions.iter().enumerate().map(|(si, s)| {
            let agent = view.agents.get(si).cloned();
            let files = s.files.iter().enumerate().map(|(fi, f)| {
                let key = f.key.clone();
                let meta = if f.reverted {
                    "reverted".to_string()
                } else if f.notes.iter().any(|n| n.open) {
                    "question".to_string()
                } else if f.unseen() > 0 {
                    format!("{} new", f.unseen())
                } else if f.summary.is_none() && f.notes.is_empty() {
                    "no reason".to_string()
                } else {
                    String::new()
                };
                h_flex()
                    .id(SharedString::from(format!("idx-{si}-{fi}")))
                    .gap_2()
                    .pl_3()
                    .pr_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.list_hover))
                    .child(
                        div()
                            .w(px(12.))
                            .text_color(pide_core::colors::change(
                                pide_core::colors::Change::Added,
                                cx,
                            ))
                            .child(if f.reviewed { "✓" } else { "" }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(if f.reviewed || f.reverted {
                                theme.muted_foreground
                            } else {
                                theme.foreground
                            })
                            .child(f.rel.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(meta),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.scroll_to(&key, None, cx)))
            });
            let loose = s.loose.iter().enumerate().map(|(li, st)| {
                let st = st.clone();
                let agent = agent.clone();
                let rel = st
                    .path
                    .strip_prefix(&view.root)
                    .unwrap_or(&st.path)
                    .display()
                    .to_string();
                h_flex()
                    .id(SharedString::from(format!("loose-{si}-{li}")))
                    .gap_2()
                    .pl_3()
                    .pr_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.list_hover))
                    .child(div().w(px(12.)).child(diamond(ochre.opacity(0.7), 5.)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .child(format!("{rel}:{}", st.line)),
                    )
                    .on_click(move |_, window, cx| {
                        if let Some(a) = &agent {
                            pide_core::collab::go_to(a, st.clone(), window, cx);
                        }
                    })
            });
            v_flex()
                .gap_0p5()
                .pt_2()
                .child(
                    h_flex()
                        .gap_2()
                        .px_2()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .truncate()
                                .child(s.name.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(if s.busy {
                                    pide_core::colors::accent(cx)
                                } else {
                                    theme.muted_foreground
                                })
                                .child(if s.busy { "pi is working" } else { "" }),
                        ),
                )
                .children(files)
                .when(!s.loose.is_empty(), |d| {
                    d.child(
                        div()
                            .px_2()
                            .pt_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Notes on other files"),
                    )
                    .children(loose)
                })
        });
        let pct = if notes == 0 {
            1.0
        } else {
            seen as f32 / notes as f32
        };
        v_flex()
            .w(px(272.))
            .flex_none()
            .h_full()
            .border_r_1()
            .border_color(theme.border)
            .bg(theme.sidebar)
            .child(
                v_flex()
                    .px_4()
                    .pt_4()
                    .pb_3()
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Review"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(subtitle),
                    ),
            )
            .child(
                v_flex()
                    .id("review-index")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .px_2()
                    .when(!review.needs.is_empty(), |d| {
                        d.child(
                            div()
                                .px_2()
                                .pb_1()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(ochre)
                                .child("Needs you"),
                        )
                        .children(needs)
                        .child(div().h(px(1.)).mx_2().my_2().bg(theme.border))
                    })
                    .children(sessions)
                    .child(div().h_4()),
            )
            .child(
                v_flex()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        h_flex()
                            .justify_between()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("{seen} of {notes} notes seen"))
                            .child(format!("{reviewed} of {all_files} files reviewed")),
                    )
                    .child(
                        div()
                            .h(px(3.))
                            .w_full()
                            .rounded_full()
                            .bg(theme.border)
                            .child(div().h_full().w(relative(pct)).rounded_full().bg(ochre)),
                    )
                    .child(
                        Button::new("review-walk")
                            .small()
                            .w_full()
                            .label("Walk through in the editor")
                            .on_click(|_, window, cx| {
                                pide_core::collab::review_next(window, cx);
                            }),
                    ),
            )
            .into_any_element()
    }

    fn render_diff(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(view) = self.view.clone() else {
            return div().into_any_element();
        };
        // Reply boxes for every note (made here, where there's a window).
        let ids: Vec<String> = view
            .review
            .files()
            .flat_map(|(_, f)| {
                (0..f.notes.len())
                    .map(|i| note_id(&f.key, i))
                    .collect::<Vec<_>>()
            })
            .collect();
        let mut replies = HashMap::new();
        for id in ids {
            let r = self.reply_box(&id, window, cx);
            replies.insert(id, r);
        }
        let view = Rc::new(View {
            review: view.review.clone(),
            by_diff_path: view.by_diff_path.clone(),
            review_to_diff: view.review_to_diff.clone(),
            anns: view.anns.clone(),
            agents: view.agents.clone(),
            root: view.root.clone(),
            replies,
            markdown: self.markdown.clone(),
            panel: view.panel.clone(),
        });
        let annotations: Vec<DiffAnnotation> = view
            .review
            .files()
            .filter_map(|(_, f)| Some((f, view.review_to_diff.get(&f.key)?.clone())))
            .flat_map(|(f, path)| {
                let mut out = vec![DiffAnnotation::file(
                    SharedString::from(format!("file:{}", f.key)),
                    path.clone(),
                )];
                for (i, n) in f.notes.iter().enumerate().filter(|(_, n)| n.in_diff) {
                    out.push(DiffAnnotation::line(
                        SharedString::from(note_id(&f.key, i)),
                        DiffLinePosition::new(path.clone(), DiffSide::Modified, n.line),
                    ));
                }
                out
            })
            .collect();
        let markers: Vec<DiffGutterMarker> = view
            .review
            .files()
            .filter_map(|(_, f)| Some((f, view.review_to_diff.get(&f.key)?.clone())))
            .flat_map(|(f, path)| {
                f.hunks
                    .iter()
                    .enumerate()
                    .map(|(i, h)| {
                        let pos = if h.after.is_empty() {
                            DiffLinePosition::new(
                                path.clone(),
                                DiffSide::Original,
                                h.before.start + 1,
                            )
                        } else {
                            DiffLinePosition::new(
                                path.clone(),
                                DiffSide::Modified,
                                h.after.start + 1,
                            )
                        };
                        DiffGutterMarker::new(SharedString::from(format!("rv:{}:{i}", f.key)), pos)
                            .with_label("Revert this change")
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        let v_ann = view.clone();
        let v_head = view.clone();
        let v_title = view.clone();
        let panel = cx.weak_entity();
        Diff::new(&self.state)
            .line_number(true)
            .syntax_highlight(true)
            .soft_wrap(true)
            .annotations(annotations)
            .render_annotation(move |ann, window, cx| render_ann(&v_ann, ann, window, cx))
            .render_header_title_suffix(move |file, _, cx| {
                let Some((s, _)) = v_title
                    .by_diff_path
                    .get(file.path())
                    .and_then(|k| v_title.review.file(k))
                else {
                    return div().into_any_element();
                };
                let multi = v_title.review.sessions.len() > 1;
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .when(multi, |d| d.child(s.name.clone()))
                    .into_any_element()
            })
            .render_header_suffix(move |file, _, cx| render_header(&v_head, file, cx))
            .gutter_markers(markers)
            .render_gutter_marker(move |marker, _, _| {
                let id = marker.id().to_string();
                let parsed = id
                    .strip_prefix("rv:")
                    .and_then(|r| r.rsplit_once(':'))
                    .map(|(k, i)| (k.to_string(), i.parse::<usize>().unwrap_or(0)));
                let panel = panel.clone();
                let label = parsed
                    .as_ref()
                    .map(|(_, i)| format!("{} ↶", i + 1))
                    .unwrap_or_default();
                Button::new(marker.id().clone())
                    .ghost()
                    .xsmall()
                    .label(label)
                    .tooltip(marker.label().clone())
                    .on_click(move |_, window, cx| {
                        let Some((key, ix)) = parsed.clone() else {
                            return;
                        };
                        let panel = panel.clone();
                        window.defer(cx, move |window, cx| {
                            let _ = panel.update(cx, |p, cx| p.revert_hunk(&key, ix, window, cx));
                        });
                    })
            })
            .size_full()
            .text_color(theme.foreground)
            .into_any_element()
    }
}

/// Reviewed / Revert / Open on a file's header.
fn render_header(view: &Rc<View>, file: &DiffFile, cx: &mut App) -> AnyElement {
    let Some(key) = view.by_diff_path.get(file.path()).cloned() else {
        return div().into_any_element();
    };
    let Some((s, f)) = view.review.file(&key) else {
        return div().into_any_element();
    };
    let Some(si) = view.review.sessions.iter().position(|x| x.index == s.index) else {
        return div().into_any_element();
    };
    let Some(agent) = view.agents.get(si).cloned() else {
        return div().into_any_element();
    };
    let reviewed = f.reviewed;
    let path = f.path.clone();
    let (a_rev, a_revert, p_rev, p_revert, p_open) = (
        agent.clone(),
        agent.clone(),
        path.clone(),
        path.clone(),
        path.clone(),
    );
    let first = f.hunks.first().map(|h| h.after.start).unwrap_or(0);
    h_flex()
        .gap_1()
        .child(
            Button::new(SharedString::from(format!("open:{key}")))
                .xsmall()
                .ghost()
                .label("Open")
                .tooltip("Open the file at its first change")
                .on_click(move |_, window, cx| {
                    pide_core::opener::open_file_at(p_open.clone(), first as u32, 0, window, cx)
                }),
        )
        .child(
            Button::new(SharedString::from(format!("revert:{key}")))
                .xsmall()
                .ghost()
                .label("Revert file")
                .on_click(move |_, window, cx| {
                    let task = a_revert.update(cx, |a, cx| a.revert(&p_revert, cx));
                    let _ = window;
                    cx.spawn(async move |_| {
                        if let Err(e) = task.await {
                            log::warn!("revert: {e}");
                        }
                    })
                    .detach();
                }),
        )
        .child(
            Button::new(SharedString::from(format!("reviewed:{key}")))
                .xsmall()
                .label(if reviewed {
                    "Reviewed ✓"
                } else {
                    "Mark reviewed"
                })
                .map(|b| pide_core::colors::toggle(b, reviewed, cx))
                .on_click(move |_, _, cx| {
                    a_rev.update(cx, |a, cx| a.set_reviewed(&p_rev, !reviewed, cx))
                }),
        )
        .into_any_element()
}

fn render_ann(view: &Rc<View>, ann: &DiffAnnotation, _: &mut Window, cx: &mut App) -> AnyElement {
    let id = ann.id().to_string();
    match view.anns.get(&id) {
        Some(Ann::File { key }) => render_file_ann(view, key, cx),
        Some(Ann::Note { key, note }) => render_note(view, key, *note, cx),
        None => div().into_any_element(),
    }
}

/// Under a file's header: pi's reason, or Ask pi why; then notes that aren't
/// on a line the diff shows.
fn render_file_ann(view: &Rc<View>, key: &str, cx: &mut App) -> AnyElement {
    let theme = cx.theme().clone();
    let Some((s, f)) = view.review.file(key) else {
        return div().into_any_element();
    };
    let si = view.review.sessions.iter().position(|x| x.index == s.index);
    let agent = si.and_then(|si| view.agents.get(si).cloned());
    let has_reason = f.summary.is_some()
        || f.notes
            .iter()
            .any(|n| !matches!(n.stop.at, StopRef::File(_)));
    let off: Vec<usize> = f
        .notes
        .iter()
        .enumerate()
        .filter(|(_, n)| !n.in_diff)
        .map(|(i, _)| i)
        .collect();
    let rel = f.rel.clone();
    let status = if f.reverted {
        Some("You reverted this file.")
    } else if f.deleted {
        Some("pi deleted this file.")
    } else if f.patch.is_empty() {
        Some("No differences any more: the file matches how it was before pi.")
    } else {
        None
    };
    v_flex()
        .w_full()
        .gap_2()
        .px_4()
        .py_2()
        .when_some(f.summary.clone(), |d, s| {
            d.child(
                h_flex()
                    .items_start()
                    .gap_2()
                    .child(
                        div()
                            .mt(px(8.))
                            .child(diamond(pide_core::colors::pi_note(cx), 7.)),
                    )
                    .child(div().flex_1().min_w_0().max_w(px(760.)).child(voice(
                        view,
                        &format!("sum:{key}"),
                        17.,
                        &s,
                    ))),
            )
        })
        .when(!has_reason && !f.reverted, |d| {
            let agent = agent.clone();
            d.child(
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child("pi changed this file without saying why."),
                    )
                    .child(
                        Button::new(SharedString::from(format!("why:{key}")))
                            .xsmall()
                            .label("Ask pi why")
                            .on_click(move |_, _, cx| {
                                if let Some(a) = &agent {
                                    let prompt = pide_core::review::ask_why_prompt(
                                        std::slice::from_ref(&rel),
                                    );
                                    a.update(cx, |a, cx| {
                                        if a.is_busy() {
                                            a.queue(prompt, true, cx);
                                        } else {
                                            a.prompt(prompt, cx);
                                        }
                                    });
                                }
                            }),
                    ),
            )
        })
        .when_some(status, |d, s| {
            d.child(div().text_xs().text_color(theme.muted_foreground).child(s))
        })
        .children(off.into_iter().map(|i| render_note(view, key, i, cx)))
        .into_any_element()
}

/// One note: pi's words, the thread, a reply box and its actions.
fn render_note(view: &Rc<View>, key: &str, i: usize, cx: &mut App) -> AnyElement {
    let theme = cx.theme().clone();
    let ochre = pide_core::colors::pi_note(cx);
    let Some((s, f)) = view.review.file(key) else {
        return div().into_any_element();
    };
    let Some(n) = f.notes.get(i).cloned() else {
        return div().into_any_element();
    };
    let si = view.review.sessions.iter().position(|x| x.index == s.index);
    let Some(agent) = si.and_then(|si| view.agents.get(si).cloned()) else {
        return div().into_any_element();
    };
    let id = note_id(key, i);
    let busy = agent.read(cx).is_busy();
    let answering = agent
        .read(cx)
        .answering
        .as_ref()
        .is_some_and(|(at, _)| *at == n.stop.at);
    let is_file_stop = matches!(n.stop.at, StopRef::File(_));
    let reply = view.replies.get(&id).cloned();
    let empty = reply
        .as_ref()
        .is_none_or(|r| r.read(cx).value().trim().is_empty());
    let stop: Stop = n.stop.clone();
    let where_ = if n.in_diff {
        String::new()
    } else {
        format!("line {}", n.line)
    };
    let thread = n.stop.thread.iter().enumerate().map(|(j, r)| {
        let body = if r.pi {
            voice(view, &format!("{id}#{}", j + 1), 15.5, &r.text)
        } else {
            div().w_full().child(r.text.clone()).into_any_element()
        };
        h_flex()
            .items_start()
            .gap_3()
            .child(
                div()
                    .w(px(36.))
                    .flex_none()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if r.pi { ochre } else { theme.foreground })
                    .child(if r.pi { "pi" } else { "You" }),
            )
            .child(div().flex_1().min_w_0().max_w(px(700.)).child(body))
    });
    let (a_send, a_resolve, a_open, a_seen) =
        (agent.clone(), agent.clone(), agent.clone(), agent.clone());
    let (s_send, s_resolve, s_open, s_seen) =
        (stop.clone(), stop.clone(), stop.clone(), stop.clone());
    let root = view.root.clone();
    let reply_for_send = reply.clone();
    v_flex()
        .w_full()
        .gap_1p5()
        .ml(px(18.))
        .my_1()
        .pl_4()
        .pr_4()
        .py_2()
        .border_l_2()
        .border_color(ochre.opacity(if n.stop.seen { 0.35 } else { 0.9 }))
        .bg(ochre.opacity(0.04))
        .child(
            h_flex()
                .items_start()
                .gap_2()
                .when(!is_file_stop, |d| {
                    d.child(div().flex_1().min_w_0().max_w(px(760.)).child(voice(
                        view,
                        &format!("{id}#0"),
                        16.,
                        &n.stop.note,
                    )))
                })
                .when(is_file_stop, |d| {
                    d.child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Replies about the whole file"),
                    )
                })
                .when(!where_.is_empty(), |d| {
                    d.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(where_),
                    )
                })
                .when(!n.stop.seen, |d| {
                    d.child(
                        div()
                            .flex_none()
                            .px_1p5()
                            .rounded(px(8.))
                            .bg(ochre)
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.background)
                            .child(if n.open { "Question" } else { "New" }),
                    )
                })
                .when(n.stop.outdated, |d| {
                    d.child(
                        div()
                            .text_xs()
                            .text_color(ochre)
                            .child("outdated: the code under it changed"),
                    )
                }),
        )
        .children(thread)
        .when(answering && busy, |d| {
            d.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("pi is answering…"),
            )
        })
        .when_some(reply, |d, r| {
            d.child(div().max_w(px(640.)).mt_1().child(Textarea::new(&r)))
        })
        .child(
            h_flex()
                .gap_1()
                .child(
                    Button::new(SharedString::from(format!("{id}:open")))
                        .xsmall()
                        .ghost()
                        .label("Open in editor")
                        .on_click(move |_, window, cx| {
                            pide_core::collab::go_to(&a_open, s_open.clone(), window, cx)
                        }),
                )
                .when(!n.stop.seen, |d| {
                    d.child(
                        Button::new(SharedString::from(format!("{id}:seen")))
                            .xsmall()
                            .ghost()
                            .label("Mark seen")
                            .on_click(move |_, _, cx| {
                                a_seen.update(cx, |a, cx| a.visit_stop(&s_seen.at, cx))
                            }),
                    )
                })
                .when(!is_file_stop, |d| {
                    d.child(
                        Button::new(SharedString::from(format!("{id}:resolve")))
                            .xsmall()
                            .ghost()
                            .label("Resolve")
                            .tooltip("Hide this note")
                            .on_click(move |_, _, cx| {
                                a_resolve.update(cx, |a, cx| a.dismiss_stop(&s_resolve.at, cx))
                            }),
                    )
                })
                .child(div().flex_1())
                .when(busy, |d| {
                    d.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("pi is busy"),
                    )
                })
                .when(!empty, |d| {
                    d.child(
                        Button::new(SharedString::from(format!("{id}:send")))
                            .xsmall()
                            .primary()
                            .label("Send")
                            .disabled(busy)
                            .on_click(move |_, window, cx| {
                                let Some(r) = reply_for_send.clone() else {
                                    return;
                                };
                                let text = r.read(cx).value().to_string();
                                let sent = a_send
                                    .update(cx, |a, cx| a.reply_to_stop(&s_send, &root, text, cx));
                                if sent {
                                    r.update(cx, |r, cx| r.set_value("", window, cx));
                                    a_send.update(cx, |a, cx| a.visit_stop(&s_send.at, cx));
                                }
                            }),
                    )
                }),
        )
        .into_any_element()
}

impl Render for ReviewPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let split = self.mode == DiffMode::Split;
        let empty = self.view.as_ref().is_some_and(|v| v.review.is_empty());
        let busy_names: Vec<String> = self
            .view
            .as_ref()
            .map(|v| {
                v.review
                    .sessions
                    .iter()
                    .filter(|s| s.busy)
                    .map(|s| s.name.clone())
                    .collect()
            })
            .unwrap_or_default();
        let any_busy = self
            .sessions
            .read(cx)
            .all()
            .iter()
            .any(|a| a.read(cx).is_busy());
        let diff = if empty || self.view.is_none() {
            None
        } else {
            Some(self.render_diff(window, cx))
        };
        h_flex()
            .size_full()
            .track_focus(&self.focus)
            .when(!empty && self.view.is_some(), |d| d.child(self.render_index(cx)))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(
                        h_flex()
                            .gap_2()
                            .px_4()
                            .py_2()
                            .border_b_1()
                            .border_color(theme.border)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(if any_busy {
                                        "pi is still changing files. New changes appear here when it stops, so the page doesn't move while you read."
                                            .to_string()
                                    } else if !busy_names.is_empty() {
                                        String::new()
                                    } else {
                                        "What pi changed and said in the open sessions".to_string()
                                    }),
                            )
                            .child(
                                Button::new("review-inline")
                                    .small()
                                    .label("Inline")
                                    .map(|b| pide_core::colors::toggle(b, !split, cx))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_mode(DiffMode::Unified, cx)
                                    })),
                            )
                            .child(
                                Button::new("review-split")
                                    .small()
                                    .label("Side by side")
                                    .map(|b| pide_core::colors::toggle(b, split, cx))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_mode(DiffMode::Split, cx)
                                    })),
                            )
                            .child(
                                Button::new("review-refresh")
                                    .small()
                                    .ghost()
                                    .label("Refresh")
                                    .on_click(cx.listener(|this, _, _, cx| this.rebuild(cx))),
                            ),
                    )
                    .when(self.loading && self.view.is_none(), |d| {
                        d.child(
                            div()
                                .p_4()
                                .text_color(theme.muted_foreground)
                                .child("Reading the sessions' changes…"),
                        )
                    })
                    .when(empty, |d| {
                        d.child(
                            v_flex()
                                .flex_1()
                                .items_center()
                                .justify_center()
                                .gap_2()
                                .child(div().text_base().child("Nothing to review yet"))
                                .child(
                                    div()
                                        .text_color(theme.muted_foreground)
                                        .child("When pi changes code in an open session, its changes and notes collect here."),
                                ),
                        )
                    })
                    .when_some(diff, |d, diff| {
                        d.child(div().flex_1().min_h_0().overflow_hidden().child(diff))
                    }),
            )
    }
}
