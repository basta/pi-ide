//! Driving pide from the background, for testing without taking over the
//! screen (build with `--features devctl`, run with `PIDE_CTL=<socket path>`).
//! Windows open hidden and the app never activates; `dev/pidectl` sends one
//! JSON request per line over the Unix socket and prints the JSON reply.
//!
//! Requests (`window` picks a window by index, default 0; coordinates are
//! window points from the top left):
//! - `{"cmd":"windows"}`: index, title and size of each window
//! - `{"cmd":"run","id":"git.log"}`: run a command
//! - `{"cmd":"keys","keys":"cmd-9 down enter"}`: keystrokes, space-separated
//! - `{"cmd":"type","text":"hello"}`: text into the focused input
//! - `{"cmd":"click","x":400,"y":300,"button":"left|right","count":1,"mods":"cmd"}`
//! - `{"cmd":"move","x":400,"y":300}`, `{"cmd":"scroll","x":..,"y":..,"dy":-120}`
//! - `{"cmd":"shot","path":"/tmp/p.bmp"}`: draw the window now and save it as a
//!   BMP (pidectl converts it to PNG with `sips`)
//!
//! Every request draws the window first, so clicks hit what a shot shows.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use gpui_kit::*;
use serde_json::{Value, json};

type Request = (Value, mpsc::Sender<Value>);

/// The socket path when pide runs under control.
pub fn socket() -> Option<PathBuf> {
    std::env::var_os("PIDE_CTL").map(PathBuf::from)
}

pub fn start(cx: &mut App) {
    let Some(path) = socket() else { return };
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            log::error!("devctl: bind {}: {e}", path.display());
            return;
        }
    };
    log::info!("devctl: listening on {}", path.display());
    let (tx, rx) = mpsc::channel::<Request>();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let Ok(read) = stream.try_clone() else { return };
                let mut out = stream;
                for line in BufReader::new(read).lines() {
                    let Ok(line) = line else { break };
                    if line.trim().is_empty() {
                        continue;
                    }
                    let reply = match serde_json::from_str::<Value>(&line) {
                        Ok(req) => {
                            let (rtx, rrx) = mpsc::channel();
                            if tx.send((req, rtx)).is_err() {
                                break;
                            }
                            rrx.recv_timeout(Duration::from_secs(30))
                                .unwrap_or_else(|_| json!({"error": "timed out"}))
                        }
                        Err(e) => json!({"error": format!("bad json: {e}")}),
                    };
                    if writeln!(out, "{reply}").is_err() {
                        break;
                    }
                }
            });
        }
    });
    // Requests are handled on the UI thread, polled (this is a test hook).
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(15))
                .await;
            while let Ok((req, reply)) = rx.try_recv() {
                // Settle first, like real frames would: draw, then let other
                // UI tasks run (a dialog opened from a task, its fade-in, the
                // focus it moves to its input once its content is built).
                for _ in 0..3 {
                    cx.update(|cx| settle(&req, cx));
                    cx.background_executor()
                        .timer(Duration::from_millis(16))
                        .await;
                }
                let _ = reply.send(cx.update(|cx| handle(&req, cx)));
            }
        }
    })
    .detach();
}

/// Draw the request's window (see the loop in `start`).
fn settle(req: &Value, cx: &mut App) {
    let index = req["window"].as_u64().unwrap_or(0) as usize;
    if let Some(handle) = cx.windows().get(index).copied() {
        let _ = handle.update(cx, |_, window, cx| redraw(window, cx));
    }
}

fn handle(req: &Value, cx: &mut App) -> Value {
    match run(req, cx) {
        Ok(v) => v,
        Err(e) => json!({"error": format!("{e:#}")}),
    }
}

