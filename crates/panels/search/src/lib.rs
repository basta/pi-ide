//! Find in Files (Cmd+Shift+F, spec M4): search the project (text or regex,
//! case, whole word; gitignore-aware), results by file, click to jump, and
//! Replace All after a confirmation. The search runs off the UI thread; open
//! buffers are searched as they are in the editor, unsaved edits included.

pub mod engine;
pub mod locations;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dock::{PanelEvent, PanelState, panel_handle, register_panel};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::{DockPosition, Panel, Services, Workspace};

use engine::{Query, Results};

pub const PANEL_NAME: &str = "find_in_files";

pub fn init(cx: &mut App) {
    locations::init(cx);
    register_panel(cx, PANEL_NAME, |_, window, cx| {
        panel_handle(cx.new(|cx| FindInFiles::new(window, cx)))
    });
    pide_core::toolwindow::register(
        cx,
        pide_core::ToolWindowSpec {
            id: PANEL_NAME,
            title: "Find",
            icon: gpui_kit::component::IconName::Search,
            dock: pide_core::DockPosition::Bottom,
            command: "view.find",
            key: Some("cmd-3"),
            create: |window, cx| panel_handle(cx.new(|cx| FindInFiles::new(window, cx))),
        },
    );
}

#[derive(Default)]
struct Instances(HashMap<WindowId, WeakEntity<FindInFiles>>);
impl Global for Instances {}

pub fn existing(window: &Window, cx: &App) -> Option<Entity<FindInFiles>> {
    cx.try_global::<Instances>()?
        .0
        .get(&window.window_handle().window_id())?
        .upgrade()
}

pub struct FindInFiles {
    workspace: Entity<Workspace>,
    root: PathBuf,
    find: Entity<InputState>,
    replace: Entity<InputState>,
    /// File mask (`*.rs, *.toml`, `!*.lock`).
    mask: Entity<InputState>,
    regex: bool,
    case_sensitive: bool,
    whole_word: bool,
    results: Option<Result<Results, String>>,
    searching: bool,
    /// The query the results are for (Replace All uses exactly this).
    searched: Option<Query>,
    focus: FocusHandle,
    _search: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<PanelEvent> for FindInFiles {}

impl FindInFiles {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let workspace = Services::of(window, cx).workspace;
        let root = workspace.read(cx).root().to_path_buf();
        let me = cx.weak_entity();
        cx.default_global::<Instances>()
            .0
            .insert(window.window_handle().window_id(), me);
        let find = cx.new(|cx| InputState::new(window, cx).placeholder("Find in files (Enter)"));
        let replace = cx.new(|cx| InputState::new(window, cx).placeholder("Replace with"));
        let mask = cx.new(|cx| InputState::new(window, cx).placeholder("File mask: *.rs, !*.lock"));
        let rerun = |this: &mut Self,
                     _: &Entity<InputState>,
                     ev: &InputEvent,
                     _: &mut Window,
                     cx: &mut Context<Self>| match ev {
            InputEvent::PressEnter { .. } => this.run(Duration::ZERO, cx),
            InputEvent::Change => this.run(Duration::from_millis(350), cx),
            _ => {}
        };
        let subs = vec![
            cx.subscribe_in(&find, window, rerun),
            cx.subscribe_in(&mask, window, rerun),
        ];
        Self {
            workspace,
            root,
            find,
            replace,
            mask,
            regex: false,
            case_sensitive: false,
            whole_word: false,
            results: None,
            searching: false,
            searched: None,
            focus: cx.focus_handle(),
            _search: None,
            _subs: subs,
        }
    }

    /// Put `text` in the search box and focus it (Cmd+Shift+F with a selection).
    pub fn focus_query(
        &mut self,
        text: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.find.update(cx, |i, cx| {
            if let Some(t) = text.filter(|t| !t.is_empty() && !t.contains('\n')) {
                i.set_value(t, window, cx);
            }
            i.focus(window, cx);
            i.select_all(window, cx);
        });
        self.run(Duration::ZERO, cx);
    }

    fn query(&self, cx: &App) -> Query {
        Query {
            text: self.find.read(cx).value().to_string(),
            regex: self.regex,
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
            mask: self.mask.read(cx).value().to_string(),
        }
    }

