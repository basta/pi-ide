//! Runs of tool calls: the calls between two bits of pi's prose collapse to one
//! summary line ("Read lib.rs, edited diff_panel.rs, ran cargo check"). Click
//! it for one row per call, click a row for its output. A run with a failure
//! opens by itself when it ends in a failure, with the failed call's output
//! showing (a failure pi recovered from is only a small red count).

use super::*;

/// What the conversation shows, in order: single items and runs of tool calls.
pub(crate) enum Segment {
    Item(usize),
    /// Item indices: tool calls, and the empty assistant messages between them.
    Run(Vec<usize>),
}

/// An assistant message with no text (only tool calls, maybe thinking): it
/// belongs to the run around it.
fn is_silent(item: &Item) -> bool {
    matches!(item, Item::Assistant { text, error: None, .. } if text.trim().is_empty())
}

fn in_run(item: &Item) -> bool {
    matches!(item, Item::Tool { .. }) || is_silent(item)
}

pub(crate) fn segments(items: &[Item]) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < items.len() {
        if !in_run(&items[i]) {
            out.push(Segment::Item(i));
            i += 1;
            continue;
        }
        let mut j = i;
        while j < items.len() && in_run(&items[j]) {
            j += 1;
        }
        if items[i..j].iter().any(|it| matches!(it, Item::Tool { .. })) {
            out.push(Segment::Run((i..j).collect()));
        } else {
            out.extend((i..j).map(Segment::Item));
        }
        i = j;
    }
    out
}

/// One tool call, as the summary sees it.
pub(crate) struct Call<'a> {
    pub name: &'a str,
    pub summary: &'a str,
}

fn kind(name: &str) -> &str {
    match name {
        "edit" | "write" => "edit",
        "grep" | "find" => "search",
        "ls" => "list",
        n => n,
    }
}

fn file_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .filter(|f| !f.is_empty())
        .unwrap_or_else(|| path.to_string())
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        format!("{}…", s.chars().take(n).collect::<String>())
    } else {
        s.to_string()
    }
}

