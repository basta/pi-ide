// pytest results, one row per test. Runs on demand and after each save.
import { View, div } from "gpui-kit";
import { h_flex, v_flex } from "gpui-base";
import { Button } from "gpui-component";
import { run } from "process";
import { root, openFile, nextEvent, colors, askPi } from "pide";

export default class Tests extends View {
  init(_props, cx) {
    this.results = [];
    this.running = false;
    this.took = null;
    cx.spawn(async (cx) => {
      await this.run(cx);
      for (;;) {
        const e = await nextEvent();
        if (e.kind === "saved" && e.path.endsWith(".py")) await this.run(cx);
      }
    });
  }

  async run(cx) {
    this.running = true;
    cx.notify();
    const out = await run("python3", ["-m", "pytest", "-q", "-rA", "--tb=line", "-p", "no:cacheprovider"], { cwd: root() });
    // "PASSED tests/test_x.py::name" and "FAILED tests/test_x.py::name - why"
    this.results = out.stdout.split("\n").flatMap((l) => {
      const m = l.match(/^(PASSED|FAILED|ERROR) (\S+?)::(\S+)(?: - (.*))?$/);
      return m ? [{ ok: m[1] === "PASSED", path: m[2], name: m[3], why: m[4] || "" }] : [];
    });
    const t = out.stdout.match(/in ([\d.]+)s/);
    this.took = t ? t[1] : null;
    this.running = false;
    cx.notify();
  }

  render() {
    const c = colors();
    const failed = this.results.filter((r) => !r.ok);
    return v_flex().size_full().bg(c.background).text_color(c.foreground)
      .child(h_flex().px_3().py_2().gap_3().border_b_1().border_color(c.border)
        .child(div().text_sm().font_weight(600).text_color(failed.length ? c.danger : c.success)
          .child(this.running ? "Running…" : failed.length ? `${failed.length} failed` : "All passed"))
        .child(div().text_xs().text_color(c.muted)
          .child(`${this.results.length - failed.length} passed${this.took ? ` · ${this.took}s` : ""}`))
        .child(div().flex_1())
        .children(failed.length === 0 ? [] : [Button.new("ask").label("Ask pi about failures").size("xsmall").ghost()
          .on_click(() => askPi(`These tests fail:\n${failed.map((f) => `${f.path}::${f.name} - ${f.why}`).join("\n")}\nWhy?`))])
        .child(Button.new("rerun").label("Run").size("xsmall").ghost().on_click((_, cx) => cx.spawn((cx) => this.run(cx)))))
      .child(v_flex().id("results").flex_1().overflow_y_scroll().children(
        [...failed, ...this.results.filter((r) => r.ok)].map((r, i) =>
          h_flex().id(`t-${i}`).px_3().py_0p5().gap_2().cursor_pointer()
            .hover((s) => s.bg(c.list_hover))
            .on_click(() => openFile(r.path))
            .child(div().flex_none().w("8px").h("8px").rounded_full().bg(r.ok ? c.success : c.danger))
            .child(div().text_sm().child(r.name))
            .child(div().text_xs().text_color(c.muted).child(r.path))
            .child(div().flex_1().text_xs().text_color(c.danger).text_ellipsis().child(r.why)))));
  }
}
