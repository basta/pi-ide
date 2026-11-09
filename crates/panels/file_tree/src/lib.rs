//! File tree panel. Walks the project root off the UI thread (respecting
//! `.gitignore`), refreshes when files are created or removed, and opens a
//! file through the app's opener (panels never depend on each other). Files
//! pi changed in any open session get an M / A / D badge.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::component::dock::{PanelEvent, PanelState, panel_handle, register_panel};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::tree::{TreeEvent, TreeItem, TreeState, tree};
use gpui_kit::component::{ActiveTheme, IconName, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::{DockPosition, Panel, Services, Workspace, WorkspaceEvent};

pub const PANEL_NAME: &str = "file_tree";

pub fn init(cx: &mut App) {
    register_panel(cx, PANEL_NAME, |_, window, cx| {
        panel_handle(cx.new(|cx| FileTree::new(window, cx)))
    });
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
        .filter_entry(|e| e.file_name() != ".git")
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

pub struct FileTree {
    workspace: Entity<Workspace>,
    tree: Entity<TreeState>,
    focus: FocusHandle,
    expanded: HashSet<SharedString>,
    dirs: HashSet<SharedString>,
    _subs: Vec<Subscription>,
    _refresh: Option<Task<()>>,
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
        };
        let _ = window;
        this.refresh(Duration::ZERO, cx);
        this
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
                                let color = if b == "A" {
                                    cx.theme().success
                                } else if b == "D" {
                                    cx.theme().danger
                                } else {
                                    cx.theme().warning
                                };
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
            .size_full()
            .text_sm()
            .p_1()
            .bg(cx.theme().sidebar),
        )
    }
}
