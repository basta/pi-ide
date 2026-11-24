//! The Settings tab's pages. Every crate registers its own (pide's switches,
//! pi's setup, language servers, tool windows, script panels); the app hosts
//! the one Settings tab that lists them, searches them and switches the scope.
//!
//! A page is a view built once per Settings tab. It reads the tab's scope with
//! [`scope`] while rendering. Pages render only their content: the tab draws
//! the title, the scope switch and the scrolling.

use std::rc::Rc;

use gpui_kit::*;

/// Who a setting applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Scope {
    /// You, in every project (`~/.config/pide`, `~/.pi/agent`).
    #[default]
    Everyone,
    /// This project only (`.pide/`, `.pi/`).
    Project,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Scope::Everyone => "Everyone",
            Scope::Project => "This project",
        }
    }
}

pub type PageFactory = Rc<dyn Fn(&mut Window, &mut App) -> AnyView>;

#[derive(Clone)]
pub struct SettingsPage {
    /// Stable id (`editor`, `pi.packages`); `open(Some(id))` shows it.
    pub id: &'static str,
    /// The heading it's listed under (`pi`, `Languages`), or none for the top level.
    pub group: Option<&'static str>,
    pub title: &'static str,
    /// Words search finds the page by besides its title: its rows' titles.
    pub keywords: &'static [&'static str],
    /// Has settings for This project too (shows the scope switch).
    pub scoped: bool,
    /// Where it's listed: lower first. Groups sit where their first page does.
    pub order: u32,
    pub create: PageFactory,
}

#[derive(Default)]
struct Pages(Vec<SettingsPage>);
impl Global for Pages {}

pub fn register(cx: &mut App, page: SettingsPage) {
    let all = &mut cx.default_global::<Pages>().0;
    all.retain(|p| p.id != page.id);
    all.push(page);
    all.sort_by_key(|p| p.order);
}

/// Every page, in order.
pub fn all(cx: &App) -> Vec<SettingsPage> {
    cx.try_global::<Pages>()
        .map(|p| p.0.clone())
        .unwrap_or_default()
}

/// Does `page` match a search (already lowercased)? Gives the keyword that
/// matched when it isn't the title.
pub fn matches(page: &SettingsPage, query: &str) -> Option<Option<&'static str>> {
    if query.is_empty()
        || page.title.to_lowercase().contains(query)
        || page.group.is_some_and(|g| g.to_lowercase().contains(query))
    {
        return Some(None);
    }
    page.keywords
        .iter()
        .find(|k| k.to_lowercase().contains(query))
        .map(|k| Some(*k))
}

/// The app's side: open the Settings tab (on a page, or where it was).
#[derive(Clone)]
pub struct SettingsHost {
    pub open: Rc<dyn Fn(Option<&'static str>, &mut Window, &mut App)>,
}
impl Global for SettingsHost {}

pub fn open(page: Option<&'static str>, window: &mut Window, cx: &mut App) {
    if let Some(open) = cx.try_global::<SettingsHost>().map(|h| h.open.clone()) {
        open(page, window, cx);
    }
}

/// The scope each window's Settings tab shows.
#[derive(Default)]
struct Scopes(std::collections::HashMap<WindowId, Scope>);
impl Global for Scopes {}

pub fn scope(window: &Window, cx: &App) -> Scope {
    cx.try_global::<Scopes>()
        .and_then(|s| s.0.get(&window.window_handle().window_id()).copied())
        .unwrap_or_default()
}

pub fn set_scope(scope: Scope, window: &mut Window, cx: &mut App) {
    cx.default_global::<Scopes>()
        .0
        .insert(window.window_handle().window_id(), scope);
    window.refresh();
}

pub fn forget_window(id: WindowId, cx: &mut App) {
    cx.default_global::<Scopes>().0.remove(&id);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(title: &'static str, keywords: &'static [&'static str]) -> SettingsPage {
        SettingsPage {
            id: "x",
            group: Some("pi"),
            title,
            keywords,
            scoped: false,
            order: 0,
            create: Rc::new(|_, _| unreachable!()),
        }
    }

    #[::core::prelude::v1::test]
    fn search_finds_titles_groups_and_rows() {
        let p = page("Editor", &["Reformat on save", "Spell check"]);
        assert_eq!(matches(&p, ""), Some(None));
        assert_eq!(matches(&p, "edit"), Some(None));
        assert_eq!(matches(&p, "pi"), Some(None));
        assert_eq!(matches(&p, "spell"), Some(Some("Spell check")));
        assert_eq!(matches(&p, "theme"), None);
    }
}
