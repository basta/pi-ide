// What pi did in a session, in one place: its checklist, the files it changed
// and why, the notes it left (click to jump), and the conversation.
// Needs "permissions": { "pi": true } in panel.json.
import { View, div } from "gpui-kit";
import { h_flex, v_flex } from "gpui-base";
import { Tag, Progress, TabBar, Tab } from "gpui-component";
import {
  piSessions, piChanges, piNotes, piProgress, piMessages, piSavedSessions, piOpen,
  openFile, nextEvent, colors,
} from "pide";

const TABS = ["Changes", "Notes", "Chat", "Saved"];

export default class PiSession extends View {
  init(_props, cx) {
    this.tab = 0;
    this.pick = null; // session id; null = the active one
    this.file = null; // a saved session picked on the Saved tab
    this.sessions = [];
    this.changes = [];
    this.notes = [];
    this.steps = [];
    this.chat = [];
    this.saved = [];
    this.error = null;
    this.refresh = async () => {
      try {
        this.sessions = piSessions();
        const s = this.session();
        this.changes = s ? piChanges(s.id) : [];
        this.notes = s ? piNotes(s.id) : [];
        this.steps = s ? piProgress(s.id) : [];
        if (this.tab === 2) this.chat = this.file || s ? await piMessages(this.file || s.id) : [];
        if (this.tab === 3) this.saved = await piSavedSessions();
        this.error = null;
      } catch (e) {
        this.error = String(e);
      }
      cx.notify();
    };
    cx.spawn(async () => {
      await this.refresh();
      for (;;) {
        const e = await nextEvent();
        if (e.kind === "pi") await this.refresh();
      }
    });
  }

  session() {
    const all = this.sessions;
    return all.find((s) => s.id === this.pick) || all.find((s) => s.active) || all[0];
  }

  render() {
    const c = colors();
    if (this.error) return div().p_3().text_sm().text_color(c.danger).child(this.error);
    const s = this.session();
    if (!s) return div().p_3().text_sm().text_color(c.muted).child("No pi session open.");
    const done = this.steps.filter((p) => p.done).length;
    const set = (f) => { f(); this.refresh(); };
    const dot = s.status === "busy" ? c.warning : s.status === "crashed" ? c.danger : c.success;
    return v_flex().size_full().bg(c.background).text_color(c.foreground)
      // One chip per open session.
      .child(h_flex().px_3().pt_3().gap_1().flex_wrap().children(this.sessions.map((x) =>
        h_flex().id(`s${x.id}`).px_2().py_0p5().gap_1().rounded_md().cursor_pointer()
          .bg(x.id === s.id ? c.list_hover : c.background).hover((st) => st.bg(c.list_hover))
          .on_click(() => set(() => { this.pick = x.id; this.file = null; }))
          .child(div().size("6px").rounded_full().bg(x.id === s.id ? dot : c.muted))
          .child(div().text_sm().child(x.name)))))
      .child(h_flex().px_3().pt_1().gap_2().text_xs().text_color(c.muted)
        .child(div().child(s.model || ""))
        .child(div().child(s.cost != null ? `$${s.cost.toFixed(2)}` : ""))
        .child(div().child(s.contextPercent != null ? `${Math.round(s.contextPercent)}% context` : ""))
        .child(div().id("open").text_color(c.pi).cursor_pointer().on_click(() => piOpen(s.id)).child("Open chat")))
      // pi's checklist.
      .child(this.steps.length === 0 ? div() : v_flex().px_3().pt_2().gap_1()
        .child(h_flex().gap_2()
          .child(div().text_xs().text_color(c.muted).child(`Checklist ${done}/${this.steps.length}`))
          .child(div().flex_1().child(Progress.new("checklist").value((100 * done) / this.steps.length))))
        .children(this.steps.map((p, i) => div().id(`p${i}`).text_sm()
          .text_color(p.done ? c.muted : c.foreground).child(`${p.done ? "✓" : "○"} ${p.text}`))))
      .child(div().px_2().pt_2().child(TabBar.new("tabs").variant("underline").size("small").selected_index(this.tab)
        .on_change((i) => set(() => { this.tab = i; }))
        .children(TABS.map((t) => Tab.new().label(t)))))
      .child(v_flex().id("body").flex_1().overflow_y_scroll().px_3().py_2().gap_2()
        .children(this.body(c)));
  }

  body(c) {
    const empty = (t) => [div().text_sm().text_color(c.muted).child(t)];
    if (this.tab === 0) {
      if (!this.changes.length) return empty("No changes yet.");
      return this.changes.map((ch) => v_flex().id(`c${ch.path}`).gap_0p5().cursor_pointer()
        .on_click(() => openFile(ch.path, ch.firstLine || 1))
        .child(h_flex().gap_2()
          .child(div().text_sm().font_weight(600).child(ch.path))
          .child(div().text_xs().text_color(c.success).child(ch.added != null ? `+${ch.added}` : ""))
          .child(div().text_xs().text_color(c.danger).child(ch.removed != null ? `−${ch.removed}` : ""))
          .child(ch.reviewed ? Tag.new().variant("success").size("xsmall").child("reviewed") : div()))
        .child(div().text_sm().text_color(c.muted).child(ch.summary || ch.tools)));
    }
    if (this.tab === 1) {
      const live = this.notes.filter((n) => !n.dismissed);
      if (!live.length) return empty("pi hasn't left notes.");
      return live.map((n) => v_flex().id(`n${n.id}`).p_2().gap_1().rounded_md().border_1().border_color(c.border)
        .cursor_pointer().hover((st) => st.bg(c.list_hover)).on_click(() => openFile(n.path, n.line))
        .child(div().text_xs().text_color(c.muted)
          .child(`${n.kind} · ${n.path}:${n.line}${n.outdated ? " · outdated" : ""}${n.seen ? "" : " · new"}`))
        .child(div().text_sm().child(n.note))
        .children(n.thread.map((r, i) => div().id(`r${n.id}${i}`).text_sm().pl_2().border_l_2()
          .border_color(r.pi ? c.pi : c.border).child(`${r.pi ? "pi" : "You"}: ${r.text}`))));
    }
    if (this.tab === 2) {
      const shown = this.chat.filter((m) => m.role !== "tool");
      if (!shown.length) return empty("No conversation yet.");
      const arg = (a) => (a && (a.path || a.pattern || a.command)) || "";
      return shown.map((m, i) => v_flex().id(`m${i}`).gap_0p5()
        // Name the speaker only when it changes: pi's tool turns run together.
        .child(i > 0 && shown[i - 1].role === m.role ? div() : div().pt_1().text_xs().font_weight(600)
          .text_color(m.role === "user" ? c.info : c.pi).child(m.role === "user" ? "You" : "pi"))
        .child(m.thinking ? div().text_xs().italic().text_color(c.muted).child(m.thinking.slice(0, 300)) : div())
        .child(m.text ? div().text_sm().child(m.text) : div())
        .children((m.tools || []).map((t, j) => div().id(`t${i}.${j}`).text_xs().text_color(c.muted)
          .child(`→ ${t.name} ${arg(t.args)}`))));
    }
    if (!this.saved.length) return empty("No saved sessions for this project.");
    return this.saved.map((x, i) => v_flex().id(`v${i}`).py_1().cursor_pointer().hover((st) => st.bg(c.list_hover))
      .on_click(() => { this.file = x.file; this.tab = 2; this.refresh(); })
      .child(div().text_sm().child(x.name || x.preview.slice(0, 60)))
      .child(div().text_xs().text_color(c.muted)
        .child(new Date(x.modified).toISOString().slice(0, 16).replace("T", " "))));
  }
}