    fn run(&mut self, delay: Duration, cx: &mut Context<Self>) {
        let query = self.query(cx);
        if query.text.is_empty() {
            self.results = None;
            self.searched = None;
            self._search = None;
            cx.notify();
            return;
        }
        // Open buffers as they are now (unsaved edits count).
        let open: HashMap<PathBuf, String> = self
            .workspace
            .read(cx)
            .buffers()
            .iter()
            .map(|b| {
                let b = b.read(cx);
                (b.path.clone(), b.text().to_string())
            })
            .collect();
        let root = self.root.clone();
        self.searching = true;
        cx.notify();
        self._search = Some(cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let q = query.clone();
            let results = cx
                .background_executor()
                .spawn(async move { engine::search(&root, &q, &open) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.results = Some(results);
                this.searched = Some(query);
                this.searching = false;
                cx.notify();
            });
        }));
    }

    fn toggle(&mut self, which: u8, cx: &mut Context<Self>) {
        match which {
            0 => self.case_sensitive = !self.case_sensitive,
            1 => self.whole_word = !self.whole_word,
            _ => self.regex = !self.regex,
        }
        self.run(Duration::ZERO, cx);
    }

    fn confirm_replace_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(query), Some(Ok(results))) = (self.searched.clone(), self.results.as_ref())
        else {
            return;
        };
        if results.total == 0 {
            return;
        }
        let replacement = self.replace.read(cx).value().to_string();
        let files: Vec<PathBuf> = results.files.iter().map(|f| f.path.clone()).collect();
        let (n, m) = (results.total, files.len());
        let me = cx.entity().downgrade();
        let shown = if replacement.is_empty() {
            "nothing (delete them)".to_string()
        } else {
            format!("“{replacement}”")
        };
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (me, query, files, replacement) = (me.clone(), query.clone(), files.clone(), replacement.clone());
            alert
                .title(format!("Replace {n} match{} in {m} file{}?", if n == 1 { "" } else { "es" }, if m == 1 { "" } else { "s" }))
                .description(format!(
                    "“{}” becomes {shown}. Files open in the editor change there (undo works); others are written to disk.",
                    query.text
                ))
                .confirm()
                .ok_text("Replace All")
                .on_ok(move |_, window, cx| {
                    let (query, files, replacement) = (query.clone(), files.clone(), replacement.clone());
                    let _ = me.update(cx, |this, cx| this.replace_all(query, files, replacement, window, cx));
                    true
                })
        });
    }

    fn replace_all(
        &mut self,
        query: Query,
        files: Vec<PathBuf>,
        replacement: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut on_disk = Vec::new();
        let mut replaced = 0;
        let mut errors = Vec::new();
        for path in files {
            let buffer = self.workspace.read(cx).buffer_for(&path, cx);
            match buffer {
                Some(buffer) => {
                    let text = buffer.read(cx).text().to_string();
                    match engine::replace_all(&query, &text, &replacement) {
                        Ok((new, n)) if n > 0 => {
                            replaced += n;
                            self.workspace.update(cx, |w, cx| {
                                w.replace_text(
                                    &buffer,
                                    gpui_kit::component::input::Rope::from(new.as_str()),
                                    cx,
                                );
                                w.save(&buffer, cx).detach_and_log_err(cx);
                            });
                        }
                        Ok(_) => {}
                        Err(e) => errors.push(e),
                    }
                }
                None => on_disk.push(path),
            }
        }
        let task = cx.background_executor().spawn(async move {
            let mut n = 0;
            let mut errs = Vec::new();
            for p in on_disk {
                match engine::replace_in_file(&p, &query, &replacement) {
                    Ok(k) => n += k,
                    Err(e) => errs.push(e),
                }
            }
            (n, errs)
        });
        self._search = Some(cx.spawn_in(window, async move |this, cx| {
            let (n, errs) = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                errors.extend(errs);
                let total = replaced + n;
                let note = if errors.is_empty() {
                    Notification::info(format!(
                        "Replaced {total} match{}.",
                        if total == 1 { "" } else { "es" }
                    ))
                } else {
                    Notification::error(format!("Replaced {total}; failed: {}", errors.join("; ")))
                };
                window.push_notification(note, cx);
                this.run(Duration::from_millis(200), cx);
            });
        }));
    }
}

impl Focusable for FindInFiles {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl gpui_kit::component::dock::BasePanel for FindInFiles {
    fn panel_name(&self) -> &'static str {
        PANEL_NAME
    }
    fn dump(&self, _: &App) -> PanelState {
        PanelState::new(PANEL_NAME)
    }
}

impl gpui_kit::component::dock::Panel for FindInFiles {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.title_text(cx)
    }
}

impl Panel for FindInFiles {
    fn title_text(&self, _: &App) -> SharedString {
        "Find".into()
    }
    fn default_dock(&self) -> DockPosition {
        DockPosition::Bottom
    }
}

