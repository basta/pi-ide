//! The in-window menu bar (File, Edit, View, Code, pi, Settings), Linux/Windows style,
//! on every platform. Items run registered commands, so they behave exactly
//! like the keybindings and Find Action; shortcuts shown are the live keymap's.

use gpui_kit::component::GlobalState;
use gpui_kit::component::input::{Copy, Cut, Paste, Redo, Search, SelectAll, Undo};
use gpui_kit::component::menu::AppMenuBar;
use gpui_kit::*;
use pide_core::RunCommand;

fn cmd(label: &'static str, id: &'static str) -> MenuItem {
    MenuItem::action(label, RunCommand { id: id.into() })
}

fn menus() -> Vec<Menu> {
    vec![
        Menu {
            name: "File".into(),
            items: vec![
                cmd("Open Project…", "project.open"),
                cmd("Recent Projects…", "project.recent"),
                cmd("Close Project", "project.close"),
                MenuItem::separator(),
                cmd("Save", "editor.save"),
                cmd("Open in Browser", "file.open_in_browser"),
                cmd("Close Tab", "editor.close"),
                MenuItem::separator(),
                cmd("Quit pide", "app.quit"),
            ],
            disabled: false,
        },
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::action("Undo", Undo),
                MenuItem::action("Redo", Redo),
                MenuItem::separator(),
                MenuItem::action("Cut", Cut),
                MenuItem::action("Copy", Copy),
                MenuItem::action("Paste", Paste),
                MenuItem::separator(),
                MenuItem::action("Select All", SelectAll),
                MenuItem::action("Find", Search),
                cmd("Find in Files…", "find.in_files"),
            ],
            disabled: false,
        },
        Menu {
            name: "View".into(),
            items: vec![
                cmd("Find Action…", "palette.open"),
                cmd("Go to File…", "file.goto"),
                MenuItem::separator(),
                cmd("Project Panel", "view.toggle_project"),
                cmd("pi Panel", "view.toggle_agent"),
                cmd("Problems", "view.problems"),
                cmd("Markdown Preview", "view.markdown_preview"),
                MenuItem::separator(),
                MenuItem::Submenu(Menu {
                    name: "Theme".into(),
                    items: vec![
                        cmd("Dark", "view.theme_dark"),
                        cmd("Light", "view.theme_light"),
                        cmd("Follow macOS", "view.theme_system"),
                    ],
                    disabled: false,
                }),
            ],
            disabled: false,
        },
        Menu {
            name: "Code".into(),
            items: vec![
                cmd("Go to Declaration", "editor.go_to_definition"),
                cmd("Show Quick Fixes", "editor.quick_fix"),
                cmd("Parameter Info", "editor.parameter_info"),
                MenuItem::separator(),
                cmd("Rename…", "editor.rename"),
                cmd("Optimize Imports", "editor.organize_imports"),
                MenuItem::separator(),
                cmd("Problems", "view.problems"),
                cmd("Restart Language Server", "lsp.restart"),
                cmd("Language Servers…", "lsp.servers"),
            ],
            disabled: false,
        },
        Menu {
            name: "pi".into(),
            items: vec![
                cmd("New Session", "agent.new_session"),
                cmd("Close Session", "agent.close_session"),
                MenuItem::separator(),
                cmd("Next Session", "agent.next_session"),
                cmd("Previous Session", "agent.prev_session"),
                MenuItem::separator(),
                cmd("Stop", "agent.abort"),
                cmd("Restart pi", "agent.restart"),
                MenuItem::separator(),
                cmd("pi Settings…", "pi.settings"),
            ],
            disabled: false,
        },
        Menu {
            name: "Settings".into(),
            items: vec![
                cmd("pi…", "pi.settings"),
                cmd("Language Servers…", "lsp.servers"),
                MenuItem::separator(),
                cmd("Open settings.json", "settings.open"),
                cmd("Open Keymap File", "keymap.open"),
                cmd("Reload Keymap", "keymap.reload"),
            ],
            disabled: false,
        },
    ]
}

/// Publish the menus to GPUI Kit (once, after every command is registered).
pub fn init(cx: &mut App) {
    let menus = menus();
    // Ids are strings: report any that don't name a registered command.
    let known: Vec<&'static str> = pide_core::CommandRegistry::all(cx).iter().map(|c| c.id).collect();
    let mut ids = Vec::new();
    collect_ids(menus.iter().flat_map(|m| &m.items), &mut ids);
    for id in ids.into_iter().filter(|id| !known.contains(&id.as_str())) {
        log::error!("menu item names unknown command {id}");
    }
    let owned = menus.into_iter().map(|m| m.owned()).collect();
    GlobalState::global_mut(cx).set_app_menus(owned);
}

fn collect_ids<'a>(items: impl Iterator<Item = &'a MenuItem>, out: &mut Vec<String>) {
    for item in items {
        match item {
            MenuItem::Action { action, .. } => {
                if let Some(c) = action.as_any().downcast_ref::<RunCommand>() {
                    out.push(c.id.to_string());
                }
            }
            MenuItem::Submenu(m) => collect_ids(m.items.iter(), out),
            _ => {}
        }
    }
}

/// A menu bar for one window.
pub fn bar(cx: &mut App) -> Entity<AppMenuBar> {
    AppMenuBar::new(cx)
}
