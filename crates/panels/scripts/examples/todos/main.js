// Every TODO, FIXME and HACK in the project, grouped by file. Click to jump.
import { View, div } from "gpui-kit";
import { h_flex, v_flex } from "gpui-base";
import { listFiles, readFile, openFile, nextEvent, colors } from "pide";

const MARK = /\b(TODO|FIXME|HACK)\b:?\s*(.*)/;

export default class Todos extends View {
  init(_props, cx) {
    this.items = [];
    const scan = async () => {
      const items = [];
      for (const path of await listFiles()) {
        if (!/\.(py|rs|ts|js|typ|md)$/.test(path)) continue;
        (await readFile(path)).split("\n").forEach((line, i) => {
          const m = line.match(MARK);
          if (m) items.push({ path, line: i + 1, kind: m[1], text: m[2] || "(no text)" });
        });
      }
      this.items = items;
      cx.notify();
    };
    cx.spawn(async () => {
      await scan();
      for (;;) {
        const e = await nextEvent();
        if (e.kind === "saved" || e.kind === "files") await scan();
      }
    });
  }

  render() {
    const c = colors();
    const tint = { TODO: c.info, FIXME: c.danger, HACK: c.warning };
    const files = [...new Set(this.items.map((t) => t.path))];
    return v_flex().size_full().bg(c.background).text_color(c.foreground)
      .child(h_flex().px_3().pt_3().pb_2().gap_2()
        .child(div().text_base().font_weight(600).child(`${this.items.length} to-dos`))
        .child(div().text_xs().text_color(c.muted).child(`in ${files.length} files`)))
      .child(v_flex().id("todos").flex_1().overflow_y_scroll().pb_2().children(
        files.map((path) => v_flex().pt_2()
          .child(div().px_3().pb_1().text_xs().text_color(c.muted).child(path))
          .children(this.items.filter((t) => t.path === path).map((t) =>
            h_flex().id(`${t.path}:${t.line}`).px_3().py_1().gap_2().items_start().cursor_pointer()
              .hover((s) => s.bg(c.list_hover))
              .on_click(() => openFile(t.path, t.line))
              .child(div().flex_none().w("44px").text_xs().font_weight(600).text_color(tint[t.kind]).child(t.kind))
              .child(div().flex_1().min_w_0().text_sm().child(t.text))
              .child(div().flex_none().text_xs().text_color(c.muted).child(`:${t.line}`)))))));
  }
}
