// Where the lines are: every file's size as a bar, biggest first, grouped by
// folder. Click a file to open it. Refreshes when a file is saved or added.
import { View, div } from "gpui-kit";
import { h_flex, v_flex } from "gpui-base";
import { listFiles, readFile, openFile, nextEvent, colors } from "pide";

const CODE = /\.(py|rs|ts|js|typ|tex|md)$/;

export default class CodeWeight extends View {
  init(_props, cx) {
    this.files = [];
    const load = async (cx) => {
      const paths = (await listFiles()).filter((p) => CODE.test(p));
      const files = [];
      for (const path of paths) {
        const text = await readFile(path);
        files.push({ path, lines: text.split("\n").length });
      }
      this.files = files.sort((a, b) => b.lines - a.lines);
      cx.notify();
    };
    cx.spawn(async (cx) => {
      await load(cx);
      for (;;) {
        const e = await nextEvent();
        if (e.kind === "saved" || e.kind === "files") await load(cx);
      }
    });
  }

  render() {
    const c = colors();
    const total = this.files.reduce((n, f) => n + f.lines, 0);
    const max = Math.max(1, ...this.files.map((f) => f.lines));
    const folders = new Map();
    for (const f of this.files) {
      const dir = f.path.includes("/") ? f.path.split("/")[0] : ".";
      folders.set(dir, (folders.get(dir) || 0) + f.lines);
    }
    return v_flex()
      .size_full()
      .bg(c.background)
      .text_color(c.foreground)
      .child(
        v_flex()
          .px_3()
          .pt_3()
          .pb_2()
          .gap_1()
          .child(div().text_base().font_weight(600).child(`${total} lines`))
          .child(
            div()
              .text_xs()
              .text_color(c.muted)
              .child(
                [...folders.entries()]
                  .sort((a, b) => b[1] - a[1])
                  .map(([d, n]) => `${d} ${Math.round((100 * n) / Math.max(1, total))}%`)
                  .join(" · "),
              ),
          ),
      )
      .child(
        v_flex()
          .id("weights")
          .flex_1()
          .overflow_y_scroll()
          .children(
            this.files.map((f, i) =>
              v_flex()
                .id(`f-${i}`)
                .px_3()
                .py_1()
                .gap_1()
                .cursor_pointer()
                .hover((s) => s.bg(c.list_hover))
                .on_click(() => openFile(f.path))
                .child(
                  h_flex()
                    .child(div().flex_1().text_sm().child(f.path))
                    .child(div().text_xs().text_color(c.muted).child(String(f.lines))),
                )
                .child(
                  div()
                    .h_1()
                    .rounded_full()
                    .bg(c.border)
                    .child(div().h_1().rounded_full().bg(c.accent).w(`${(100 * f.lines) / max}%`)),
                ),
            ),
          ),
      );
  }
}
