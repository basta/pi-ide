//! Keystrokes → the bytes a terminal sends (xterm conventions, as alacritty
//! and most terminals do). Printable text without Ctrl/Alt isn't handled here:
//! it arrives through the input handler, so IME, dead keys and AltGr work.

use std::borrow::Cow;

use alacritty_terminal::term::TermMode;
use gpui_kit::Keystroke;

/// The bytes for `keystroke`, or `None` to leave it to text input.
pub fn to_bytes(keystroke: &Keystroke, mode: TermMode) -> Option<Cow<'static, [u8]>> {
    let m = &keystroke.modifiers;
    if m.platform {
        return None;
    }
    let modifier_code = 1 + m.shift as u8 + 2 * m.alt as u8 + 4 * m.control as u8;
    if let Some(bytes) = named_key(&keystroke.key, modifier_code, mode) {
        return Some(bytes);
    }

    let key = keystroke.key.as_str();
    if m.control && !m.alt {
        return control_byte(key, m.shift).map(|b| Cow::Owned(vec![b]));
    }
    // AltGr arrives as Ctrl+Alt on Windows with a composed character: that's text.
    if m.alt && !m.control && alt_is_meta() {
        let ch = if key.chars().count() == 1 {
            if m.shift {
                key.to_uppercase()
            } else {
                key.to_string()
            }
        } else {
            return None;
        };
        return Some(Cow::Owned(format!("\x1b{ch}").into_bytes()));
    }
    None
}

/// On macOS Option composes characters (Terminal.app's default); elsewhere Alt is Meta.
fn alt_is_meta() -> bool {
    !cfg!(target_os = "macos")
}

fn named_key(key: &str, modifier_code: u8, mode: TermMode) -> Option<Cow<'static, [u8]>> {
    let plain = modifier_code == 1;
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let shift_only = modifier_code == 2;
    let ctrl = modifier_code >= 5;
    let alt = matches!(modifier_code, 3 | 4 | 7 | 8);
    let s: Cow<'static, str> = match key {
        "enter" => {
            if alt {
                "\x1b\r".into()
            } else {
                "\r".into()
            }
        }
        "tab" if shift_only => "\x1b[Z".into(),
        "tab" if plain => "\t".into(),
        "escape" if plain => "\x1b".into(),
        "backspace" if ctrl => "\x08".into(),
        "backspace" if alt => "\x1b\x7f".into(),
        "backspace" => "\x7f".into(),
        "space" if ctrl => "\0".into(),
        "up" | "down" | "right" | "left" | "home" | "end" => {
            let c = match key {
                "up" => 'A',
                "down" => 'B',
                "right" => 'C',
                "left" => 'D',
                "home" => 'H',
                _ => 'F',
            };
            if plain {
                if app_cursor {
                    format!("\x1bO{c}").into()
                } else {
                    format!("\x1b[{c}").into()
                }
            } else {
                format!("\x1b[1;{modifier_code}{c}").into()
            }
        }
        "f1" | "f2" | "f3" | "f4" => {
            let c = match key {
                "f1" => 'P',
                "f2" => 'Q',
                "f3" => 'R',
                _ => 'S',
            };
            if plain {
                format!("\x1bO{c}").into()
            } else {
                format!("\x1b[1;{modifier_code}{c}").into()
            }
        }
        _ => {
            let n = tilde_code(key)?;
            if plain {
                format!("\x1b[{n}~").into()
            } else {
                format!("\x1b[{n};{modifier_code}~").into()
            }
        }
    };
    Some(match s {
        Cow::Borrowed(s) => Cow::Borrowed(s.as_bytes()),
        Cow::Owned(s) => Cow::Owned(s.into_bytes()),
    })
}

fn tilde_code(key: &str) -> Option<u8> {
    Some(match key {
        "insert" => 2,
        "delete" => 3,
        "pageup" => 5,
        "pagedown" => 6,
        "f5" => 15,
        "f6" => 17,
        "f7" => 18,
        "f8" => 19,
        "f9" => 20,
        "f10" => 21,
        "f11" => 23,
        "f12" => 24,
        _ => return None,
    })
}

