// A living picture of the project, drawn natively: health ring, import graph
// that pulses when a file changes, commit heatmap, live edit sparkline, and
// the README as rich text. No HTML: paths, divs and TextView.
import { View, div, PathBuilder, Background } from "gpui-kit";
import { h_flex, v_flex, TextView } from "gpui-base";
import { run } from "process";
import { root, listFiles, readFile, openFile, diagnostics, nextEvent, colors } from "pide";

const WEEKS = 22;
const BUCKETS = 40; // live sparkline: 40 × 1.5 s
const now = () => Date.now();

// ── geometry helpers ────────────────────────────────────────────────────
function arc(cx, cy, r, from, to) {
  // Angles in turns, 0 = top, clockwise. Split long arcs so arc_to stays exact.
  const pt = (t) => [cx + r * Math.sin(2 * Math.PI * t), cy - r * Math.cos(2 * Math.PI * t)];
  const b = PathBuilder.stroke(7);
  const [x0, y0] = pt(from);
  b.move_to(x0, y0);
  const steps = Math.max(1, Math.ceil((to - from) / 0.25));
  for (let i = 1; i <= steps; i++) {
    const [x, y] = pt(from + ((to - from) * i) / steps);
    b.arc_to(r, r, 0, false, true, x, y);
  }
  return b.build();
}

// Force-directed layout in the unit square: springs on imports, repulsion
// between all nodes. Run once; positions are percentages afterwards.
function layout(nodes, edges) {
  let rnd = 1;
  const rand = () => ((rnd = (rnd * 16807) % 2147483647) / 2147483647);
  nodes.forEach((n, i) => { n.x = 0.5 + 0.3 * Math.cos(i * 2.4); n.y = 0.5 + 0.3 * Math.sin(i * 2.4); });
  for (let it = 0; it < 400; it++) {
    const k = 0.04 * (1 - it / 400);
    for (const a of nodes) for (const b of nodes) {
      if (a === b) continue;
      const dx = a.x - b.x, dy = a.y - b.y, d2 = dx * dx + dy * dy + 1e-4;
      a.x += (k * 0.06 * dx) / d2; a.y += (k * 0.06 * dy) / d2;
    }
    for (const [s, t] of edges) {
      const a = nodes[s], b = nodes[t], dx = b.x - a.x, dy = b.y - a.y;
      a.x += dx * k * 0.35; a.y += dy * k * 0.35; b.x -= dx * k * 0.35; b.y -= dy * k * 0.35;
    }
    for (const n of nodes) { n.x += (0.5 - n.x) * k * 0.6 + (rand() - 0.5) * k * 0.01; n.y += (0.5 - n.y) * k * 0.6; }
  }
  // Fit into 12%..88% so labels stay inside.
  const xs = nodes.map((n) => n.x), ys = nodes.map((n) => n.y);
  const [x0, x1, y0, y1] = [Math.min(...xs), Math.max(...xs), Math.min(...ys), Math.max(...ys)];
  for (const n of nodes) {
    n.x = 12 + (76 * (n.x - x0)) / Math.max(1e-6, x1 - x0);
    n.y = 12 + (70 * (n.y - y0)) / Math.max(1e-6, y1 - y0);
  }
}

export default class CodebaseMap extends View {
  init(_props, cx) {
    this.nodes = [];
    this.edges = [];
    this.days = new Map();
    this.commits = 0;
    this.readme = null;
    this.hover = null;
    this.cell = null;
    this.pulses = new Map(); // path -> time it changed
    this.spark = new Array(BUCKETS).fill(0);
    this.born = now();
    this.animating = false;

    cx.spawn(async (cx) => {
      const files = (await listFiles()).filter((p) => p.endsWith(".py"));
      const index = new Map(files.map((p, i) => [p, i]));
      this.nodes = [];
      const edges = [];
      for (const path of files) {
        const text = await readFile(path);
        this.nodes.push({ path, name: path.split("/").pop().replace(/\.py$/, ""), lines: text.split("\n").length, test: path.startsWith("tests/") });
        for (const m of text.matchAll(/^\s*(?:from\s+([\w.]+)\s+import|import\s+([\w.]+))/gm)) {
          const target = `${(m[1] || m[2]).replace(/\./g, "/")}.py`;
          if (index.has(target)) edges.push([index.get(path), index.get(target)]);
        }
      }
      this.edges = edges;
      layout(this.nodes, this.edges);
      try { this.readme = await readFile("README.md"); } catch (_) { this.readme = null; }
      const log = await run("git", ["log", "--format=%ad", "--date=short"], { cwd: root() });
      for (const d of log.stdout.split("\n").filter(Boolean)) this.days.set(d, (this.days.get(d) || 0) + 1);
      this.commits = log.stdout.split("\n").filter(Boolean).length;
      this.born = now();
      this.animate(cx, 1200);
      cx.notify();
    });

    // Live: pulse a node when its file changes, count edits for the sparkline.
    cx.spawn(async (cx) => {
      for (;;) {
        const e = await nextEvent();
        if ((e.kind === "changed" || e.kind === "saved") && e.path) {
          this.pulses.set(e.path, now());
          if (e.kind === "changed") this.spark[BUCKETS - 1]++;
          this.animate(cx, 1600);
        }
        if (e.kind === "diagnostics") cx.notify();
      }
    });
    cx.spawn(async (cx) => {
      for (;;) { await cx.sleep(1500); this.spark = [...this.spark.slice(1), 0]; cx.notify(); }
    });
  }

