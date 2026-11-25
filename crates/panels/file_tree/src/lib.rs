//! File tree panel. Walks the project root off the UI thread (respecting
//! `.gitignore`), refreshes when files are created or removed, and opens a
//! file through the app's opener (panels never depend on each other). Files
//! pi changed in any open session get an M / A / D badge.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::component::dock::{PanelEvent, PanelState, panel_handle, register_panel};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::tree::{TreeEvent, TreeItem, TreeState, tree};
use gpui_kit::component::{ActiveTheme, IconName, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::{DockPosition, Panel, Services, Workspace, WorkspaceEvent};

mod ops;

pub const PANEL_NAME: &str = "file_tree";

pub fn init(cx: &mut App) {
    register_panel(cx, PANEL_NAME, |_, window, cx| {
        panel_handle(cx.new(|cx| FileTree::new(window, cx)))
    });
    pide_core::toolwindow::register(
        cx,
        pide_core::ToolWindowSpec {
            id: PANEL_NAME,
            title: "Project",
            icon: gpui_kit::component::IconName::Folder,
            dock: pide_core::DockPosition::Left,
            command: "view.toggle_project",
            key: Some("cmd-1"),
            create: std::rc::Rc::new(|window, cx| {
                panel_handle(cx.new(|cx| FileTree::new(window, cx)))
            }),
        },
    );
}

/// A plain, `Send` description of the tree, built on a background thread.
struct Node {
    path: PathBuf,
    name: String,
    children: Option<BTreeMap<(bool, String), Node>>, // Some = directory; key sorts dirs first
}

fn walk(root: &Path) -> Node {
    let mut top = Node {
        path: root.to_path_buf(),
        name: String::new(),
        children: Some(BTreeMap::new()),
    };
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .filter_entry(|e| e.file_name() != ".git" && e.file_name() != ".DS_Store")
        .sort_by_file_name(|a, b| a.cmp(b))
        .build();
    for entry in walker.flatten() {
        let path = entry.path();
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
        let mut node = &mut top;
        let comps: Vec<_> = rel.components().collect();
        for (i, comp) in comps.iter().enumerate() {
            let name = comp.as_os_str().to_string_lossy().to_string();
            let last = i == comps.len() - 1;
            let dir = !last || is_dir;
            let children = node.children.get_or_insert_with(BTreeMap::new);
            let key = (!dir, name.to_lowercase());
            node = children.entry(key).or_insert_with(|| Node {
                path: root.join(comps[..=i].iter().collect::<PathBuf>()),
                name: name.clone(),
                children: dir.then(BTreeMap::new),
            });
        }
    }
    top
}

fn to_items(
    node: Node,
    expanded: &HashSet<SharedString>,
    dirs: &mut HashSet<SharedString>,
) -> Vec<TreeItem> {
    node.children
        .unwrap_or_default()
        .into_values()
        .map(|n| {
            let id: SharedString = n.path.to_string_lossy().to_string().into();
            let label = n.name.clone();
            if n.children.is_some() {
                dirs.insert(id.clone());
                let open = expanded.contains(&id);
                TreeItem::new(id, label)
                    .children(to_items(n, expanded, dirs))
                    .expanded(open)
            } else {
                TreeItem::new(id, label)
            }
        })
        .collect()
}

#[derive(Default)]
struct Instances(HashMap<WindowId, WeakEntity<FileTree>>);
impl Global for Instances {}

/// Expand to `path` in this window's tree, select it and scroll it into view.
pub fn reveal(path: PathBuf, window: &mut Window, cx: &mut App) {
    let tree = cx
        .try_global::<Instances>()
        .and_then(|i| i.0.get(&window.window_handle().window_id()))
        .and_then(|w| w.upgrade());
    if let Some(tree) = tree {
        tree.update(cx, |t, cx| t.reveal(path, cx));
    }
}

pub struct FileTree {
    workspace: Entity<Workspace>,
    tree: Entity<TreeState>,
    focus: FocusHandle,
    expanded: HashSet<SharedString>,
    dirs: HashSet<SharedString>,
    _subs: Vec<Subscription>,
    _refresh: Option<Task<()>>,
    /// Select this item once the next refresh has built the tree.
    pending_reveal: Option<SharedString>,
}

impl EventEmitter<PanelEvent> for FileTree {}

impl FileTree {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let Services {
            workspace,
            sessions,
        } = Services::of(window, cx);
        let tree = cx.new(|cx| TreeState::new(cx));
        let subs = vec![
            cx.subscribe(&tree, |this, _, ev: &TreeEvent, _| match ev {
                TreeEvent::Expanded(id) => {
                    this.expanded.insert(id.clone());
                }
                TreeEvent::Collapsed(id) => {
                    this.expanded.remove(id);
                }
            }),
            // Re-render when pi's change lists move (badges).
            cx.observe(&sessions, |_, _, cx| cx.notify()),
            cx.subscribe(&workspace, |this, _, ev: &WorkspaceEvent, cx| {
                if matches!(ev, WorkspaceEvent::TreeChanged) {
                    this.refresh(Duration::from_millis(300), cx);
                }
            }),
        ];
        let mut this = Self {
            workspace,
            tree,
            focus: cx.focus_handle(),
            expanded: HashSet::new(),
            dirs: HashSet::new(),
            _subs: subs,
            _refresh: None,
            pending_reveal: None,
        };
        let me = cx.entity().downgrade();
        cx.default_global::<Instances>()
            .0
            .insert(window.window_handle().window_id(), me);
        let _ = window;
        this.refresh(Duration::ZERO, cx);
        this
    }

    fn reveal(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let root = self.workspace.read(cx).root().to_path_buf();
        if !path.starts_with(&root) {
            return;
        }
        // Open every folder between the root and the file.
        let mut dir = path.parent();
        while let Some(d) = dir {
            if d == root || !d.starts_with(&root) {
                break;
            }
            self.expanded.insert(d.to_string_lossy().to_string().into());
            dir = d.parent();
        }
        self.pending_reveal = Some(path.to_string_lossy().to_string().into());
        self.refresh(Duration::ZERO, cx);
    }

    /// Re-walk the root after `delay` (debounces bursts like `cargo build`).
    fn refresh(&mut self, delay: Duration, cx: &mut Context<Self>) {
        let root = self.workspace.read(cx).root().to_path_buf();
        self._refresh = Some(cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let node = cx
                .background_executor()
                .spawn(async move { walk(&root) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let mut dirs = HashSet::new();
                let items = to_items(node, &this.expanded, &mut dirs);
                this.dirs = dirs;
                this.tree.update(cx, |t, cx| t.set_items(items, cx));
                if let Some(id) = this.pending_reveal.take() {
                    this.tree.update(cx, |t, cx| {
                        if let Some(ix) = t.index_of(&id) {
                            t.set_selected_index(Some(ix), cx);
                            t.scroll_to_item(ix, ScrollStrategy::Center);
                        }
                    });
                }
                cx.notify();
            });
        }));
    }
}