fn run(req: &Value, cx: &mut App) -> anyhow::Result<Value> {
    let cmd = req["cmd"].as_str().unwrap_or("");
    let windows = cx.windows();
    if cmd == "windows" {
        let mut list = Vec::new();
        for (i, w) in windows.iter().enumerate() {
            let info = w.update(cx, |_, window, _| {
                let b = window.bounds();
                json!({
                    "index": i,
                    "title": window.window_title(),
                    "width": f32::from(b.size.width),
                    "height": f32::from(b.size.height),
                })
            });
            if let Ok(info) = info {
                list.push(info);
            }
        }
        return Ok(json!({ "windows": list }));
    }
    let index = req["window"].as_u64().unwrap_or(0) as usize;
    let handle = *windows
        .get(index)
        .ok_or_else(|| anyhow::anyhow!("no window {index}"))?;
    handle.update(cx, |_, window, cx| -> anyhow::Result<Value> {
        let point = || {
            point(
                px(req["x"].as_f64().unwrap_or(0.) as f32),
                px(req["y"].as_f64().unwrap_or(0.) as f32),
            )
        };
        let mods = || modifiers(req["mods"].as_str().unwrap_or(""));
        let result = match cmd {
            "run" => {
                let id = req["id"].as_str().unwrap_or("");
                json!({ "ran": pide_core::CommandRegistry::run(id, window, cx) })
            }
            "keys" => {
                let mut handled = Vec::new();
                for k in req["keys"].as_str().unwrap_or("").split_whitespace() {
                    let ks = Keystroke::parse_with_os(k, OperatingSystem::Mac)
                        .map_err(|e| anyhow::anyhow!("{k}: {e:?}"))?;
                    handled.push(window.dispatch_keystroke(ks, cx));
                    redraw(window, cx);
                }
                json!({ "handled": handled })
            }
            "type" => {
                for c in req["text"].as_str().unwrap_or("").chars() {
                    let key = match c {
                        ' ' => "space".to_string(),
                        '\n' => "enter".to_string(),
                        c if c.is_uppercase() => format!("shift-{}", c.to_lowercase()),
                        c => c.to_string(),
                    };
                    let mut ks = Keystroke::parse_with_os(&key, OperatingSystem::Mac)
                        .map_err(|e| anyhow::anyhow!("{key}: {e:?}"))?;
                    if c != '\n' {
                        ks.key_char = Some(c.to_string());
                    }
                    window.dispatch_keystroke(ks, cx);
                }
                json!({ "ok": true })
            }
            "move" => {
                window.dispatch_event(
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position: point(),
                        pressed_button: None,
                        modifiers: mods(),
                    }),
                    cx,
                );
                json!({ "ok": true })
            }
            "click" => {
                let button = match req["button"].as_str() {
                    Some("right") => MouseButton::Right,
                    Some("middle") => MouseButton::Middle,
                    _ => MouseButton::Left,
                };
                let count = req["count"].as_u64().unwrap_or(1).max(1) as usize;
                let position = point();
                let modifiers = mods();
                window.dispatch_event(
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position,
                        pressed_button: None,
                        modifiers,
                    }),
                    cx,
                );
                redraw(window, cx);
                for n in 1..=count {
                    window.dispatch_event(
                        PlatformInput::MouseDown(MouseDownEvent {
                            button,
                            position,
                            modifiers,
                            click_count: n,
                            first_mouse: false,
                        }),
                        cx,
                    );
                    window.dispatch_event(
                        PlatformInput::MouseUp(MouseUpEvent {
                            button,
                            position,
                            modifiers,
                            click_count: n,
                        }),
                        cx,
                    );
                }
                json!({ "ok": true })
            }
            "scroll" => {
                let dy = req["dy"].as_f64().unwrap_or(0.) as f32;
                let dx = req["dx"].as_f64().unwrap_or(0.) as f32;
                window.dispatch_event(
                    PlatformInput::Scroll(ScrollEvent {
                        position: point(),
                        delta: ScrollDelta::Pixels(gpui_kit::point(px(dx), px(dy))),
                        modifiers: mods(),
                        touch_phase: TouchPhase::Moved,
                        ..Default::default()
                    }),
                    cx,
                );
                json!({ "ok": true })
            }
            "shot" => {
                let path = req["path"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("shot needs a path"))?;
                redraw(window, cx);
                let image = window.render_to_image()?;
                let (w, h) = image.dimensions();
                std::fs::write(path, bmp(w, h, image.as_raw()))?;
                json!({ "path": path, "width": w, "height": h, "scale": window.scale_factor() })
            }
            other => anyhow::bail!("unknown cmd {other:?}"),
        };
        window.refresh();
        Ok(result)
    })?
}

/// A 32-bit BMP from RGBA rows (no image crate: adding one re-resolves gpui-kit's
/// dependencies, which conflicts).
fn bmp(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let data = (w * h * 4) as usize;
    let mut out = Vec::with_capacity(54 + data);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&((54 + data) as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(-(h as i32)).to_le_bytes()); // top-down rows
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0u8; 24]); // no compression; sizes and palette unused
    for px in rgba.chunks_exact(4) {
        out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
    }
    out
}

fn redraw(window: &mut Window, cx: &mut App) {
    let arena = window.draw(cx);
    arena.clear(cx);
}

fn modifiers(s: &str) -> Modifiers {
    let mut m = Modifiers::default();
    for part in s.split([',', '-', '+']) {
        match part.trim() {
            "cmd" | "platform" => m.platform = true,
            "shift" => m.shift = true,
            "alt" | "option" => m.alt = true,
            "ctrl" | "control" => m.control = true,
            _ => {}
        }
    }
    m
}
