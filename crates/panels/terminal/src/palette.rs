//! The 16 ANSI colours from pide's theme, the xterm 256-colour cube, and the
//! overrides a program sets with OSC 4/10/11.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use gpui_kit::component::{ActiveTheme as _, Theme};
use gpui_kit::*;

#[derive(Clone)]
pub struct Palette {
    ansi: [Hsla; 16],
    pub foreground: Hsla,
    pub background: Hsla,
    pub cursor: Hsla,
    pub selection: Hsla,
}

impl Palette {
    pub fn from_theme(cx: &App) -> Self {
        let t: &Theme = cx.theme();
        let gray = |l: f32| hsla(0., 0., l, 1.);
        let (black, bright_black, white, bright_white) = if t.is_dark() {
            (gray(0.25), gray(0.5), gray(0.8), gray(0.97))
        } else {
            (gray(0.0), gray(0.45), gray(0.72), gray(0.9))
        };
        Self {
            ansi: [
                black,
                t.red,
                t.green,
                t.yellow,
                t.blue,
                t.magenta,
                t.cyan,
                white,
                bright_black,
                t.red_light,
                t.green_light,
                t.yellow_light,
                t.blue_light,
                t.magenta_light,
                t.cyan_light,
                bright_white,
            ],
            foreground: t.foreground,
            background: t.background,
            cursor: t.caret,
            selection: t.selection,
        }
    }

    pub fn resolve(&self, color: Color, overrides: &Colors) -> Hsla {
        match color {
            Color::Spec(rgb) => from_rgb(rgb),
            Color::Indexed(i) => self.indexed(i as usize, overrides),
            Color::Named(named) => self.named(named, overrides),
        }
    }

    fn named(&self, named: NamedColor, overrides: &Colors) -> Hsla {
        if let Some(rgb) = overrides[named as usize] {
            return from_rgb(rgb);
        }
        let i = named as usize;
        match named {
            NamedColor::Foreground | NamedColor::BrightForeground => self.foreground,
            NamedColor::Background => self.background,
            NamedColor::Cursor => self.cursor,
            NamedColor::DimForeground => self.foreground.opacity(0.66),
            _ if i < 16 => self.ansi[i],
            // DimBlack..DimWhite follow Cursor in alacritty's numbering.
            _ => self.ansi[(i - NamedColor::DimBlack as usize) % 8].opacity(0.66),
        }
    }

    fn indexed(&self, i: usize, overrides: &Colors) -> Hsla {
        if let Some(rgb) = overrides[i] {
            return from_rgb(rgb);
        }
        match i {
            0..16 => self.ansi[i],
            16..232 => {
                let i = i - 16;
                let level = |v: usize| if v == 0 { 0 } else { (55 + v * 40) as u8 };
                from_rgb(Rgb {
                    r: level(i / 36),
                    g: level((i / 6) % 6),
                    b: level(i % 6),
                })
            }
            _ => {
                let v = (8 + (i.min(255) - 232) * 10) as u8;
                from_rgb(Rgb { r: v, g: v, b: v })
            }
        }
    }

    /// For a program asking for a colour (OSC 4/10/11 queries: vim and fish
    /// pick light or dark defaults from the background).
    pub fn rgb_of(&self, index: usize, overrides: &Colors) -> Rgb {
        let hsla = match index {
            0..256 => self.indexed(index, overrides),
            _ => match index {
                256 => self.foreground,
                257 => self.background,
                _ => self.cursor,
            },
        };
        let c = Rgba::from(hsla);
        let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
        Rgb {
            r: byte(c.r),
            g: byte(c.g),
            b: byte(c.b),
        }
    }
}

fn from_rgb(c: Rgb) -> Hsla {
    rgb(((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32).into()
}