impl Render for FindInFiles {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let toggle = |id: &'static str,
                      label: &'static str,
                      on: bool,
                      tip: &'static str,
                      which: u8,
                      cx: &mut Context<Self>| {
            Button::new(id)
                .xsmall()
                .label(label)
                .when(on, |b| b.primary())
                .tooltip(tip)
                .on_click(cx.listener(move |this, _, _, cx| this.toggle(which, cx)))
        };
        let summary = match (&self.results, self.searching) {
            (_, true) => "Searching…".to_string(),
            (None, _) => String::new(),
            (Some(Err(e)), _) => format!("Invalid pattern: {e}"),
            (Some(Ok(r)), _) => format!(
                "{}{} match{} in {} file{}{}",
                if r.truncated { "first " } else { "" },
                r.total,
                if r.total == 1 { "" } else { "es" },
                r.files.len(),
                if r.files.len() == 1 { "" } else { "s" },
                if r.truncated {
                    " (refine the search)"
                } else {
                    ""
                }
            ),
        };
        let can_replace = matches!(&self.results, Some(Ok(r)) if r.total > 0) && !self.searching;

        let mut rows: Vec<AnyElement> = Vec::new();
        if let Some(Ok(results)) = &self.results {
            for f in &results.files {
                let rel = f
                    .path
                    .strip_prefix(&self.root)
                    .unwrap_or(&f.path)
                    .display()
                    .to_string();
                rows.push(
                    h_flex()
                        .gap_2()
                        .px_3()
                        .pt_1p5()
                        .text_sm()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(rel))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(f.matches.len().to_string()),
                        )
                        .into_any_element(),
                );
                for (i, m) in f.matches.iter().enumerate().take(200) {
                    // The line around the match, trimmed so the match stays visible.
                    let start = m.text[..m.range.start]
                        .char_indices()
                        .rev()
                        .nth(40)
                        .map(|(b, _)| b)
                        .unwrap_or(0);
                    let before = m.text[start..m.range.start].trim_start().to_string();
                    let hit = m.text[m.range.clone()].to_string();
                    let after = m.text[m.range.end..].chars().take(160).collect::<String>();
                    let (path, line, ch) = (f.path.clone(), m.line, m.character);
                    rows.push(
                        h_flex()
                            .id(SharedString::from(format!("r-{}-{i}", f.path.display())))
                            .gap_2()
                            .pl_6()
                            .pr_3()
                            .py_0p5()
                            .text_sm()
                            .cursor_pointer()
                            .hover(|s| s.bg(theme.list_hover))
                            .on_click(move |_, window, cx| {
                                pide_core::opener::open_file_at(path.clone(), line, ch, window, cx)
                            })
                            .child(
                                div()
                                    .w(px(44.))
                                    .flex_none()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child((line + 1).to_string()),
                            )
                            .child(
                                h_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .font_family(theme.mono_font_family.clone())
                                    .text_xs()
                                    .child(before)
                                    .child(
                                        div()
                                            .bg(theme.warning.opacity(0.35))
                                            .text_color(theme.foreground)
                                            .child(hit),
                                    )
                                    .child(after),
                            )
                            .into_any_element(),
                    );
                }
                if f.matches.len() > 200 {
                    rows.push(
                        div()
                            .pl_6()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("… {} more in this file", f.matches.len() - 200))
                            .into_any_element(),
                    );
                }
            }
        }

        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .child(
                v_flex()
                    .gap_1()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        h_flex()
                            .gap_1()
                            .child(div().flex_1().child(Input::new(&self.find).small()))
                            .child(
                                div()
                                    .w(px(150.))
                                    .flex_none()
                                    .child(Input::new(&self.mask).small()),
                            )
                            .child(toggle(
                                "case",
                                "Aa",
                                self.case_sensitive,
                                "Match case",
                                0,
                                cx,
                            ))
                            .child(toggle("word", "W", self.whole_word, "Whole words", 1, cx))
                            .child(toggle(
                                "regex",
                                ".*",
                                self.regex,
                                "Regular expression",
                                2,
                                cx,
                            )),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(div().flex_1().child(Input::new(&self.replace).small()))
                            .child(
                                Button::new("replace-all")
                                    .xsmall()
                                    .label("Replace All")
                                    .disabled(!can_replace)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.confirm_replace_all(window, cx)
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(summary),
                    ),
            )
            .child(
                div()
                    .id("find-results")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .pb_2()
                    .children(rows),
            )
    }
}
