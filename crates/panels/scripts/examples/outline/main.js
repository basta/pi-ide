// Classes and functions of the active Python file, following the cursor.
import { View, div } from "gpui-kit";
import { h_flex, v_flex } from "gpui-base";
import { activeFile, readFile, openFile, nextEvent, colors } from "pide";

const DEF = /^(\s*)(class|def|async def)\s+([A-Za-z_]\w*)/;

export default class Outline extends View {
  init(_props, cx) {
    this.path = null;
    this.defs = [];
    this.line = null;
    const load = async () => {
      const f = activeFile();
      if (f && f.line) this.line = f.line;
      if (!f || !f.path.endsWith(".py")) { this.path = f ? f.path : null; this.defs = []; cx.notify(); return; }
      this.path = f.path;
      this.defs = (await readFile(f.path)).split("\n").flatMap((text, i) => {
        const m = text.match(DEF);
        return m ? [{ line: i + 1, depth: m[1].length / 4, kind: m[2], name: m[3] }] : [];
      });
      cx.notify();
    };
    cx.spawn(async (cx) => {
      await load();
      for (;;) {
        const e = await nextEvent();
        if (e.kind === "cursor") {
          const f = activeFile();
          if (f && f.path !== this.path) await load();
          else if (f && f.line !== this.line) { this.line = f.line; cx.notify(); }
        } else if (e.kind === "active" || e.kind === "changed" || e.kind === "saved") await load();
      }
    });
  }

  render() {
    const c = colors();
    // The definition the cursor is in: the last one that starts at or above it.
    const here = this.defs.filter((d) => this.line && d.line <= this.line).pop();
    const body = this.defs.length === 0
      ? div().px_3().py_2().text_sm().text_color(c.muted).child(this.path ? `No Python outline for ${this.path}` : "Open a Python file")
      : v_flex().id("defs").flex_1().overflow_y_scroll().children(this.defs.map((d) =>
          h_flex().id(`d-${d.line}`).pl(`${12 + d.depth * 16}px`).pr_3().py_0p5().gap_2().cursor_pointer()
            .bg(d === here ? c.list_hover : c.background)
            .hover((s) => s.bg(c.list_hover))
            .on_click(() => openFile(this.path, d.line))
            .child(div().flex_none().w("14px").text_xs().font_weight(600)
              .text_color(d.kind === "class" ? c.warning : c.accent).child(d.kind === "class" ? "C" : "ƒ"))
            .child(div().flex_1().text_sm().font_weight(d === here ? 600 : 400).child(d.name))
            .child(div().text_xs().text_color(c.muted).child(String(d.line)))));
    return v_flex().size_full().bg(c.background).text_color(c.foreground)
      .child(div().px_3().pt_3().pb_2().text_xs().text_color(c.muted).child(this.path || ""))
      .child(body);
  }
}
