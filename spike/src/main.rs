//! M0 spike (throwaway). Opens one large Rust file in the GPUI Kit editor with
//! tree-sitter highlighting and measures what M0 asks for.
//!
//!   cargo run --release -- [FILE] [--bench]
//!
//! Without `--bench` the window stays open for manual use; on close it prints
//! GPUI's own draw-time and input-latency histograms (type and scroll by hand
//! to fill them). With `--bench` it runs a scripted scroll + typing pass, prints
//! a report and quits.
//!
//! Modelled on reference/gpui-kit-v0.9.0/{editor,large-text,hello_world}.

use std::time::{Duration, Instant};

use gpui_kit::component::input::{Editor, EditorState, InputEvent, Position, TabSize};
use gpui_kit::component::{ActiveTheme, h_flex, v_flex};
use gpui_kit::*;

const DEFAULT_FILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/large.rs");

struct Spike {
    editor: Entity<EditorState>,
    path: String,
    lines: usize,
    status: SharedString,
    _subs: Vec<Subscription>,
    _bench: Option<Task<()>>,
}

/// Per-phase numbers taken from GPUI's frame trace.
#[derive(Default)]
struct PhaseStats {
    name: &'static str,
    draws: Vec<Duration>,
    dirty_to_draw_end: Vec<Duration>,
    present_intervals: Vec<Duration>,
    op_times: Vec<Duration>,
}

fn pct(v: &mut [Duration], p: f64) -> Duration {
    if v.is_empty() {
        return Duration::ZERO;
    }
    v.sort();
    let i = ((v.len() as f64 - 1.0) * p).round() as usize;
    v[i]
}

fn ms(d: Duration) -> String {
    format!("{:.2}", d.as_secs_f64() * 1000.0)
}

impl PhaseStats {
    fn absorb(&mut self, events: Vec<FrameEvent>) {
        for e in events {
            match e {
                FrameEvent::Draw(t) => {
                    self.draws.push(t.draw_end.duration_since(t.draw_start));
                    if let Some(d) = t.dirty_at {
                        self.dirty_to_draw_end.push(t.draw_end.duration_since(d));
                    }
                }
                FrameEvent::Present(p) => {
                    if let Some(i) = p.animation_interval {
                        self.present_intervals.push(i);
                    }
                }
            }
        }
    }

    fn report(&mut self) -> String {
        let n = self.draws.len();
        let over16 = self.draws.iter().filter(|d| d.as_millis() > 16).count();
        let over32 = self.draws.iter().filter(|d| d.as_millis() > 32).count();
        let max = self.draws.iter().max().copied().unwrap_or_default();
        let mut s = format!(
            "## {}\nframes drawn: {n}\ndraw ms   p50 {} | p95 {} | p99 {} | max {} | >16ms {over16} | >32ms {over32}\n",
            self.name,
            ms(pct(&mut self.draws, 0.5)),
            ms(pct(&mut self.draws, 0.95)),
            ms(pct(&mut self.draws, 0.99)),
            ms(max),
        );
        if !self.dirty_to_draw_end.is_empty() {
            s += &format!(
                "change->drawn ms   p50 {} | p95 {} | max {}\n",
                ms(pct(&mut self.dirty_to_draw_end, 0.5)),
                ms(pct(&mut self.dirty_to_draw_end, 0.95)),
                ms(pct(&mut self.dirty_to_draw_end, 1.0)),
            );
        }
        if !self.present_intervals.is_empty() {
            let over = self
                .present_intervals
                .iter()
                .filter(|d| d.as_secs_f64() > 1.5 / 60.0)
                .count();
            s += &format!(
                "present interval ms   p50 {} | p95 {} | max {} | intervals >25ms {over}/{}\n",
                ms(pct(&mut self.present_intervals, 0.5)),
                ms(pct(&mut self.present_intervals, 0.95)),
                ms(pct(&mut self.present_intervals, 1.0)),
                self.present_intervals.len(),
            );
        }
        if !self.op_times.is_empty() {
            s += &format!(
                "editor call (insert/scroll) ms   p50 {} | p95 {} | max {}\n",
                ms(pct(&mut self.op_times, 0.5)),
                ms(pct(&mut self.op_times, 0.95)),
                ms(pct(&mut self.op_times, 1.0)),
            );
        }
        s
    }
}

fn rss_mb() -> f64 {
    // Throwaway: shell out to ps; good enough on macOS and Linux.
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output();
    out.ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<f64>().ok())
        .map(|kb| kb / 1024.0)
        .unwrap_or(-1.0)
}

