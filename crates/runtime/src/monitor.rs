//! Logs UI-thread stalls over 16 ms (spec: "Measure this").
//!
//! Two signals:
//! - a foreground timer that measures how late it fires; lateness means the UI
//!   thread was busy with *anything* (event handlers, draws, entity updates).
//!   Only checked while the app is active: macOS throttles background apps;
//! - GPUI's frame trace, which says how long each `Window::draw` took.

use std::time::{Duration, Instant};

use gpui_kit::*;

const BUDGET: Duration = Duration::from_millis(16);
const TICK: Duration = Duration::from_millis(50);

/// Start monitoring. Keep the returned task alive for the app's lifetime.
pub fn start(cx: &mut App) -> Task<()> {
    set_trace_enabled(true);
    let mut frames = FrameTimingCollector::new();
    cx.spawn(async move |cx| {
        loop {
            let before = Instant::now();
            cx.background_executor().timer(TICK).await;
            let late = before.elapsed().saturating_sub(TICK);
            // A hidden or background app gets its timers throttled by the OS (App Nap,
            // occlusion); lateness then isn't our work, so only judge it while active.
            let active = cx.update(|cx| cx.active_window().is_some());
            if active && late > BUDGET {
                log::warn!("ui thread blocked ~{} ms", late.as_millis());
            }
            for event in frames.collect_unseen() {
                if let FrameEvent::Draw(t) = event {
                    let draw = t.draw_end.duration_since(t.draw_start);
                    if draw > BUDGET {
                        log::warn!("slow frame: draw took {:.1} ms", draw.as_secs_f64() * 1e3);
                    }
                }
            }
        }
    })
}
