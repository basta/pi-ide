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
                cmd("Close Other Tabs", "editor.close_others"),
                cmd("Reopen Closed Tab", "editor.reopen_closed"),
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
                cmd("Recent Files…", "file.recent"),
                cmd("Go to Line…", "editor.goto_line"),
                cmd("Select Opened File", "project.select_opened"),
                cmd("Back", "nav.back"),
                cmd("Forward", "nav.forward"),
                MenuItem::separator(),
                MenuItem::Submenu(Menu {
                    name: "Tool Windows".into(),
                    items: vec![
                        cmd("Project", "view.toggle_project"),
                        cmd("pi", "view.toggle_agent"),
                        cmd("Problems", "view.problems"),
                        cmd("Find", "view.find"),
                        cmd("Run", "view.run"),
                        cmd("Commit", "git.changes"),
                        cmd("Git Log", "git.log"),
                        MenuItem::separator(),
                        cmd("Hide Active Tool Window", "view.hide_tool"),
                        cmd("Hide All Tool Windows", "view.hide_all_tools"),
                    ],
                    disabled: false,
                }),
                cmd("Next Problem", "nav.next_problem"),
                cmd("Previous Problem", "nav.prev_problem"),
                cmd("Markdown Preview", "view.markdown_preview"),
                cmd("Move Tab to Right Split", "editor.split_right"),
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
                cmd("Go to Implementation", "editor.go_to_implementation"),
                cmd("Go to Type Declaration", "editor.go_to_type"),
                cmd("Find Usages", "editor.find_usages"),
                cmd("File Structure", "editor.file_structure"),
                cmd("Go to Symbol…", "editor.go_to_symbol"),
                cmd("Show Quick Fixes", "editor.quick_fix"),
                cmd("Parameter Info", "editor.parameter_info"),
                cmd("Quick Documentation", "editor.quick_doc"),
                MenuItem::separator(),
                cmd("Rename…", "editor.rename"),
                cmd("Reformat File", "editor.reformat"),
                cmd("Run Command…", "run.command"),
                cmd("Optimize Imports", "editor.organize_imports"),
                MenuItem::separator(),
                cmd("Commit…", "git.changes"),
                cmd("Git Log", "git.log"),
                cmd("Next Change", "git.next_change"),
                cmd("Previous Change", "git.prev_change"),
                cmd("Rollback Lines", "git.rollback"),
                cmd("Annotate with Git Blame", "git.annotate"),
                cmd("Why This Line? (pi)", "git.why_line"),
                MenuItem::separator(),
                cmd("LaTeX: Show in PDF", "latex.forward_search"),
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
                cmd("Ask pi", "agent.ask"),
                MenuItem::separator(),
                cmd("New Session", "agent.new_session"),
                cmd("Close Session", "agent.close_session"),
                cmd("Rename Session…", "agent.rename"),
                MenuItem::separator(),
                cmd("Next Session", "agent.next_session"),
                cmd("Previous Session", "agent.prev_session"),
                MenuItem::separator(),
                cmd("Walk Through Changes", "agent.walk_through"),
                cmd("Next Note", "agent.next_note"),
                cmd("Previous Note", "agent.previous_note"),
                cmd("Review Next Session", "agent.review_next"),
                MenuItem::separator(),
                cmd("Stop", "agent.abort"),
                cmd("Compact Session", "agent.compact"),
                cmd("Export Session as HTML", "agent.export"),
                cmd("Restart pi", "agent.restart"),
                MenuItem::separator(),
                cmd("pi Settings…", "pi.settings"),
            ],
            disabled: false,
        },
        Menu {
            name: "Settings".into(),
            items: vec![
                cmd("pide…", "settings.pide"),
                cmd("Keymap…", "settings.keymap"),
                cmd("pi…", "pi.settings"),
                cmd("Language Servers…", "lsp.servers"),
                MenuItem::separator(),
                cmd("Open settings.json", "settings.open"),
                cmd("Open Keymap File", "keymap.open"),
                cmd("Reload Keymap", "keymap.reload"),
            ],
            disabled: false,
        },
        Menu {
            name: "Help".into(),
            items: vec![
                cmd("Find Action…", "palette.open"),
                cmd("Keyboard Shortcuts…", "settings.keymap"),
                MenuItem::separator(),
                cmd("Show Log in Finder", "app.show_log"),
            ],
            disabled: false,
        },
    ]
}

/// Publish the menus to GPUI Kit (once, after every command is registered).
pub fn init(cx: &mut App) {
    let menus = menus();
    // Ids are strings: report any that don't name a registered command.
    let known: Vec<&'static str> = pide_core::CommandRegistry::all(cx)
        .iter()
        .map(|c| c.id)
        .collect();
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