impl Spike {
    fn new(
        text: String,
        path: String,
        bench: bool,
        started: Instant,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let lines = text.lines().count();
        let t0 = Instant::now();
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("rust".to_string())
                .line_number(true)
                .indent_guides(true)
                .tab_size(TabSize {
                    tab_size: 4,
                    hard_tabs: false,
                })
                .soft_wrap(false)
                .default_value(text)
        });
        let construct = t0.elapsed();
        eprintln!(
            "[spike] EditorState::new with {lines} lines took {} ms (rss {:.0} MB)",
            ms(construct),
            rss_mb()
        );

        let focus = editor.focus_handle(cx);
        window.defer(cx, move |window, cx| focus.focus(window, cx));

        let subs = vec![cx.subscribe(&editor, |_, _, _: &InputEvent, cx| cx.notify())];

        window.on_next_frame(move |_, _| {
            eprintln!(
                "[spike] process start -> first frame: {} ms",
                ms(started.elapsed())
            );
        });

        let mut this = Self {
            editor,
            path,
            lines,
            status: "manual mode: type and scroll; close window for stats".into(),
            _subs: subs,
            _bench: None,
        };
        if bench {
            this.status = "benchmark running…".into();
            this._bench = Some(Self::run_bench(window, cx));
        }
        this
    }

    fn run_bench(window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        set_trace_enabled(true);
        cx.spawn_in(window, async move |this, cx| {
            let tick = || Duration::from_millis(16);
            let mut out = String::new();
            out += &format!("# pide M0 spike results\n\nos: {} {}\n", std::env::consts::OS, std::env::consts::ARCH);

            // Let the first layout + initial tree-sitter parse finish.
            let rss_before = rss_mb();
            let mut collector = FrameTimingCollector::new();
            let mut idle = PhaseStats { name: "settle (3 s idle after open, includes initial parse)", ..Default::default() };
            cx.background_executor().timer(Duration::from_secs(3)).await;
            idle.absorb(collector.collect_unseen());
            out += &idle.report();
            out += &format!("rss after open+parse: {:.0} MB (at start of settle: {:.0} MB)\n\n", rss_mb(), rss_before);

            let Ok(editor) = this.read_with(cx, |this, _| this.editor.clone()) else { return };
            let line_h = editor
                .read_with(cx, |e, _| e.line_height())
                .unwrap_or(px(20.));

            // Phase 1: smooth scroll, ~3 lines per frame, top -> 3000 lines down.
            let mut smooth = PhaseStats { name: "smooth scroll (3 lines/frame, 1000 frames)", ..Default::default() };
            collector.collect_unseen();
            for i in 0..1000 {
                let y = line_h * (3.0 * i as f32);
                let t = Instant::now();
                let _ = editor.update(cx, |e, cx| e.set_scroll_offset(point(px(0.), -y), cx));
                smooth.op_times.push(t.elapsed());
                cx.background_executor().timer(tick()).await;
                smooth.absorb(collector.collect_unseen());
            }
            out += &smooth.report();
            out += "\n";

            // Phase 2: fast fling, 60 lines per frame across the whole file.
            let mut fling = PhaseStats { name: "fast scroll (60 lines/frame, whole file)", ..Default::default() };
            let total = this.read_with(cx, |this, _| this.lines).unwrap_or(50_000);
            let steps = (total / 60).max(1);
            for i in 0..steps {
                let y = line_h * (60.0 * i as f32);
                let t = Instant::now();
                let _ = editor.update(cx, |e, cx| e.set_scroll_offset(point(px(0.), -y), cx));
                fling.op_times.push(t.elapsed());
                cx.background_executor().timer(tick()).await;
                fling.absorb(collector.collect_unseen());
            }
            out += &fling.report();
            out += "\n";

            // Phase 3: typing in the middle of the file, one char per frame.
            let mut typing = PhaseStats { name: "typing (400 chars at line 25000, 1 char / 16 ms)", ..Default::default() };
            let _ = this.update_in(cx, |this, window, cx| {
                this.editor.update(cx, |e, cx| {
                    e.set_cursor_position(Position::new(25_000, 0), window, cx);
                });
            });
            cx.background_executor().timer(Duration::from_millis(500)).await;
            collector.collect_unseen();
            let sample = "let spike_value = compute(alpha, beta) + 42; // typing\n";
            let chars: Vec<char> = sample.chars().cycle().take(400).collect();
            for ch in chars {
                let t = Instant::now();
                let _ = this.update_in(cx, |this, window, cx| {
                    this.editor.update(cx, |e, cx| e.insert(ch.to_string(), window, cx));
                });
                typing.op_times.push(t.elapsed());
                cx.background_executor().timer(tick()).await;
                typing.absorb(collector.collect_unseen());
            }
            out += &typing.report();

            // Phase 4: a large paste (1000 lines) to stress reparse/highlight.
            let mut paste = PhaseStats { name: "paste 1000 lines at cursor", ..Default::default() };
            cx.background_executor().timer(Duration::from_millis(500)).await;
            collector.collect_unseen();
            let block = "fn pasted(x: u32) -> u32 { x.wrapping_mul(31) ^ 0xdead_beef }\n".repeat(1000);
            let t = Instant::now();
            let _ = this.update_in(cx, |this, window, cx| {
                this.editor.update(cx, |e, cx| e.insert(block, window, cx));
            });
            paste.op_times.push(t.elapsed());
            cx.background_executor().timer(Duration::from_secs(2)).await;
            paste.absorb(collector.collect_unseen());
            out += "\n";
            out += &paste.report();

            out += &format!("\nrss at end: {:.0} MB\n", rss_mb());

            // GPUI's own cumulative histograms for the window.
            let _ = this.update_in(cx, |_, window, _| {
                out += &histograms(window);
            });

            println!("{out}");
            let file = format!(
                "{}/results-{}-{}.md",
                env!("CARGO_MANIFEST_DIR"),
                std::env::consts::OS,
                std::env::consts::ARCH
            );
            let _ = std::fs::write(&file, &out);
            eprintln!("[spike] wrote {file}");
            let _ = this.update(cx, |this, cx| {
                this.status = "benchmark done".into();
                cx.notify();
            });
            let _ = cx.update(|_, cx| cx.quit());
        })
    }
}

