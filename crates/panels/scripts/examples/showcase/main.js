// Everything a script panel can do today, one tab per area:
// Project (the pide module), Controls (gpui-component), Charts, System
// (files, processes, network, storage, timers).
import { View, div } from "gpui-kit";
import { h_flex, v_flex, Slider, SliderTrack, SliderIndicator, SliderThumb, SliderState, Input, InputState } from "gpui-base";
import {
  Button, Switch, Checkbox, Progress,
  Tag, Badge, Kbd, Rating, Alert, Avatar, Spinner, Skeleton, ShimmerText,
  TabBar, Tab, BarChart, LineChart, PieChart, Separator,
} from "gpui-component";
import * as fs from "fs/promises";
import { run } from "process";
import {
  root, activeFile, openFile, diagnostics, listFiles, readFile,
  runCommand, askPi, colors, nextEvent,
} from "pide";

const TABS = ["Project", "Controls", "Charts", "System"];

export default class Showcase extends View {
  init(_props, cx) {
    this.tab = 0;
    this.events = [];
    this.files = [];
    // Controls keep their own state objects; make them once, here.
    this.slider = SliderState.new({ value: 40 });
    this.sliderValue = 40;
    this.slider.on("change", (v, cx) => { this.sliderValue = Math.round(typeof v === "number" ? v : v.end); cx.notify(); });
    this.input = InputState.new({ placeholder: "Type to filter files…" });
    this.filter = "";
    this.input.on("change", (_e, cx) => { this.filter = this.input.value(); cx.notify(); });
    this.compact = false;
    this.hints = true;
    this.rating = 3;
    // System tab results.
    this.entries = [];
    this.gitLog = "…";
    this.fetched = "…";
    this.clicks = Number(window.localStorage.getItem("clicks") || 0);
    this.now = new Date();

    cx.spawn(async (cx) => {
      this.files = await listFiles();
      // readFile: the open editor's text, or the file on disk.
      this.lines = [];
      for (const path of this.files.filter((p) => /\.(py|js|md)$/.test(p) && !p.startsWith(".pide"))) {
        this.lines.push({ path, n: (await readFile(path)).split("\n").length });
      }
      this.entries = await fs.readdir(root(), { withFileTypes: true });
      try {
        const out = await run("git", ["log", "--oneline", "-5"], { cwd: root() });
        this.gitLog = out.stdout.trim() || out.stderr.trim();
      } catch (e) { this.gitLog = `denied: ${e.message}`; }
      try {
        const r = await fetch("https://example.com");
        this.fetched = `HTTP ${r.status}`;
      } catch (e) { this.fetched = `denied: ${e.message}`; }
      cx.notify();
    });
    cx.spawn(async (cx) => {
      for (;;) {
        const e = await nextEvent();
        this.events = [{ ...e, at: new Date() }, ...this.events].slice(0, 6);
        cx.notify();
      }
    });
    cx.spawn(async (cx) => {
      for (;;) { await cx.sleep(1000); this.now = new Date(); if (this.tab === 3) cx.notify(); }
    });
  }

  render() {
    const c = colors();
    this.c = c;
    const body = [this.project, this.controls, this.charts, this.system][this.tab].call(this, c);
    return v_flex().size_full().bg(c.background).text_color(c.foreground)
      .child(div().px_2().pt_2()
        .child(TabBar.new("tabs").variant("underline").size("small").selected_index(this.tab)
          .on_change((i, cx) => { this.tab = i; cx.notify(); })
          .children(TABS.map((t) => Tab.new().label(t)))))
      .child(v_flex().id("body").flex_1().overflow_y_scroll().px_3().py_3().gap_4().child(body));
  }

  section(title, note, ...children) {
    const c = this.c;
    return v_flex().gap_2()
      .child(h_flex().gap_2().items_baseline()
        .child(div().text_sm().font_weight(600).child(title))
        .child(div().text_xs().text_color(c.muted).child(note)))
      .children(children);
  }

  row(label, value) {
    return h_flex().gap_2().text_sm()
      .child(div().w("96px").flex_none().text_xs().text_color(this.c.muted).child(label))
      .child(div().flex_1().min_w_0().text_ellipsis().child(value));
  }

