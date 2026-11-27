//! How one part of the app asks another to open something, without panels
//! depending on each other: the app installs these handlers at startup.

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::*;

/// Show how a file changed: `before` (`None` = file didn't exist) vs `after`
/// (`None` = the file on disk now; a fixed text for one commit's change).
#[derive(Clone, Debug)]
pub struct DiffRequest {
    pub path: PathBuf,
    pub before: Option<String>,
    /// A fixed after-text (e.g. the file in a commit). Fixed diffs can't be
    /// reverted from the tab. `None` compares with the file on disk.
    pub after: Option<String>,
    /// Shown in the tab, e.g. the session name.
    pub label: SharedString,
    /// What "before" is, for the header: "when pi first touched it", "HEAD".
    pub before_is: SharedString,
}

/// Show a PDF (a LaTeX build's output): open it, beside the editor the first
/// time when `beside`; scroll to a spot (SyncTeX); reload it after a build.
#[derive(Clone, Debug, PartialEq)]
pub struct PdfRequest {
    pub path: PathBuf,
    /// Open it in a split to the right of the active tab, if it isn't open yet.
    pub beside: bool,
    /// Read the file again (it was just rebuilt).
    pub reload: bool,
    /// Scroll to this spot and mark it.
    pub at: Option<PdfSpot>,
    /// Scroll to where one of these phrases is in the PDF (after the reload),
    /// and mark it: the first phrase found wins, a single hit over several,
    /// the hit nearest the view. For documents without SyncTeX (Typst).
    pub find: Vec<String>,
    /// Bring an open PDF's tab to the front (and mark a found spot even when
    /// it's already in view). Off for background refreshes while typing.
    pub raise: bool,
    /// Open a tab for it when none is open. Off for a rebuild after an outside
    /// change (pi's edit), which only refreshes a PDF already on screen.
    pub open: bool,
}

/// A point on a PDF page: 0-based page, PDF points from its top-left corner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdfSpot {
    pub page: usize,
    pub x: f32,
    pub y: f32,
}

/// A place in a file, with its line's text (usages, symbol search results).
#[derive(Clone, Debug, PartialEq)]
pub struct Location {
    pub path: PathBuf,
    /// 0-based line and character column.
    pub line: u32,
    pub character: u32,
    /// The whole line, for showing the hit in context.
    pub text: String,
    /// Byte range of the hit inside `text`.
    pub range: std::ops::Range<usize>,
}

#[derive(Clone)]
pub struct Openers {
    /// Show a list of locations under a title ("Usages of greet").
    pub locations: Rc<dyn Fn(SharedString, Vec<Location>, &mut Window, &mut App)>,
    pub file: Rc<dyn Fn(PathBuf, &mut Window, &mut App)>,
    /// Open and put the cursor at a 0-based line and character column.
    pub file_at: Rc<dyn Fn(PathBuf, u32, u32, &mut Window, &mut App)>,
    pub diff: Rc<dyn Fn(DiffRequest, &mut Window, &mut App)>,
    /// Close the tabs of a file, or of everything under a folder (it was renamed or trashed).
    pub close: Rc<dyn Fn(PathBuf, &mut Window, &mut App)>,
    /// Put text in the agent's message box (not sent) and focus it.
    pub ask: Rc<dyn Fn(String, &mut Window, &mut App)>,
    /// Show a panel as a center tab, or select it when it's already one
    /// (the entity id tells). For panels that aren't files (Annotate).
    pub panel: Rc<CenterPanelFn>,
    /// See [`PdfRequest`]. Keeps the focus where it is.
    pub pdf: Rc<dyn Fn(PdfRequest, &mut Window, &mut App)>,
    /// Open a terminal tab in a folder.
    pub terminal: Rc<dyn Fn(PathBuf, &mut Window, &mut App)>,
}

pub type CenterPanelFn = dyn Fn(
    std::sync::Arc<dyn gpui_kit::component::dock::BasePanelView>,
    EntityId,
    &mut Window,
    &mut App,
);

impl Global for Openers {}

pub fn open_file(path: PathBuf, window: &mut Window, cx: &mut App) {
    if let Some(o) = cx.try_global::<Openers>().map(|o| o.file.clone()) {
        o(path, window, cx);
    }
}

/// See [`Openers::panel`].
pub fn open_panel(
    view: std::sync::Arc<dyn gpui_kit::component::dock::BasePanelView>,
    id: EntityId,
    window: &mut Window,
    cx: &mut App,
) {
    if let Some(o) = cx.try_global::<Openers>().map(|o| o.panel.clone()) {
        o(view, id, window, cx);
    }
}

/// See [`Openers::terminal`].
pub fn open_terminal(dir: PathBuf, window: &mut Window, cx: &mut App) {
    if let Some(o) = cx.try_global::<Openers>().map(|o| o.terminal.clone()) {
        o(dir, window, cx);
    }
}

/// See [`PdfRequest`].
pub fn show_pdf(req: PdfRequest, window: &mut Window, cx: &mut App) {
    if let Some(o) = cx.try_global::<Openers>().map(|o| o.pdf.clone()) {
        o(req, window, cx);
    }
}

pub fn ask_agent(text: String, window: &mut Window, cx: &mut App) {
    if let Some(o) = cx.try_global::<Openers>().map(|o| o.ask.clone()) {
        o(text, window, cx);
    }
}

pub fn close_file(path: PathBuf, window: &mut Window, cx: &mut App) {
    if let Some(o) = cx.try_global::<Openers>().map(|o| o.close.clone()) {
        o(path, window, cx);
    }
}

pub fn open_file_at(path: PathBuf, line: u32, character: u32, window: &mut Window, cx: &mut App) {
    if let Some(o) = cx.try_global::<Openers>().map(|o| o.file_at.clone()) {
        o(path, line, character, window, cx);
    }
}

pub fn show_locations(
    title: SharedString,
    locations: Vec<Location>,
    window: &mut Window,
    cx: &mut App,
) {
    if let Some(o) = cx.try_global::<Openers>().map(|o| o.locations.clone()) {
        o(title, locations, window, cx);
    }
}

pub fn open_diff(req: DiffRequest, window: &mut Window, cx: &mut App) {
    if let Some(o) = cx.try_global::<Openers>().map(|o| o.diff.clone()) {
        o(req, window, cx);
    }
}