  // Re-render at ~60 fps until `ms` from now; the shell redraws natively between.
  animate(cx, ms) {
    this.until = Math.max(this.until || 0, now() + ms);
    if (this.animating) return;
    this.animating = true;
    cx.spawn(async (cx) => {
      while (now() < this.until) { await cx.sleep(16); cx.notify(); }
      this.animating = false;
      cx.notify();
    });
  }

  render() {
    const c = colors();
    this.c = c;
    return v_flex().id("map").size_full().overflow_y_scroll().bg(c.background).text_color(c.foreground)
      .child(this.header(c))
      .child(this.card("Imports", "hover a file to trace it · click to open · edits pulse", this.graph(c)))
      .child(this.card("Commits", this.cell ? `${this.cell.date}: ${this.cell.n} commit${this.cell.n === 1 ? "" : "s"}` : `last ${WEEKS} weeks`, this.heatmap(c)))
      .child(this.card("Typing", "edits per 1.5 s, last minute", this.sparkline(c)))
      .child(this.readme ? this.card("README.md", "TextView.markdown", div().text_sm().child(TextView.markdown("readme", this.readme).selectable(true))) : div());
  }

  card(title, note, body) {
    const c = this.c;
    return v_flex().mx_3().mb_3().p_3().gap_2().rounded_lg().border_1().border_color(c.border)
      .child(h_flex().gap_2().child(div().text_sm().font_weight(600).child(title))
        .child(div().flex_1().min_w_0().text_xs().text_color(c.muted).text_ellipsis().child(note)))
      .child(body);
  }

  // ── health ring + numbers ───────────────────────────────────────────────
  header(c) {
    const d = diagnostics();
    const broken = new Set(d.filter((x) => x.severity === "error").map((x) => x.path));
    const total = Math.max(1, this.nodes.length);
    const healthy = (total - broken.size) / total;
    const t = Math.min(1, (now() - this.born) / 900);
    const ease = 1 - Math.pow(1 - t, 3);
    const shown = healthy * ease;
    const tone = healthy > 0.85 ? c.success : healthy > 0.6 ? c.warning : c.danger;
    const lines = this.nodes.reduce((n, f) => n + f.lines, 0);
    const stat = (n, label) => v_flex().child(div().text_base().font_weight(600).child(String(n)))
      .child(div().text_xs().text_color(c.muted).child(label));
    return h_flex().px_3().pt_3().pb_3().gap_4().items_center()
      .child(div().relative().size(96).flex_none()
        .child(window.paint_path(arc(48, 48, 40, 0, 0.9999), Background.solid(c.border)).absolute().inset_0())
        .child(shown > 0.002 ? window.paint_path(arc(48, 48, 40, 0, shown), Background.solid(tone)).absolute().inset_0() : div())
        .child(v_flex().absolute().inset_0().items_center().justify_center()
          .child(div().text_base().font_weight(600).child(`${Math.round(shown * 100)}%`))
          .child(div().text_xs().text_color(c.muted).child("error-free"))))
      .child(v_flex().gap_2()
        .child(h_flex().gap_5().child(stat(this.nodes.length, "Python files")).child(stat(lines, "lines")))
        .child(h_flex().gap_5().child(stat(this.edges.length, "imports")).child(stat(this.commits, "commits"))));
  }