fn histograms(window: &Window) -> String {
    let f = window.frame_duration_snapshot();
    let l = window.input_latency_snapshot();
    // hdrhistogram isn't re-exported by gpui_kit, so stay generic over the type.
    macro_rules! h {
        ($name:expr, $h:expr) => {{
            let h = &$h;
            if h.is_empty() {
                format!("{}: no samples\n", $name)
            } else {
                let v = |p: f64| ms(Duration::from_nanos(h.value_at_quantile(p)));
                format!(
                    "{}: n {} | p50 {} | p95 {} | p99 {} | max {}\n",
                    $name,
                    h.len(),
                    v(0.5),
                    v(0.95),
                    v(0.99),
                    ms(Duration::from_nanos(h.max()))
                )
            }
        }};
    }
    let mut s = String::from("\n## GPUI window histograms (whole run)\n");
    s += &h!("draw duration ms", f.draw_duration_histogram);
    s += &h!("dirty->present ms", f.dirty_to_present_histogram);
    s += &h!("present interval ms", f.present_interval_histogram);
    s += &h!("input->frame latency ms (real OS input only)", l.latency_histogram);
    s
}

impl Render for Spike {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pos = self.editor.read(cx).cursor_position();
        v_flex()
            .size_full()
            .child(
                Editor::new(&self.editor)
                    .bordered(false)
                    .p_0()
                    .flex_1()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(cx.theme().mono_font_size),
            )
            .child(
                h_flex()
                    .justify_between()
                    .px_3()
                    .py_1()
                    .text_sm()
                    .bg(cx.theme().secondary)
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{} — {} lines — {}", self.path, self.lines, self.status))
                    .child(format!("{}:{}", pos.line + 1, pos.character + 1)),
            )
    }
}

fn main() {
    let started = Instant::now();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bench = args.iter().any(|a| a == "--bench");
    let path = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .unwrap_or_else(|| DEFAULT_FILE.to_string());
    let t = Instant::now();
    let text = std::fs::read_to_string(&path).expect("read file");
    eprintln!("[spike] read {} bytes in {} ms", text.len(), ms(t.elapsed()));

    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        cx.activate(true);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(1200.), px(800.)),
                cx,
            ))),
            ..Default::default()
        };
        let (window, _view) = gpui_kit::open_window(options, cx, move |window, cx| {
            cx.new(|cx| Spike::new(text, path, bench, started, window, cx))
        })
        .expect("open window");

        if !bench {
            set_trace_enabled(true);
            let _ = window.update(cx, |_, window, cx| {
                window.on_window_should_close(cx, |window, _| {
                    eprint!("{}", histograms(window));
                    true
                });
            });
        }
    });
}
