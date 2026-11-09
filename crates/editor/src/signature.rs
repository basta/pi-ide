//! Signature help (spec M3). GPUI Kit has no provider for it: request it on the
//! server's trigger characters (`(` and `,` usually) and on every edit while it
//! shows; draw it above the caret with the active parameter highlighted.

use gpui_kit::component::ActiveTheme;
use gpui_kit::component::input::RopeExt as _;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::*;
use lsp_types::{ParameterLabel, SignatureHelp};
use pide_lsp::convert::offset_to_server;
use pide_lsp::process::file_uri;
use serde_json::json;

use crate::EditorPanel;

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Trigger {
    /// Cmd+P.
    Invoked,
    /// A trigger character was typed.
    Character(char),
    /// An edit while it is showing.
    Edit,
}

impl EditorPanel {
    /// After an edit: ask for signature help if a trigger character was typed,
    /// or refresh it if it's showing (the server answers null when the caret
    /// has left the call, which closes it).
    pub(crate) fn signature_after_edit(&mut self, cx: &mut Context<Self>) {
        // The character just before the caret.
        let prev = {
            let e = self.editor.read(cx);
            let pos = e.text().offset_to_position(e.cursor());
            pos.character
                .checked_sub(1)
                .and_then(|i| e.text().slice_line(pos.line as usize).to_string().chars().nth(i as usize))
        };
        let triggers = self.signature_triggers(cx);
        match prev {
            Some(c) if triggers.contains(&c) => self.request_signature(Trigger::Character(c), cx),
            _ if self.signature.is_some() => self.request_signature(Trigger::Edit, cx),
            _ => {}
        }
    }

    fn signature_triggers(&self, cx: &App) -> Vec<char> {
        let Some(store) = self.lsp.as_ref() else { return vec![] };
        let Some(opts) = store
            .read(cx)
            .server_for(&self.path)
            .and_then(|s| s.capabilities.as_ref())
            .and_then(|c| c.signature_help_provider.clone())
        else {
            return vec![];
        };
        opts.trigger_characters
            .into_iter()
            .chain(opts.retrigger_characters)
            .flatten()
            .filter_map(|s| s.chars().next())
            .collect()
    }

    pub(crate) fn request_signature(&mut self, trigger: Trigger, cx: &mut Context<Self>) {
        let Some(store) = self.lsp.clone() else { return };
        let Some(enc) = store.read(cx).server_for(&self.path).map(|s| s.encoding) else { return };
        let (rope, cursor) = {
            let e = self.editor.read(cx);
            (e.text().clone(), e.cursor())
        };
        let line = rope.offset_to_position(cursor).line;
        let context = match trigger {
            Trigger::Invoked => json!({ "triggerKind": 1, "isRetrigger": self.signature.is_some() }),
            Trigger::Character(c) => {
                json!({ "triggerKind": 2, "triggerCharacter": c.to_string(), "isRetrigger": self.signature.is_some() })
            }
            Trigger::Edit => json!({ "triggerKind": 3, "isRetrigger": true }),
        };
        let params = json!({
            "textDocument": { "uri": file_uri(&self.path) },
            "position": offset_to_server(&rope, cursor, enc),
            "context": context,
        });
        let path = self.path.clone();
        let task = store.update(cx, |s, cx| s.request(&path, Some(&rope), "textDocument/signatureHelp", params, cx));
        self._signature_task = Some(cx.spawn(async move |this, cx| {
            let help = match task.await {
                Ok(v) if !v.is_null() => serde_json::from_value::<SignatureHelp>(v).ok().filter(|h| !h.signatures.is_empty()),
                _ => None,
            };
            let _ = this.update(cx, |this, cx| {
                this.signature = help;
                this.signature_line = line;
                cx.notify();
            });
        }));
    }

    pub(crate) fn hide_signature(&mut self, cx: &mut Context<Self>) {
        if self.signature.take().is_some() {
            cx.notify();
        }
    }