  // ── import graph: paths for edges, divs for nodes ───────────────────────
  graph(c) {
    const errs = new Set(diagnostics().filter((x) => x.severity === "error").map((x) => x.path));
    const h = this.hover;
    const touches = (s, t) => h !== null && (s === h || t === h);
    const edgePath = (s, t) => {
      const a = this.nodes[s], b = this.nodes[t];
      const mx = (a.x + b.x) / 2 + (b.y - a.y) * 0.15, my = (a.y + b.y) / 2 - (b.x - a.x) * 0.15;
      return window.paint_path(
        PathBuilder.stroke(touches(s, t) ? 2 : 1).move_to(`${a.x}%`, `${a.y}%`).curve_to(`${b.x}%`, `${b.y}%`, `${mx}%`, `${my}%`).build(),
        Background.solid(touches(s, t) ? c.pi : `${c.muted}66`)).absolute().inset_0();
    };
    const maxLines = Math.max(1, ...this.nodes.map((n) => n.lines));
    return div().relative().w_full().h(260).rounded_md().bg(`${c.border}40`)
      // Edges under the nodes; the hovered file's edges drawn last, on top.
      .children(this.edges.filter(([s, t]) => !touches(s, t)).map(([s, t]) => edgePath(s, t)))
      .children(this.edges.filter(([s, t]) => touches(s, t)).map(([s, t]) => edgePath(s, t)))
      .children(this.nodes.map((n, i) => {
        const r = 7 + 11 * Math.sqrt(n.lines / maxLines);
        const since = now() - (this.pulses.get(n.path) || -1e9);
        const pulse = Math.max(0, 1 - since / 1500);
        const base = errs.has(n.path) ? c.danger : n.test ? c.muted : c.info;
        const dim = h !== null && h !== i && !this.edges.some(([s, t]) => touches(s, t) && (s === i || t === i));
        return v_flex().id(`n-${i}`).absolute().left(`${n.x}%`).top(`${n.y}%`).ml(-40).mt(-r).w(80).items_center()
          .opacity(dim ? 0.35 : 1).cursor_pointer()
          .on_hover((on, cx) => { this.hover = on ? i : (this.hover === i ? null : this.hover); cx.notify(); })
          .on_click(() => openFile(n.path))
          .child(div().size(2 * r).rounded_full().bg(base).border_2().border_color(c.background)
            .shadow(pulse > 0 ? [{ blur: 6 + 18 * pulse, spread: 2 + 8 * pulse, color: `${c.pi}${Math.round(pulse * 200).toString(16).padStart(2, "0")}` }] : []))
          .child(div().mt_1().text_xs().whitespace_nowrap().text_color(h === i ? c.foreground : c.muted).child(n.name));
      }));
  }

  // ── commit heatmap ──────────────────────────────────────────────────────
  heatmap(c) {
    const today = new Date();
    const end = new Date(today); end.setDate(end.getDate() + (6 - end.getDay()));
    const max = Math.max(1, ...this.days.values());
    const cols = [];
    let lastMonth = -1;
    const months = [];
    for (let w = WEEKS - 1; w >= 0; w--) {
      const cells = [];
      for (let d = 0; d < 7; d++) {
        const day = new Date(end); day.setDate(end.getDate() - w * 7 - (6 - d));
        const key = day.toISOString().slice(0, 10);
        const n = this.days.get(key) || 0;
        const level = n === 0 ? 0 : Math.ceil((4 * n) / max);
        const alpha = ["", "50", "88", "c0", "ff"][level];
        const future = day > today;
        cells.push(div().id(`c-${key}`).size(13).rounded_sm()
          .bg(future ? c.background : n === 0 ? `${c.border}` : `${c.success}${alpha}`)
          .on_hover((on, cx) => { this.cell = on ? { date: key, n } : null; cx.notify(); }));
        if (d === 0) months.push(day.getMonth() !== lastMonth ? "Jan Feb Mar Apr May Jun Jul Aug Sep Oct Nov Dec".split(" ")[day.getMonth()] : "");
        if (d === 0) lastMonth = day.getMonth();
      }
      cols.push(v_flex().gap(3).children(cells));
    }
    return v_flex().gap_1()
      .child(h_flex().gap(3).children(months.map((m) => div().w(13).text_xs().text_color(c.muted).whitespace_nowrap().child(m))))
      .child(h_flex().gap(3).children(cols));
  }

  // ── live sparkline: a gradient area under a line ────────────────────────
  sparkline(c) {
    const max = Math.max(3, ...this.spark);
    const pts = this.spark.map((v, i) => [`${(100 * i) / (BUCKETS - 1)}%`, `${96 - (86 * v) / max}%`]);
    const line = PathBuilder.stroke(2);
    pts.forEach(([x, y], i) => (i ? line.line_to(x, y) : line.move_to(x, y)));
    const area = PathBuilder.fill().add_polygon([["0%", "100%"], ...pts, ["100%", "100%"]], true).build();
    const sum = this.spark.reduce((a, b) => a + b, 0);
    return v_flex().gap_1()
      .child(div().relative().w_full().h(56)
        .child(window.paint_path(area, Background.linear_gradient(180, Background.stop(`${c.info}66`, 0), Background.stop(`${c.info}00`, 1))).absolute().inset_0())
        .child(window.paint_path(line.build(), Background.solid(c.info)).absolute().inset_0()))
      .child(div().text_xs().text_color(c.muted).child(sum ? `${sum} edits in the last minute` : "type in any file…"));
  }
}