/// Ctrl+key as a C0 control byte (Ctrl+A = 0x01 … Ctrl+_ = 0x1f).
fn control_byte(key: &str, shift: bool) -> Option<u8> {
    let mut chars = key.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    Some(match c {
        'a'..='z' => c as u8 - b'a' + 1,
        '@' | '2' | ' ' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '-' if shift => 0x1f,
        '_' | '/' | '7' => 0x1f,
        '8' | '?' => 0x7f,
        _ => return None,
    })
}

/// Pasted text as the shell should get it: bracketed if the program asked for
/// it (so a multi-line paste doesn't run line by line), newlines as Enter.
pub fn paste_bytes(text: &str, mode: TermMode) -> Vec<u8> {
    if mode.contains(TermMode::BRACKETED_PASTE) {
        // A pasted ESC could end the bracket early and type the rest as commands.
        let clean = text.replace('\x1b', "");
        format!("\x1b[200~{clean}\x1b[201~").into_bytes()
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(keys: &'static str, mode: TermMode) -> Option<Vec<u8>> {
        to_bytes(&Keystroke::parse(keys).unwrap(), mode).map(|b| b.into_owned())
    }

    #[::core::prelude::v1::test]
    fn plain_and_special_keys() {
        let none = TermMode::empty();
        assert_eq!(bytes("enter", none).unwrap(), b"\r");
        assert_eq!(bytes("backspace", none).unwrap(), b"\x7f");
        assert_eq!(bytes("shift-tab", none).unwrap(), b"\x1b[Z");
        assert_eq!(bytes("escape", none).unwrap(), b"\x1b");
        assert_eq!(bytes("delete", none).unwrap(), b"\x1b[3~");
        assert_eq!(bytes("f5", none).unwrap(), b"\x1b[15~");
        assert_eq!(bytes("f1", none).unwrap(), b"\x1bOP");
        assert_eq!(bytes("a", none), None);
        assert_eq!(bytes("shift-a", none), None);
    }

    #[::core::prelude::v1::test]
    fn cursor_keys_follow_app_cursor_mode() {
        assert_eq!(bytes("up", TermMode::empty()).unwrap(), b"\x1b[A");
        assert_eq!(bytes("up", TermMode::APP_CURSOR).unwrap(), b"\x1bOA");
        assert_eq!(bytes("ctrl-left", TermMode::empty()).unwrap(), b"\x1b[1;5D");
        assert_eq!(
            bytes("shift-end", TermMode::APP_CURSOR).unwrap(),
            b"\x1b[1;2F"
        );
        assert_eq!(
            bytes("ctrl-pagedown", TermMode::empty()).unwrap(),
            b"\x1b[6;5~"
        );
    }

    #[::core::prelude::v1::test]
    fn control_bytes() {
        let none = TermMode::empty();
        assert_eq!(bytes("ctrl-c", none).unwrap(), [3]);
        assert_eq!(bytes("ctrl-a", none).unwrap(), [1]);
        assert_eq!(bytes("ctrl-z", none).unwrap(), [26]);
        assert_eq!(bytes("ctrl-[", none).unwrap(), [0x1b]);
        assert_eq!(bytes("ctrl-space", none).unwrap(), [0]);
        assert_eq!(bytes("ctrl-backspace", none).unwrap(), [8]);
        assert_eq!(bytes("cmd-c", none), None);
    }

    #[::core::prelude::v1::test]
    fn alt_is_escape_prefix_off_macos() {
        let b = bytes("alt-b", TermMode::empty());
        if cfg!(target_os = "macos") {
            assert_eq!(b, None);
        } else {
            assert_eq!(b.unwrap(), b"\x1bb");
        }
        assert_eq!(bytes("ctrl-alt-q", TermMode::empty()), None);
    }

    #[::core::prelude::v1::test]
    fn paste() {
        assert_eq!(paste_bytes("a\nb", TermMode::empty()), b"a\rb");
        assert_eq!(
            paste_bytes("x\x1by", TermMode::BRACKETED_PASTE),
            b"\x1b[200~xy\x1b[201~"
        );
    }
}