/// The gist of a shell command: `cd x && cargo check -p y 2>&1 | tail` → `cargo check`.
pub(crate) fn command_label(cmd: &str) -> String {
    // Tools whose first argument says what they do.
    const SUB: &[&str] = &[
        "cargo", "git", "npm", "pnpm", "yarn", "npx", "go", "uv", "pip", "docker", "gh", "pidectl",
        "kubectl", "bun", "deno", "swift", "xcrun",
    ];
    const SETUP: &[&str] = &[
        "cd", "export", "set", "source", "pushd", "popd", "mkdir", "rm",
    ];
    let cmd = cmd.replace(" ⏎ ", ";");
    let part = cmd
        .split(['&', ';', '|'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .find(|p| {
            p.split_whitespace()
                .next()
                .is_some_and(|w| !SETUP.contains(&w))
        });
    let Some(part) = part else {
        return clip(cmd.trim(), 30);
    };
    let mut words = part
        .split_whitespace()
        // Leading `FOO=bar` environment assignments.
        .skip_while(|w| {
            w.split_once('=')
                .is_some_and(|(k, _)| !k.is_empty() && !k.contains('/'))
        });
    let Some(tool) = words.next() else {
        return clip(part, 30);
    };
    let tool = tool.rsplit('/').next().unwrap_or(tool);
    match words.next() {
        Some(sub) if SUB.contains(&tool) && !sub.starts_with('-') => format!("{tool} {sub}"),
        _ => tool.to_string(),
    }
}

/// Distinct labels in order of first appearance.
fn distinct(labels: impl Iterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for l in labels {
        if !out.contains(&l) {
            out.push(l);
        }
    }
    out
}

/// "Read lib.rs and diff_panel.rs, edited diff_panel.rs, ran cargo check".
pub(crate) fn summarize(calls: &[Call]) -> String {
    let mut kinds: Vec<&str> = Vec::new();
    for c in calls {
        let k = kind(c.name);
        if !kinds.contains(&k) {
            kinds.push(k);
        }
    }
    let phrases: Vec<String> = kinds
        .iter()
        .map(|&k| {
            let of_kind: Vec<&Call> = calls.iter().filter(|c| kind(c.name) == k).collect();
            let n = of_kind.len();
            let names = |f: fn(&str) -> String| distinct(of_kind.iter().map(|c| f(c.summary)));
            let list = |verb: &str, labels: Vec<String>, many: String| {
                if labels.len() <= 2 {
                    format!("{verb} {}", labels.join(" and "))
                } else {
                    format!("{verb} {many}")
                }
            };
            match k {
                "read" => {
                    let l = names(file_name);
                    let m = format!("{} files", l.len());
                    list("read", l, m)
                }
                "edit" => {
                    let l = names(file_name);
                    let m = format!("{} files", l.len());
                    list("edited", l, m)
                }
                "list" => {
                    let l = names(file_name);
                    let m = format!("{} folders", l.len());
                    list("listed", l, m)
                }
                "bash" => list("ran", names(command_label), format!("{n} commands")),
                "search" => list("searched", names(|s| clip(s, 24)), format!("{n} times")),
                other if n == 1 => format!("used {other}"),
                other => format!("used {other} ×{n}"),
            }
        })
        .collect();
    let s = phrases.join(", ");
    let mut chars = s.chars();
    match chars.next() {
        Some(f) => f.to_uppercase().chain(chars).collect(),
        None => s,
    }
}

/// The call that's running: "Running cargo test".
pub(crate) fn running_phrase(c: &Call) -> String {
    match kind(c.name) {
        "read" => format!("Reading {}", file_name(c.summary)),
        "edit" => format!("Editing {}", file_name(c.summary)),
        "list" => format!("Listing {}", file_name(c.summary)),
        "search" => format!("Searching {}", clip(c.summary, 24)),
        "bash" => format!("Running {}", command_label(c.summary)),
        other => format!("Running {other}"),
    }
}

pub(crate) fn fmt_ms(ms: u64) -> String {
    match ms {
        0..1000 => format!("{ms} ms"),
        1000..60_000 => format!("{:.1} s", ms as f64 / 1000.0),
        _ => format!("{} min {} s", ms / 60_000, ms % 60_000 / 1000),
    }
}

impl AgentChat {
    /// A run of tool calls: its summary line, and its rows when open.
    pub(crate) fn render_run(
        &self,
        view: &SessionView,
        ixs: &[usize],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let agent = self.active(cx);
        let agent_id = agent.entity_id();
        struct Tool<'a> {
            ix: usize,
            id: &'a str,
            call: Call<'a>,
            state: ToolState,
            ms: Option<u64>,
        }
        let tools: Vec<Tool> = ixs
            .iter()
            .filter_map(|&ix| match &view.items[ix] {
                Item::Tool {
                    id,
                    name,
                    summary,
                    state,
                    duration_ms,
                    ..
                } => Some(Tool {
                    ix,
                    id,
                    call: Call { name, summary },
                    state: *state,
                    ms: *duration_ms,
                }),
                _ => None,
            })
            .collect();
        let Some(first) = tools.first() else {
            return div().into_any_element();
        };
        let (first_ix, key) = (first.ix, first.id.to_string());
        let failed = tools
            .iter()
            .filter(|t| t.state == ToolState::Failed)
            .count();
        let running = tools.iter().find(|t| t.state == ToolState::Running);
        // A call that failed and was followed by ones that worked is a detail
        // (a small red count); only a run that ends in a failure reads as failed
        // and opens itself.
        let ended_failed = tools.last().is_some_and(|t| t.state == ToolState::Failed);
        let open = view.run_open.get(&key).copied().unwrap_or(ended_failed);

        let calls: Vec<Call> = tools
            .iter()
            .map(|t| Call {
                name: t.call.name,
                summary: t.call.summary,
            })
            .collect();
        let line = match running {
            Some(t) => format!("{}…", running_phrase(&t.call)),
            None => summarize(&calls),
        };
        let total: u64 = tools.iter().filter_map(|t| t.ms).sum();
        let mut meta = Vec::new();
        if tools.len() > 1 {
            meta.push(format!("{} calls", tools.len()));
        }
        if total > 0 {
            meta.push(fmt_ms(total));
        }
        let color = if ended_failed {
            theme.danger
        } else {
            theme.muted_foreground
        };
        // While a call runs, its last lines show under the line (closed runs only).
        let live_tail = running.and_then(|t| match &view.items[t.ix] {
            Item::Tool { output, .. } if !open && !output.trim().is_empty() => {
                let lines: Vec<&str> = output.trim_end().lines().collect();
                Some(lines[lines.len().saturating_sub(2)..].join("\n"))
            }
            _ => None,
        });

        let toggle_key = key.clone();
        let header = h_flex()
            .id(("run", first_ix))
            .gap_2()
            .items_start()
            .py_0p5()
            .px_1()
            .rounded(theme.radius)
            .text_xs()
            .text_color(color)
            .cursor_pointer()
            .hover(|s| s.bg(theme.list_hover))
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(v) = this.views.get_mut(&agent_id) {
                    let o = v.run_open.entry(toggle_key.clone()).or_insert(ended_failed);
                    *o = !*o;
                    cx.notify();
                }
            }))
            .child(div().flex_none().child(if open { "▾" } else { "▸" }))
            .when(ended_failed, |this| {
                this.child(div().flex_none().child("✗"))
            })
            .child(
                // Wraps rather than clipping: the summary is the point of the line.
                div().flex_1().min_w_0().child(line),
            )
            .when(failed > 0, |this| {
                this.child(
                    div()
                        .flex_none()
                        .text_color(theme.danger)
                        .child(format!("{failed} failed")),
                )
            })
            .when(!meta.is_empty(), |this| {
                this.child(
                    div()
                        .flex_none()
                        .text_color(theme.muted_foreground)
                        .child(meta.join(" · ")),
                )
            });

        let mut el = v_flex().gap_0p5().child(header);
        if let Some(tail) = live_tail {
            el = el.child(
                div()
                    .pl_5()
                    .text_xs()
                    .font_family(theme.mono_font_family.clone())
                    .text_color(theme.muted_foreground)
                    .child(tail),
            );
        }
        if open {
            let rows: Vec<AnyElement> = ixs
                .iter()
                .filter_map(|&ix| self.render_run_row(view, ix, cx))
                .collect();
            el = el.child(v_flex().pl_4().gap_0p5().children(rows));
        }
        el.into_any_element()
    }

    /// One call in an open run (or the thinking of the message that made it).
    fn render_run_row(
        &self,
        view: &SessionView,
        ix: usize,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        let agent = self.active(cx);
        let agent_id = agent.entity_id();
        let mono = theme.mono_font_family.clone();
        match &view.items[ix] {
            Item::Assistant {
                thinking,
                thinking_open,
                ..
            } if !thinking.trim().is_empty() => {
                let open = *thinking_open;
                Some(
                    v_flex()
                        .child(
                            div()
                                .id(("run-thinking", ix))
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .cursor_pointer()
                                .hover(|s| s.bg(theme.list_hover))
                                .px_1()
                                .rounded(theme.radius)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(Item::Assistant { thinking_open, .. }) = this
                                        .views
                                        .get_mut(&agent_id)
                                        .and_then(|v| v.items.get_mut(ix))
                                    {
                                        *thinking_open = !*thinking_open;
                                        cx.notify();
                                    }
                                }))
                                .child(format!(
                                    "{} thought ({} words)",
                                    if open { "▾" } else { "▸" },
                                    thinking.split_whitespace().count()
                                )),
                        )
                        .when(open, |this| {
                            this.child(
                                div()
                                    .pl_4()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(thinking.trim().to_string()),
                            )
                        })
                        .into_any_element(),
                )
            }
            Item::Tool {
                id,
                name,
                summary,
                state,
                output,
                duration_ms,
            } => {
                let (mark, mark_color) = match state {
                    ToolState::Running => ("…", theme.muted_foreground),
                    ToolState::Done => ("✓", theme.success),
                    ToolState::Failed => ("✗", theme.danger),
                };
                let open = view
                    .tool_open
                    .get(id)
                    .copied()
                    .unwrap_or(*state == ToolState::Failed);
                let root = self.workspace.read(cx).root().to_path_buf();
                // A file tool's path opens the file.
                let file = matches!(name.as_str(), "read" | "write" | "edit").then(|| {
                    let p = std::path::Path::new(summary.as_str());
                    if p.is_absolute() {
                        p.to_path_buf()
                    } else {
                        root.join(p)
                    }
                });
                let shown = file
                    .as_ref()
                    .and_then(|f| f.strip_prefix(&root).ok())
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| summary.clone());
                // An edit links to what it changed (the file's change this session).
                let change = matches!(name.as_str(), "write" | "edit")
                    .then(|| {
                        agent
                            .read(cx)
                            .changes
                            .iter()
                            .find(|c| {
                                !c.reverted && c.touches.iter().any(|t| &t.tool_call_id == id)
                            })
                            .map(|c| (c.path.clone(), c.before.clone(), c.stats))
                    })
                    .flatten();
                let toggle_id = id.clone();
                let default_open = *state == ToolState::Failed;
                Some(
                    v_flex()
                        .child(
                            h_flex()
                                .id(("run-row", ix))
                                .gap_2()
                                .px_1()
                                .rounded(theme.radius)
                                .text_xs()
                                .font_family(mono.clone())
                                .when(!output.is_empty(), |this| {
                                    this.cursor_pointer().hover(|s| s.bg(theme.list_hover))
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(v) = this.views.get_mut(&agent_id) {
                                        let o = v
                                            .tool_open
                                            .entry(toggle_id.clone())
                                            .or_insert(default_open);
                                        *o = !*o;
                                        cx.notify();
                                    }
                                }))
                                .child(div().flex_none().text_color(mark_color).child(mark))
                                .child(
                                    div()
                                        .flex_none()
                                        .min_w(px(36.))
                                        .whitespace_nowrap()
                                        .text_color(theme.muted_foreground)
                                        .child(name.clone()),
                                )
                                .child(
                                    div()
                                        .id(("run-path", ix))
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .when(*state == ToolState::Failed, |d| {
                                            d.text_color(theme.danger)
                                        })
                                        .when_some(file, |d, file| {
                                            d.cursor_pointer().hover(|s| s.underline()).on_click(
                                                move |_, window, cx| {
                                                    cx.stop_propagation();
                                                    if file.is_file() {
                                                        pide_core::opener::open_file(
                                                            file.clone(),
                                                            window,
                                                            cx,
                                                        );
                                                    }
                                                },
                                            )
                                        })
                                        .child(shown.clone()),
                                )
                                .when_some(change, |this, (path, before, stats)| {
                                    let label: SharedString = shown.clone().into();
                                    this.when_some(stats, |this, (p, m)| {
                                        this.child(
                                            h_flex()
                                                .flex_none()
                                                .gap_1()
                                                .child(
                                                    div()
                                                        .text_color(theme.success)
                                                        .child(format!("+{p}")),
                                                )
                                                .child(
                                                    div()
                                                        .text_color(theme.danger)
                                                        .child(format!("−{m}")),
                                                ),
                                        )
                                    })
                                    .child(
                                        div()
                                            .id(("run-change", ix))
                                            .flex_none()
                                            .font_family(theme.font_family.clone())
                                            .text_color(pide_core::colors::accent(cx))
                                            .cursor_pointer()
                                            .hover(|s| s.underline())
                                            .on_click(move |_, window, cx| {
                                                cx.stop_propagation();
                                                pide_core::opener::open_diff(
                                                    pide_core::opener::DiffRequest {
                                                        path: path.clone(),
                                                        before: before.clone(),
                                                        after: None,
                                                        label: label.clone(),
                                                        before_is: "when pi first touched it"
                                                            .into(),
                                                    },
                                                    window,
                                                    cx,
                                                );
                                            })
                                            .child("View change"),
                                    )
                                })
                                .when_some(*duration_ms, |this, d| {
                                    this.child(
                                        div()
                                            .flex_none()
                                            .text_color(theme.muted_foreground)
                                            .child(fmt_ms(d)),
                                    )
                                }),
                        )
                        .when(open && !output.is_empty(), |this| {
                            this.child(
                                div()
                                    .ml(px(52.))
                                    .my_0p5()
                                    .px_2()
                                    .py_1()
                                    .rounded(theme.radius)
                                    .border_1()
                                    .border_color(theme.border)
                                    .text_xs()
                                    .font_family(mono)
                                    .text_color(theme.muted_foreground)
                                    .child(output.clone()),
                            )
                        })
                        .into_any_element(),
                )
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call<'a>(name: &'a str, summary: &'a str) -> Call<'a> {
        Call { name, summary }
    }

    #[::core::prelude::v1::test]
    fn commands_reduce_to_their_gist() {
        assert_eq!(
            command_label("cargo check -p pide-editor 2>&1 | tail -3"),
            "cargo check"
        );
        assert_eq!(
            command_label("cd /tmp && rm -rf dwrap && mkdir dwrap ⏎ dev/pidectl launch x"),
            "pidectl launch"
        );
        assert_eq!(
            command_label("cd ~/p && RUST_LOG=debug cargo test --locked"),
            "cargo test"
        );
        assert_eq!(command_label("ls -la"), "ls");
        assert_eq!(command_label("git -C x status"), "git");
        assert_eq!(command_label("/usr/bin/python3 script.py"), "python3");
    }

    #[::core::prelude::v1::test]
    fn runs_read_as_one_sentence() {
        assert_eq!(
            summarize(&[
                call("read", "crates/editor/src/lib.rs"),
                call("read", "crates/editor/src/diff_panel.rs"),
                call("grep", "soft_wrap"),
            ]),
            "Read lib.rs and diff_panel.rs, searched soft_wrap"
        );
        assert_eq!(
            summarize(&[
                call("edit", "crates/editor/src/diff_panel.rs"),
                call("bash", "cargo check -p pide-editor"),
                call("edit", "crates/editor/src/diff_panel.rs"),
                call("bash", "cargo check -p pide-editor"),
            ]),
            "Edited diff_panel.rs, ran cargo check"
        );
        assert_eq!(
            summarize(&[
                call("read", "a.rs"),
                call("read", "b.rs"),
                call("read", "c.rs")
            ]),
            "Read 3 files"
        );
        assert_eq!(
            summarize(&[call("ide_diagnostics", "{}"), call("ide_diagnostics", "{}")]),
            "Used ide_diagnostics ×2"
        );
    }

    #[::core::prelude::v1::test]
    fn durations() {
        assert_eq!(fmt_ms(9), "9 ms");
        assert_eq!(fmt_ms(2733), "2.7 s");
        assert_eq!(fmt_ms(65_000), "1 min 5 s");
    }
}
