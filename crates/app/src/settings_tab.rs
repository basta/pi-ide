//! The Settings tab (Cmd+,): every setting in pide in one place. Pages come
//! from the crates that own them (`pide_core::settings_page`); this tab lists
//! them on the left, searches them, and switches between Everyone and This
//! project for pages that have both. Changes apply at once.

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::component::button::Button;
use gpui_kit::component::dock::{PanelEvent, PanelInfo, PanelState, panel_handle, register_panel};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::settings_page::{self, Scope, SettingsHost, SettingsPage};
use pide_core::{CommandRegistry, CommandSpec, DockPosition, Panel};

pub const PANEL_NAME: &str = "settings";
/// The tabs it replaced, in layouts saved before: they come back as this tab.
const OLD_TABS: &[(&str, &str)] = &[("pi_settings", "pi.packages"), ("language_servers", "lsp")];

pub fn init(cx: &mut App) {
    register_panel(cx, PANEL_NAME, |ctx, window, cx| {
        let page = match ctx.info() {
            PanelInfo::Panel(v) => v.get("page").and_then(|p| p.as_str()).map(str::to_string),
            _ => None,
        };
        panel_handle(cx.new(|cx| SettingsTab::new(page.as_deref(), window, cx)))
    });
    for &(old, page) in OLD_TABS {
        register_panel(cx, old, move |_, window, cx| {
            panel_handle(cx.new(|cx| SettingsTab::new(Some(page), window, cx)))
        });
    }
    cx.set_global(SettingsHost {
        open: Rc::new(open),
    });
    CommandRegistry::register(
        cx,
        CommandSpec::new("settings.pide", "Settings…")
            .key("cmd-,")
            .over_terminal(),
        |window, cx| open(None, window, cx),
    );
    CommandRegistry::register(cx, CommandSpec::new("pi.settings", "Settings: pi…"), |window, cx| {
        open(Some("pi.packages"), window, cx)
    });
    CommandRegistry::register(
        cx,
        CommandSpec::new("lsp.servers", "Settings: Language Servers…"),
        |window, cx| open(Some("lsp"), window, cx),
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("settings.tool_windows", "Settings: Tool Windows…"),
        |window, cx| open(Some("tool_windows"), window, cx),
    );
}

#[derive(Default)]
struct Instances(HashMap<WindowId, WeakEntity<SettingsTab>>);
impl Global for Instances {}

/// Show the Settings tab in `window` (on `page`, or where it was).
pub fn open(page: Option<&'static str>, window: &mut Window, cx: &mut App) {
    let id = window.window_handle().window_id();
    let tab = cx
        .try_global::<Instances>()
        .and_then(|i| i.0.get(&id)?.upgrade());
    let tab = match tab {
        Some(tab) => {
            if let Some(page) = page {
                tab.update(cx, |t, cx| t.show(page, window, cx));
            }
            tab
        }
        None => cx.new(|cx| SettingsTab::new(page, window, cx)),
    };
    let entity = tab.entity_id();
    pide_core::opener::open_panel(panel_handle(tab.clone()), entity, window, cx);
    let focus = tab.read(cx).search.read(cx).focus_handle(cx);
    window.defer(cx, move |window, cx| focus.focus(window, cx));
}

pub struct SettingsTab {
    page: &'static str,
    views: HashMap<&'static str, AnyView>,
    search: Entity<InputState>,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<PanelEvent> for SettingsTab {}

impl SettingsTab {
    fn new(page: Option<&str>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let me = cx.weak_entity();
        cx.default_global::<Instances>()
            .0
            .insert(window.window_handle().window_id(), me);
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search settings"));
        let subs = vec![cx.subscribe(&search, |_, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        })];
        let pages = settings_page::all(cx);
        let page = page
            .and_then(|id| pages.iter().find(|p| p.id == id))
            .or(pages.first())
            .map(|p| p.id)
            .unwrap_or("");
        Self {
            page,
            views: HashMap::new(),
            search,
            focus: cx.focus_handle(),
            _subs: subs,
        }
    }

    fn show(&mut self, id: &str, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = settings_page::all(cx).into_iter().find(|p| p.id == id) {
            self.page = p.id;
            cx.notify();
        }
    }

    fn view(&mut self, page: &SettingsPage, window: &mut Window, cx: &mut App) -> AnyView {
        self.views
            .entry(page.id)
            .or_insert_with(|| (page.create)(window, cx))
            .clone()
    }