  // ── Project: the pide module ────────────────────────────────────────────
  project(c) {
    const f = activeFile();
    const d = diagnostics();
    const count = (s) => d.filter((x) => x.severity === s).length;
    return v_flex().gap_4()
      .child(this.section("Read", "root, activeFile, diagnostics, listFiles",
        this.row("root", root()),
        this.row("active file", f ? `${f.path}${f.line ? `:${f.line}:${f.column}` : ""}` : "none"),
        this.row("selection", f && f.selection ? JSON.stringify(f.selection.slice(0, 40)) : "none"),
        this.row("diagnostics", `${count("error")} errors · ${count("warning")} warnings · ${d.length} total`),
        this.row("files", `${this.files.length} (gitignore-aware)`)))
      .child(this.section("Act", "openFile, runCommand, askPi",
        h_flex().gap_2().flex_wrap()
          .child(Button.new("open").label("Open src/app.py:12").size("small").outline()
            .on_click(() => openFile("src/app.py", 12, 5)))
          .child(Button.new("cmd").label("Run “view.problems”").size("small").outline()
            .on_click(() => runCommand("view.problems")))
          .child(Button.new("ask").label("Ask pi about this file").size("small").outline()
            .on_click(() => askPi(`What does ${f ? f.path : "this project"} do?`)))))
      .child(this.section("Events", "await nextEvent()",
        this.events.length === 0
          ? div().text_xs().text_color(c.muted).child("Edit, save, move the cursor or switch tabs…")
          : v_flex().gap_1().children(this.events.map((e, i) =>
              h_flex().gap_2().text_xs()
                .child(div().w("60px").flex_none().text_color(c.muted).child(e.at.toTimeString().slice(0, 8)))
                .child(Tag.new().size("xsmall").variant(e.kind === "diagnostics" ? "danger" : e.kind === "cursor" ? "secondary" : "info").child(e.kind))
                .child(div().text_color(c.muted).child(e.path || ""))))))
      .child(this.section("Theme", "colors()",
        h_flex().gap_2().flex_wrap().children(Object.entries(c).map(([name, hex]) =>
          v_flex().gap_1().items_center()
            .child(div().w("36px").h("24px").rounded_md().bg(hex).border_1().border_color(c.border))
            .child(div().text_xs().text_color(c.muted).child(name))))));
  }

  // ── Controls: gpui-component ────────────────────────────────────────────
  controls(c) {
    const shown = this.files.filter((p) => p.includes(this.filter)).slice(0, 4);
    return v_flex().gap_4()
      .child(this.section("Buttons", "variants, sizes, loading",
        h_flex().gap_2().flex_wrap()
          .child(Button.new("b1").label("Primary").primary().size("small"))
          .child(Button.new("b2").label("Outline").outline().size("small"))
          .child(Button.new("b3").label("Ghost").ghost().size("small"))
          .child(Button.new("b4").label("Danger").danger().size("small"))
          .child(Button.new("b5").label("Saving").loading(true).size("small"))))
      .child(this.section("Toggles", "Switch, Checkbox, Rating",
        h_flex().gap_4().items_center()
          .child(Switch.new("sw").label("Compact").checked(this.compact)
            .on_change((v, cx) => { this.compact = v; cx.notify(); }))
          .child(Checkbox.new("cb").label("Show hints").checked(this.hints)
            .on_change((v, cx) => { this.hints = v; cx.notify(); }))
          .child(Rating.new("rt").value(this.rating).size("small")
            .on_change((v, cx) => { this.rating = v; cx.notify(); }))))
      .child(this.section("Slider and progress", `${this.sliderValue}%`,
        // gpui-base parts draw nothing themselves: style each one.
        Slider.new(this.slider).child(
          SliderTrack.new(this.slider).flex().items_center().h(20).w_full().child(
            SliderIndicator.new(this.slider).relative().w_full().h(4).rounded(2).bg(c.border)
              .range_style((fill) => fill.rounded(2).bg(c.accent))
              .child(SliderThumb.new(this.slider).size(14).rounded(7).bg(c.foreground).ml(-7)))),
        Progress.new("pg").value(this.sliderValue)))
      .child(this.section("Input", "InputState, live filter over listFiles()",
        div().px_2().py_1().rounded_md().border_1().border_color(c.border).child(Input.new(this.input)),
        v_flex().gap_1().children(shown.map((p, i) =>
          div().id(`hit-${i}`).text_xs().cursor_pointer().hover((s) => s.text_color(c.accent))
            .on_click(() => openFile(p)).child(p)))))
      .child(this.section("Labels", "Tag, Badge, Kbd, Avatar",
        h_flex().gap_2().items_center().flex_wrap()
          .child(Tag.new().variant("success").size("small").child("passing"))
          .child(Tag.new().variant("warning").size("small").child("3 warnings"))
          .child(Tag.new().variant("danger").outline().size("small").child("blocked"))
          .child(Badge.new().count(7).child(Button.new("inbox").label("Inbox").ghost().size("small")))
          .child(Kbd.new("ctrl-shift-a"))
          .child(Avatar.new().name("pi").size("small"))
          .child(Avatar.new().name("Ondřej").size("small"))))
      .child(this.section("Status", "Alert, Spinner, Skeleton, ShimmerText",
        Alert.new("al", "Script panels can show any of these in pide's theme.").title("Heads up"),
        h_flex().gap_3().items_center()
          .child(Spinner.new().size("small"))
          .child(ShimmerText.new("pi is thinking…"))
          .child(Skeleton.new().w("120px").h("12px").rounded_md())));
  }

