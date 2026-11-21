//! A script panel's view of pide, headless: a script imports `pide` and
//! renders what it got back.

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::{AppContext as _, Context, Entity, IntoElement, Render, TestAppContext, VisualTestContext, Window};
use pide_core::{Agent, AgentBackend, DialogAnswer, Services, Sessions, Workspace};

struct NoPi;
impl AgentBackend for NoPi {
    fn prompt(&mut self, _: String, _: &mut Agent, _: &mut Context<Agent>) {}
    fn abort(&mut self, _: &mut Agent, _: &mut Context<Agent>) {}
    fn new_session(&mut self, _: &mut Agent, _: &mut Context<Agent>) {}
    fn queue(&mut self, _: String, _: bool, _: &mut Agent, _: &mut Context<Agent>) {}
    fn answer_dialog(&mut self, _: String, _: DialogAnswer, _: &mut Agent, _: &mut Context<Agent>) {}
    fn restart(&mut self, _: &mut Agent, _: &mut Context<Agent>) {}
}

struct Host(Entity<gpui_shell::ScriptView>);
impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.0.clone()
    }
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pide-scripts-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

#[gpui_kit::test]
fn a_script_reads_its_project_through_pide(cx: &mut TestAppContext) {
    let project = temp("project");
    std::fs::write(project.join("notes.txt"), "hello from disk").unwrap();
    let app = temp("app");
    std::fs::write(
        app.join("main.js"),
        r#"
import { View, div } from "gpui-kit";
import { root, diagnostics, readFile, colors } from "pide";
export default class Probe extends View {
  init(_props, cx) {
    this.text = "…";
    cx.spawn(async (cx) => { this.text = await readFile("notes.txt"); cx.notify(); });
  }
  render() {
    return div()
      .child(`root=${root()}`)
      .child(`diags=${diagnostics().length}`)
      .child(`bg=${colors().background.length}`)
      .child(`file=${this.text}`);
  }
}
"#,
    )
    .unwrap();

    cx.update(|cx| {
        gpui_kit::init(cx);
        pide_scripts::init(cx);
    });
    let runtime = gpui_shell::components::new_isolated_runtime().unwrap();
    gpui_shell::set_capabilities(
        gpui_shell::Capabilities::new().read_roots([project.clone(), app.clone()]),
    );
    let loaded = runtime.load_application(&app, "main.js").unwrap();
    let slot = Rc::new(std::cell::RefCell::new(None));
    let slot2 = slot.clone();
    let p = project.clone();
    let window = cx.add_window(move |window, cx| {
        let workspace = cx.new(|cx| Workspace::new(p.clone(), cx));
        let sessions = cx.new(|cx| Sessions::new(Rc::new(|_| Box::new(NoPi)), cx));
        Services::register(window.window_handle().window_id(), Services { workspace, sessions }, cx);
        let view = runtime.mount_application(&loaded, window, cx).unwrap();
        *slot2.borrow_mut() = Some(view.clone());
        Host(view)
    });
    let mut vcx = VisualTestContext::from_window(*window, cx);
    let view = slot.borrow().clone().unwrap();
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    let tree = vcx.update(|_, cx| {
        let v = view.read(cx);
        assert_eq!(v.build_error(), None);
        v.snapshot().unwrap().debug_tree()
    });
    assert!(tree.contains(&format!("root={}", project.display())), "{tree}");
    assert!(tree.contains("diags=0"), "{tree}");
    assert!(tree.contains("bg=7"), "{tree}");
    assert!(tree.contains("file=hello from disk"), "{tree}");
}
