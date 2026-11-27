// Problems, as a script panel: every diagnostic the language servers report,
// grouped by file, errors first. Click one to open the file there.
// A port of crates/panels/problems (the Rust panel stays until this matches it).

import { View, div } from "gpui-kit";
import { h_flex, v_flex } from "gpui-base";
import { Button } from "gpui-component";
import { diagnostics, nextEvent, openFile, colors } from "pide";

const RANK = { error: 0, warning: 1, info: 2, hint: 3 };

export default class Problems extends View {
  init(_props, cx) {
    this.showHints = false;
    this.diags = diagnostics();
    // Take the servers' diagnostics once they've been still for a moment:
    // every keystroke on a half-typed line would make the list churn.
    cx.spawn(async (cx) => {
      for (;;) {
        const event = await nextEvent();
        if (event.kind !== "diagnostics") continue;
        await cx.sleep(700);
        this.diags = diagnostics();
        cx.notify();
      }
    });
  }

  render(cx) {
    const c = colors();
    const tone = { error: c.danger, warning: c.warning, info: c.info, hint: c.muted };
    const shown = this.diags.filter((d) => this.showHints || d.severity !== "hint");
    const errors = shown.filter((d) => d.severity === "error").length;
    const warnings = shown.filter((d) => d.severity === "warning").length;

    const files = new Map();
    for (const d of shown) {
      if (!files.has(d.path)) files.set(d.path, []);
      files.get(d.path).push(d);
    }
    const groups = [...files.entries()]
      .map(([path, items]) => {
        items.sort((a, b) => RANK[a.severity] - RANK[b.severity] || a.line - b.line);
        return { path, items, worst: RANK[items[0].severity] };
      })
      .sort((a, b) => a.worst - b.worst || a.path.localeCompare(b.path));

    const header = h_flex()
      .gap_3()
      .px_3()
      .py_1()
      .border_b_1()
      .border_color(c.border)
      .text_xs()
      .text_color(c.muted)
      .child(shown.length === 0 ? "No problems" : `${errors} errors, ${warnings} warnings`)
      .child(div().flex_1())
      .child(
        Button.new("hints")
          .label(this.showHints ? "Hide hints" : "Show hints")
          .size("xsmall")
          .ghost()
          .on_click((_e, cx) => {
            this.showHints = !this.showHints;
            cx.notify();
          }),
      );

    const rows = [];
    for (const g of groups) {
      rows.push(
        h_flex()
          .gap_2()
          .px_3()
          .pt_2()
          .pb_1()
          .child(div().text_sm().font_weight(600).child(g.path))
          .child(div().text_xs().text_color(c.muted).child(String(g.items.length))),
      );
      g.items.forEach((d, i) => {
        rows.push(
          h_flex()
            .id(`${g.path}:${i}`)
            .gap_2()
            .px_3()
            .py_0p5()
            .pl_6()
            .items_start()
            .cursor_pointer()
            .hover((s) => s.bg(c.list_hover))
            .on_click(() => openFile(d.path, d.line, d.column))
            .child(div().mt_1p5().w_2().h_2().flex_shrink_0().rounded_full().bg(tone[d.severity]))
            .child(div().flex_1().min_w_0().text_sm().child(d.message))
            .child(
              div()
                .flex_shrink_0()
                .text_xs()
                .text_color(c.muted)
                .child(`${d.source ? d.source + " · " : ""}${d.line}:${d.column}`),
            ),
        );
      });
    }

    return v_flex()
      .size_full()
      .bg(c.background)
      .text_color(c.foreground)
      .child(header)
      .child(v_flex().id("problems-list").flex_1().overflow_y_scroll().pb_2().children(rows));
  }
}