  // ── Charts: built from live project data ────────────────────────────────
  charts(c) {
    const lines = this.lines || [];
    const fileRows = [...lines].sort((a, b) => b.n - a.n).slice(0, 6)
      .map((f) => ({ label: f.path.split("/").pop().replace(/\.py$/, ""), value: f.n }));
    const byDir = new Map();
    for (const f of lines) {
      const dir = f.path.includes("/") ? f.path.split("/")[0] : ".";
      byDir.set(dir, (byDir.get(dir) || 0) + f.n);
    }
    const dirRows = [...byDir.entries()].map(([label, value]) => ({ label, value }));
    const lineRows = [3, 5, 4, 8, 6, 9, 7].map((value, i) => ({ label: `d${i + 1}`, value }));
    return v_flex().gap_4()
      .child(this.section("Lines per file", "BarChart over listFiles() + readFile()",
        div().h("150px").child(BarChart.new(() => fileRows).grid(false).value_axis(true))))
      .child(this.section("Lines per folder", "PieChart",
        div().h("150px").child(PieChart.new(() => dirRows).inner_radius(30).pad_angle(0.04).labels(true))))
      .child(this.section("A trend", "LineChart",
        div().h("120px").child(LineChart.new(() => lineRows).dot().natural().grid(false))));
  }

  // ── System: files, processes, network, storage, timers ──────────────────
  system(c) {
    const mono = (t) => div().text_xs().font_family("monospace").text_color(c.muted).child(t);
    return v_flex().gap_4()
      .child(this.section("Files", "fs/promises: readdir(root)",
        h_flex().gap_2().flex_wrap().children(this.entries.map((e) =>
          Tag.new().size("xsmall").variant(e.isDirectory() ? "info" : "secondary").child(e.name)))))
      .child(this.section("Processes", "process.run — allowed: git",
        mono(this.gitLog)))
      .child(this.section("Network", "fetch — no hosts granted",
        mono(this.fetched)))
      .child(this.section("Storage", "window.localStorage, per panel, survives restarts",
        h_flex().gap_2().items_center()
          .child(Button.new("click").label(`Clicked ${this.clicks}×`).size("small").outline()
            .on_click((_, cx) => { this.clicks++; window.localStorage.setItem("clicks", String(this.clicks)); cx.notify(); }))
          .child(div().text_xs().text_color(c.muted).child("close pide and it remembers"))))
      .child(this.section("Timers", "cx.spawn + cx.sleep",
        div().text_base().font_weight(600).child(this.now.toTimeString().slice(0, 8))));
  }
}