    fn render_nav(&self, pages: &[SettingsPage], query: &str, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let mut nav = v_flex().gap_0p5();
        let mut group: Option<&str> = None;
        let mut any = false;
        for page in pages {
            let Some(hit) = settings_page::matches(page, query) else {
                continue;
            };
            any = true;
            if page.group != group {
                group = page.group;
                if let Some(g) = group {
                    nav = nav.child(
                        div()
                            .px_2()
                            .pt_3()
                            .pb_1()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.muted_foreground)
                            .child(g.to_uppercase()),
                    );
                }
            }
            let id = page.id;
            let active = self.page == id;
            nav = nav.child(
                v_flex()
                    .id(SharedString::from(format!("settings-nav-{id}")))
                    .px_2()
                    .py_1()
                    .when(page.group.is_some(), |t| t.pl_4())
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.list_hover))
                    .when(active, |s| s.bg(theme.list_active).font_weight(FontWeight::SEMIBOLD))
                    .child(page.title)
                    .when_some(hit, |t, k| {
                        t.child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::NORMAL)
                                .text_color(theme.muted_foreground)
                                .child(k),
                        )
                    })
                    .on_click(cx.listener(move |this, _, window, cx| this.show(id, window, cx))),
            );
        }
        if !any {
            nav = nav.child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Nothing found"),
            );
        }
        nav
    }
}

impl Focusable for SettingsTab {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl gpui_kit::component::dock::BasePanel for SettingsTab {
    fn panel_name(&self) -> &'static str {
        PANEL_NAME
    }
    fn dump(&self, _: &App) -> PanelState {
        let mut state = PanelState::new(PANEL_NAME);
        state.info = PanelInfo::panel(serde_json::json!({ "page": self.page }));
        state
    }
}

impl gpui_kit::component::dock::Panel for SettingsTab {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.title_text(cx)
    }
}

impl Panel for SettingsTab {
    fn title_text(&self, _: &App) -> SharedString {
        "Settings".into()
    }
    fn default_dock(&self) -> DockPosition {
        DockPosition::Center
    }
}

impl Render for SettingsTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let pages = settings_page::all(cx);
        let query = self.search.read(cx).value().trim().to_lowercase();
        // While searching, show the first page that matches.
        let visible = |id: &str| {
            pages
                .iter()
                .any(|p| p.id == id && settings_page::matches(p, &query).is_some())
        };
        if !visible(self.page)
            && let Some(p) = pages
                .iter()
                .find(|p| settings_page::matches(p, &query).is_some())
        {
            self.page = p.id;
        }
        let page = pages.iter().find(|p| p.id == self.page).cloned();
        let scope = settings_page::scope(window, cx);
        let nav = self.render_nav(&pages, &query, cx);

        let header = page.as_ref().map(|p| {
            let title = match p.group {
                Some(g) => format!("{g} › {}", p.title),
                None => p.title.to_string(),
            };
            h_flex()
                .justify_between()
                .gap_3()
                .px_4()
                .py_2()
                .border_b_1()
                .border_color(theme.border)
                .child(div().text_base().font_weight(FontWeight::SEMIBOLD).child(title))
                .child(if p.scoped {
                    h_flex()
                        .gap_1()
                        .children([Scope::Everyone, Scope::Project].map(|s| {
                            Button::new(SharedString::from(format!("settings-scope-{}", s.label())))
                                .small()
                                .label(s.label())
                                .map(|b| pide_core::colors::toggle(b, scope == s, cx))
                                .on_click(move |_, window, cx| {
                                    settings_page::set_scope(s, window, cx)
                                })
                        }))
                        .into_any_element()
                } else {
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("Applies to every project")
                        .into_any_element()
                })
        });
        let body = page.as_ref().map(|p| self.view(p, window, cx));

        h_flex()
            .size_full()
            .track_focus(&self.focus)
            .bg(theme.background)
            .child(
                v_flex()
                    .w(px(220.))
                    .h_full()
                    .flex_none()
                    .border_r_1()
                    .border_color(theme.border)
                    .child(
                        div().p_2().child(
                            Input::new(&self.search)
                                .small()
                                .prefix(IconName::Search)
                                .cleanable(true),
                        ),
                    )
                    .child(
                        div()
                            .id("settings-nav")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .px_2()
                            .pb_2()
                            .child(nav),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .children(header)
                    .child(
                        div()
                            .id(SharedString::from(format!("settings-body-{}", self.page)))
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .p_4()
                            .children(body),
                    ),
            )
    }
}
