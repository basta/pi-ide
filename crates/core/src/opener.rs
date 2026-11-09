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

#[derive(Clone)]
pub struct Openers {
    pub file: Rc<dyn Fn(PathBuf, &mut Window, &mut App)>,
    pub diff: Rc<dyn Fn(DiffRequest, &mut Window, &mut App)>,
}

impl Global for Openers {}

pub fn open_file(path: PathBuf, window: &mut Window, cx: &mut App) {
    if let Some(o) = cx.try_global::<Openers>().map(|o| o.file.clone()) {
        o(path, window, cx);
    }
}

pub fn open_diff(req: DiffRequest, window: &mut Window, cx: &mut App) {
    if let Some(o) = cx.try_global::<Openers>().map(|o| o.diff.clone()) {
        o(req, window, cx);
    }
}
