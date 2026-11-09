//! How one part of the app asks another to open something, without panels
//! depending on each other: the app installs these handlers at startup.

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::*;

/// Show what pi changed in a file: `before` (`None` = file didn't exist) vs the file on disk now.
#[derive(Clone, Debug)]
pub struct DiffRequest {
    pub path: PathBuf,
    pub before: Option<String>,
    /// Shown in the tab, e.g. the session name.
    pub label: SharedString,
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
}

impl Global for Openers {}

pub fn open_file(path: PathBuf, window: &mut Window, cx: &mut App) {
    if let Some(o) = cx.try_global::<Openers>().map(|o| o.file.clone()) {
        o(path, window, cx);
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