    /// The popup above the caret, if signature help is showing.
    pub(crate) fn render_signature(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let help = self.signature.as_ref()?;
        // The caret moved to another line (clicks and arrows don't send an edit).
        let e = self.editor.read(cx);
        if e.text().offset_to_position(e.cursor()).line != self.signature_line {
            return None;
        }
        let active = help.active_signature.unwrap_or(0) as usize;
        let sig = help.signatures.get(active).or_else(|| help.signatures.first())?;
        let param_ix = sig.active_parameter.or(help.active_parameter).unwrap_or(0) as usize;
        let range = sig.parameters.as_ref().and_then(|ps| ps.get(param_ix)).and_then(|p| param_range(&sig.label, &p.label));
        let (before, param, after) = match range {
            Some(r) => (&sig.label[..r.start], &sig.label[r.clone()], &sig.label[r.end..]),
            None => (sig.label.as_str(), "", ""),
        };
        let doc = sig
            .parameters
            .as_ref()
            .and_then(|ps| ps.get(param_ix))
            .and_then(|p| p.documentation.as_ref())
            .map(|d| match d {
                lsp_types::Documentation::String(s) => s.clone(),
                lsp_types::Documentation::MarkupContent(m) => m.value.clone(),
            })
            .and_then(|d| d.lines().find(|l| !l.trim().is_empty()).map(str::to_string));
        let count = help.signatures.len();

        // The caret's rendered bounds (window coordinates); none when it's scrolled away.
        let c = e.cursor();
        let caret = e.range_to_bounds(&(c..c))?;
        let anchor = point(caret.origin.x - px(4.), caret.origin.y - px(4.));
        let theme = cx.theme().clone();
        Some(
            deferred(
                anchored().position(anchor).anchor(Anchor::BottomLeft).snap_to_window_with_margin(px(8.)).child(
                    v_flex()
                        .max_w(px(640.))
                        .px_2()
                        .py_1()
                        .gap_0p5()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.popover)
                        .text_color(theme.popover_foreground)
                        .shadow_md()
                        .font_family(theme.mono_font_family.clone())
                        .text_size(theme.mono_font_size)
                        .child(
                            h_flex()
                                .flex_wrap()
                                .child(before.to_string())
                                .child(div().font_weight(FontWeight::BOLD).text_color(theme.primary).child(param.to_string()))
                                .child(after.to_string())
                                .when(count > 1, |this| {
                                    this.child(
                                        div()
                                            .pl_2()
                                            .text_color(theme.muted_foreground)
                                            .child(format!("{}/{count}", active + 1)),
                                    )
                                }),
                        )
                        .when_some(doc, |this, d| {
                            this.child(div().font_family(theme.font_family.clone()).text_sm().text_color(theme.muted_foreground).child(d))
                        }),
                ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }
}

use gpui_kit::prelude::FluentBuilder as _;

/// Byte range of a parameter inside the signature label. Offsets are UTF-16
/// code units (we advertise `labelOffsetSupport`); a plain string is searched for.
fn param_range(label: &str, param: &ParameterLabel) -> Option<std::ops::Range<usize>> {
    match param {
        ParameterLabel::Simple(s) if !s.is_empty() => label.find(s.as_str()).map(|i| i..i + s.len()),
        ParameterLabel::Simple(_) => None,
        ParameterLabel::LabelOffsets([a, b]) => {
            let byte = |units: u32| {
                let mut seen = 0u32;
                for (i, c) in label.char_indices() {
                    if seen >= units {
                        return Some(i);
                    }
                    seen += c.len_utf16() as u32;
                }
                (seen >= units).then_some(label.len())
            };
            Some(byte(*a)?..byte(*b)?)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn parameter_ranges_from_offsets_and_strings() {
        let label = "fn greet(name: &str, ünits: u8)";
        let r = param_range(label, &ParameterLabel::Simple("name: &str".into())).unwrap();
        assert_eq!(&label[r], "name: &str");
        // UTF-16 offsets: "ünits: u8" starts at 21 (ü is one unit).
        let r = param_range(label, &ParameterLabel::LabelOffsets([21, 30])).unwrap();
        assert_eq!(&label[r], "ünits: u8");
    }
}