impl Focusable for FileTree {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl gpui_kit::component::dock::BasePanel for FileTree {
    fn panel_name(&self) -> &'static str {
        PANEL_NAME
    }
    fn closable(&self, _: &App) -> bool {
        false
    }
    fn dump(&self, _: &App) -> PanelState {
        PanelState::new(PANEL_NAME)
    }
}

impl gpui_kit::component::dock::Panel for FileTree {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.title_text(cx)
    }
}

impl Panel for FileTree {
    fn title_text(&self, cx: &App) -> SharedString {
        self.workspace
            .read(cx)
            .root()
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Project".into())
            .into()
    }
    fn default_dock(&self) -> DockPosition {
        DockPosition::Left
    }
}

impl Render for FileTree {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        // path -> badge, for files pi changed (and that weren't reverted) in any open session.
        let mut badges: std::collections::HashMap<String, &'static str> =
            std::collections::HashMap::new();
        for agent in Services::of(window, cx).sessions.read(cx).all() {
            for c in &agent.read(cx).changes {
                if !c.reverted {
                    badges.insert(c.path.to_string_lossy().to_string(), c.badge());
                }
            }
        }
        let badges = std::rc::Rc::new(badges);
        let root = std::rc::Rc::new(self.workspace.read(cx).root().to_path_buf());
        div().size_full().track_focus(&self.focus).child(
            tree(&self.tree, move |ix, entry, _selected, _window, cx| {
                let item = entry.item();
                let badge = badges.get(item.id.as_ref()).copied();
                let is_dir = view.read(cx).dirs.contains(&item.id);
                let icon = if !is_dir {
                    IconName::File
                } else if entry.is_expanded() {
                    IconName::FolderOpen
                } else {
                    IconName::Folder
                };
                let id = item.id.clone();
                ListItem::new(ix)
                    // GPUI Kit's list items are 16 px; tree rows are body text.
                    .text_sm()
                    .w_full()
                    .rounded(cx.theme().radius)
                    .py_0p5()
                    .px_2()
                    .pl(px(14.) * entry.depth() + px(8.))
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .child(icon)
                            .child(div().flex_1().child(item.label.clone()))
                            .when_some(badge, |this, b| {
                                let color = pide_core::colors::change(
                                    pide_core::colors::Change::from_letter(
                                        b.chars().next().unwrap_or('M'),
                                    ),
                                    cx,
                                );
                                this.child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::BOLD)
                                        .text_color(color)
                                        .child(b),
                                )
                            }),
                    )
                    .on_click(move |_, window, cx| {
                        if is_dir {
                            return;
                        }
                        pide_core::opener::open_file(PathBuf::from(id.as_ref()), window, cx);
                    })
            })
            .context_menu(move |_ix, entry, menu, _window, _cx| {
                let path = PathBuf::from(entry.item().id.as_ref());
                let is_dir = entry.is_folder();
                // New things go inside a folder, or next to a file.
                let dir = if is_dir {
                    path.clone()
                } else {
                    path.parent().map(Path::to_path_buf).unwrap_or_default()
                };
                let rel = path
                    .strip_prefix(root.as_path())
                    .map(|r| r.display().to_string())
                    .unwrap_or_else(|_| path.display().to_string());
                let (d1, d2, d3) = (dir.clone(), dir.clone(), dir);
                let (p1, p2, p3) = (path.clone(), path.clone(), path);
                menu.item(
                    PopupMenuItem::new("New File…").on_click(move |_, window, cx| {
                        ops::new_entry(d1.clone(), false, window, cx)
                    }),
                )
                .item(
                    PopupMenuItem::new("New Folder…").on_click(move |_, window, cx| {
                        ops::new_entry(d2.clone(), true, window, cx)
                    }),
                )
                .separator()
                .item(
                    PopupMenuItem::new("Rename…")
                        .on_click(move |_, window, cx| ops::rename(p1.clone(), window, cx)),
                )
                .item(
                    PopupMenuItem::new("Move to Trash…")
                        .on_click(move |_, window, cx| ops::delete(p2.clone(), window, cx)),
                )
                .separator()
                .item(PopupMenuItem::new("Copy Path").on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(rel.clone()))
                }))
                .item(
                    PopupMenuItem::new(if cfg!(target_os = "macos") {
                        "Reveal in Finder"
                    } else {
                        "Show in Folder"
                    })
                    .on_click(move |_, _, _| ops::reveal(&p3)),
                )
                .item(PopupMenuItem::new("Open in Terminal").on_click(move |_, window, cx| {
                    pide_core::opener::open_terminal(d3.clone(), window, cx)
                }))
            })
            .size_full()
            .text_sm()
            .p_1()
            .bg(cx.theme().sidebar),
        )
    }
}
