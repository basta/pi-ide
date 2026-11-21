//! Math in markdown: `$…$` inline and `$$…$$` display formulas, typeset with
//! RaTeX (a pure-Rust KaTeX port) and drawn as images.
//!
//! GPUI Kit's markdown parser always parses math but draws nothing for it:
//! an unclaimed inline formula stays literal text and a `$$` block becomes a
//! code block. [`markdown_extensions`] claims both. Every `TextView` that
//! shows markdown passes it (`TextView::markdown_extensions`).
//!
//! - Parsing and layout run with the markdown parse (often a background task);
//!   a formula RaTeX can't parse isn't claimed, so it falls back to the text.
//! - `$5 and $10` isn't a formula: like pandoc, an inline formula must not
//!   start or end with whitespace and the closing `$` must not be followed by
//!   a digit.
//! - Rasterising happens at render time (it needs the size, color and scale
//!   factor) and is cached per formula.
//! - Models often write `\(…\)` and `\[…\]`, which markdown reads as escaped
//!   brackets: [`normalize_delimiters`] rewrites them to dollars before the
//!   text reaches a `TextViewState`.
//! - Copying a formula copies its LaTeX source.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::text::{
    InlineElement, InlineRenderContext, MarkdownExtensions, MarkdownNode, MarkdownParseContext,
    MarkdownPlugin, markdown_ast as mdast,
};
use gpui_kit::{
    App, Hsla, Image, ImageFormat, IntoElement, ParentElement as _, Styled as _, Window, div, img,
    px,
};
use ratex_layout::{LayoutOptions, layout, to_display_list, validate_layout};
use ratex_render::{RenderOptions, render_to_png};
use ratex_types::{Color, DisplayItem, DisplayList, MathStyle};

/// Formula size relative to the surrounding text (KaTeX uses 1.21).
const INLINE_SCALE: f32 = 1.1;
const DISPLAY_SCALE: f32 = 1.2;
/// Logical pixels around the raster, so italic overhangs aren't clipped.
const PAD: f32 = 1.0;
/// Rasters kept; past this the cache starts over.
const CACHE_LIMIT: usize = 512;

const INLINE: &str = "pide-math-inline";
const DISPLAY: &str = "pide-math-display";

/// Markdown extensions that render math. Cheap to call (built once).
pub fn markdown_extensions() -> MarkdownExtensions {
    static EXT: LazyLock<MarkdownExtensions> = LazyLock::new(|| {
        MarkdownExtensions::default()
            // Fixed, so a TextView doesn't re-parse when this is passed every frame.
            .parser_revision(0x6d61_7468)
            .plugin(InlineMath)
            .plugin(DisplayMath)
    });
    EXT.clone()
}

/// Loads RaTeX's fonts off the UI thread, so the first formula doesn't stall a frame.
pub fn warm_up() {
    std::thread::Builder::new()
        .name("math-warm-up".into())
        .spawn(|| {
            if let Some(f) = typeset(r"x^2+\sqrt{y}=\frac{\alpha}{\beta}\sum\int", true) {
                let _ = rasterize(&f, 14.0, 2.0, Color::BLACK);
            }
        })
        .ok();
}

/// A parsed, laid-out formula (node data).
struct Formula {
    latex: String,
    display: bool,
    list: DisplayList,
}

fn typeset(latex: &str, display: bool) -> Option<Formula> {
    let latex = latex.trim();
    if latex.is_empty() {
        return None;
    }
    let nodes = ratex_parser::parse(latex).ok()?;
    let options = LayoutOptions {
        style: if display {
            MathStyle::Display
        } else {
            MathStyle::Text
        },
        ..Default::default()
    };
    let root = layout(&nodes, &options);
    validate_layout(&root).ok()?;
    let list = to_display_list(&root);
    (list.width > 0.0).then(|| Formula {
        latex: latex.to_string(),
        display,
        list,
    })
}

/// Whether `$…$` at `range` in `source` reads as a formula rather than two
/// dollar amounts (pandoc's rule).
fn looks_like_math(value: &str, source: &str, end: usize) -> bool {
    let starts_blank = value.chars().next().is_none_or(char::is_whitespace);
    let ends_blank = value.chars().next_back().is_none_or(char::is_whitespace);
    let digit_after = source[end.min(source.len())..]
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit());
    !starts_blank && !ends_blank && !digit_after
}

struct InlineMath;

impl MarkdownPlugin for InlineMath {
    fn name(&self) -> &str {
        INLINE
    }

    fn parse(&self, node: &mdast::Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let mdast::Node::InlineMath(math) = node else {
            return None;
        };
        let end = node.position()?.end.offset;
        if !looks_like_math(&math.value, cx.source(), end) {
            return None;
        }
        let source = cx.node_source(node).unwrap_or(&math.value);
        // `$$x$$` inside a line: a display formula that stays in the text.
        let formula = typeset(&math.value, source.starts_with("$$"))?;
        Some(
            MarkdownNode::new(INLINE, formula)
                .text(source.to_string())
                .markdown(source.to_string()),
        )
    }

    fn render_inline(
        &self,
        node: &MarkdownNode,
        context: &InlineRenderContext,
        window: &mut Window,
        _cx: &mut App,
    ) -> Option<InlineElement> {
        let formula = node.data::<Formula>()?;
        let size = f32::from(context.font_size()) * INLINE_SCALE;
        let r = cached(formula, size, window.scale_factor(), context.text_style().color)?;
        Some(
            InlineElement::new(img(r.image.clone()).w(px(r.width)).h(px(r.height)))
                .with_baseline(px(r.baseline)),
        )
    }
}

struct DisplayMath;

impl MarkdownPlugin for DisplayMath {
    fn is_block(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        DISPLAY
    }

    fn parse(&self, node: &mdast::Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let mdast::Node::Math(math) = node else {
            return None;
        };
        // Not claimed when RaTeX can't parse it: it stays a code block with the source.
        let formula = typeset(&math.value, true)?;
        let source = cx.node_source(node).unwrap_or(&math.value).to_string();
        Some(
            MarkdownNode::new(DISPLAY, formula)
                .text(source.clone())
                .markdown(source),
        )
    }

    fn render(&self, node: &MarkdownNode, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let row = div().w_full().flex().justify_center().overflow_hidden().py_1();
        let Some(formula) = node.data::<Formula>() else {
            return row;
        };
        let style = window.text_style();
        let size = f32::from(style.font_size.to_pixels(window.rem_size())) * DISPLAY_SCALE;
        let color = cx.theme().foreground;
        match cached(formula, size, window.scale_factor(), color) {
            Some(r) => row.child(
                img(r.image.clone())
                    .flex_shrink_0()
                    .w(px(r.width))
                    .h(px(r.height)),
            ),
            None => row
                .text_color(cx.theme().muted_foreground)
                .font_family(cx.theme().mono_font_family.clone())
                .child(formula.latex.clone()),
        }
    }
}

/// A formula raster: logical size and baseline (from the top).
struct Rendered {
    image: Arc<Image>,
    width: f32,
    height: f32,
    baseline: f32,
}

#[derive(Hash, PartialEq, Eq)]
struct Key {
    latex: String,
    display: bool,
    size: u32,
    scale: u32,
    color: [u8; 4],
}

fn cached(formula: &Formula, size: f32, scale: f32, color: Hsla) -> Option<Arc<Rendered>> {
    static CACHE: LazyLock<Mutex<HashMap<Key, Option<Arc<Rendered>>>>> =
        LazyLock::new(Default::default);
    let rgba = color.to_rgb();
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let key = Key {
        latex: formula.latex.clone(),
        display: formula.display,
        size: (size * 100.0).round() as u32,
        scale: (scale * 100.0).round() as u32,
        color: [byte(rgba.r), byte(rgba.g), byte(rgba.b), byte(rgba.a)],
    };
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(hit) = cache.get(&key) {
        return hit.clone();
    }
    let color = Color::new(rgba.r, rgba.g, rgba.b, rgba.a);
    let rendered = rasterize(formula, size, scale, color).map(Arc::new);
    if rendered.is_none() {
        log::debug!("math: couldn't rasterize {:?}", formula.latex);
    }
    if cache.len() >= CACHE_LIMIT {
        cache.clear();
    }
    cache.insert(key, rendered.clone());
    rendered
}

fn rasterize(formula: &Formula, size: f32, scale: f32, color: Color) -> Option<Rendered> {
    // Laid out in black: paint what's still black in the text color, keep `\color`s.
    let mut list = formula.list.clone();
    for item in &mut list.items {
        let c = match item {
            DisplayItem::GlyphPath { color, .. }
            | DisplayItem::Line { color, .. }
            | DisplayItem::Rect { color, .. }
            | DisplayItem::Path { color, .. } => color,
        };
        if *c == Color::BLACK {
            *c = color;
        }
    }
    let options = RenderOptions {
        font_size: size,
        padding: PAD,
        background_color: Color::new(0.0, 0.0, 0.0, 0.0),
        device_pixel_ratio: scale,
        ..Default::default()
    };
    let png = render_to_png(&list, &options).ok()?;
    let (w, h) = png_size(&png)?;
    Some(Rendered {
        image: Arc::new(Image::from_bytes(ImageFormat::Png, png)),
        width: w as f32 / scale,
        height: h as f32 / scale,
        baseline: PAD + list.height as f32 * size,
    })
}

/// Width and height from a PNG's IHDR chunk.
fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    let be = |at: usize| Some(u32::from_be_bytes(png.get(at..at + 4)?.try_into().ok()?));
    (png.get(1..4) == Some(b"PNG")).then_some(())?;
    Some((be(16)?, be(20)?))
}

/// Rewrites `\(…\)` to `$…$` and `\[…\]` to `$$…$$` (a `$$` block when `\[`
/// starts its line), outside code. A delimiter without its closing partner
/// in the same paragraph stays as it is, so while a reply streams the text
/// only changes once the formula is complete.
pub fn normalize_delimiters(text: &str) -> String {
    if !text.contains("\\(") && !text.contains("\\[") {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut fence: Option<(u8, usize)> = None;
    let mut lines = text.split_inclusive('\n').peekable();
    // Paragraph text outside fences is collected and rewritten in one go, so
    // a formula may span lines.
    let mut para = String::new();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        let marker = trimmed.as_bytes().first().copied();
        if let Some((ch, len)) = fence {
            out.push_str(line);
            let run = trimmed.bytes().take_while(|&b| b == ch).count();
            if run >= len && trimmed[run..].trim().is_empty() {
                fence = None;
            }
            continue;
        }
        if let Some(ch @ (b'`' | b'~')) = marker {
            let run = trimmed.bytes().take_while(|&b| b == ch).count();
            if run >= 3 {
                out.push_str(&rewrite_paragraph(&para));
                para.clear();
                out.push_str(line);
                fence = Some((ch, run));
                continue;
            }
        }
        para.push_str(line);
        if line.trim().is_empty() || lines.peek().is_none() {
            out.push_str(&rewrite_paragraph(&para));
            para.clear();
        }
    }
    out.push_str(&rewrite_paragraph(&para));
    out
}

/// What to do to a markdown view showing `old` (through [`normalize_delimiters`])
/// when the text grows to `new`.
pub enum Streamed {
    /// Append this to the view (`push_str`, cheap).
    Append(String),
    /// The rewrite changed earlier text (a formula closed): `set_text` this.
    Replace(String),
}

/// [`normalize_delimiters`] for streamed text: appends while it can.
pub fn stream(old: &str, new: &str) -> Streamed {
    let has = |s: &str| s.contains("\\(") || s.contains("\\[");
    if !has(new) && new.starts_with(old) {
        return Streamed::Append(new[old.len()..].to_string());
    }
    let (was, now) = (normalize_delimiters(old), normalize_delimiters(new));
    match now.strip_prefix(was.as_str()) {
        Some(rest) => Streamed::Append(rest.to_string()),
        None => Streamed::Replace(now),
    }
}

fn rewrite_paragraph(para: &str) -> String {
    let b = para.as_bytes();
    let mut out = String::with_capacity(para.len());
    let mut i = 0;
    let mut copied = 0;
    while i < b.len() {
        match b[i] {
            b'`' => {
                // A code span: skip to the matching backtick run.
                let run = b[i..].iter().take_while(|&&c| c == b'`').count();
                let ticks = &para[i..i + run];
                let rest = i + run;
                i = match find_run(&para[rest..], ticks) {
                    Some(at) => rest + at + run,
                    None => rest,
                };
            }
            b'\\' if i + 1 < b.len() => {
                let (close, display) = match b[i + 1] {
                    b'(' => ("\\)", false),
                    b'[' => ("\\]", true),
                    _ => {
                        i += 2; // an escape like `\\` or `\$`
                        continue;
                    }
                };
                let inner_start = i + 2;
                let Some(at) = para[inner_start..].find(close) else {
                    i += 2;
                    continue;
                };
                let inner = para[inner_start..inner_start + at].trim();
                let end = inner_start + at + 2;
                out.push_str(&para[copied..i]);
                if inner.is_empty() || inner.contains('$') {
                    out.push_str(&para[i..end]);
                } else if display && at_line_start(para, i) {
                    out.push_str("$$\n");
                    out.push_str(inner);
                    out.push_str("\n$$");
                } else if display {
                    out.push_str("$$");
                    out.push_str(inner);
                    out.push_str("$$");
                } else {
                    out.push('$');
                    out.push_str(inner);
                    out.push('$');
                }
                copied = end;
                i = end;
            }
            _ => i += 1,
        }
    }
    out.push_str(&para[copied..]);
    out
}

/// A run of exactly `ticks` backticks in `s`.
fn find_run(s: &str, ticks: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'`' {
            let run = b[i..].iter().take_while(|&&c| c == b'`').count();
            if run == ticks.len() {
                return Some(i);
            }
            i += run;
        } else {
            i += 1;
        }
    }
    None
}

fn at_line_start(s: &str, i: usize) -> bool {
    let line_start = s[..i].rfind('\n').map_or(0, |n| n + 1);
    // Allow list/quote indentation and markers before it.
    s[line_start..i]
        .chars()
        .all(|c| c.is_whitespace() || c == '>')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn dollars_vs_math() {
        let src = "costs $5 and $10 today";
        assert!(!looks_like_math("5 and ", src, 13));
        assert!(looks_like_math("x^2", "$x^2$ is", 5));
        assert!(!looks_like_math(" x", "$ x$", 4));
        assert!(!looks_like_math("x", "$x$5", 3));
    }

    #[::core::prelude::v1::test]
    fn typesets_and_rasterizes() {
        let f = typeset(r"\frac{-b \pm \sqrt{b^2-4ac}}{2a}", true).expect("parses");
        let r = rasterize(&f, 16.0, 2.0, Color::rgb(1.0, 1.0, 1.0)).expect("renders");
        assert!(r.width > 10.0 && r.height > 10.0);
        assert!(r.baseline > 0.0 && r.baseline < r.height);
        assert!(typeset(r"\frac{", false).is_none());
        assert!(typeset("  ", false).is_none());
    }

    #[::core::prelude::v1::test]
    fn rewrites_delimiters() {
        assert_eq!(normalize_delimiters(r"so \( x^2 \) holds"), "so $x^2$ holds");
        assert_eq!(
            normalize_delimiters("see\n\\[\na+b\n\\]\nok"),
            "see\n$$\na+b\n$$\nok"
        );
        assert_eq!(normalize_delimiters(r"mid \[a\] line"), "mid $$a$$ line");
        // Unclosed: left alone (streaming).
        assert_eq!(normalize_delimiters(r"open \(x"), r"open \(x");
        // Not inside code.
        assert_eq!(normalize_delimiters(r"`\(x\)` and \(y\)"), r"`\(x\)` and $y$");
        let fenced = "```\n\\(x\\)\n```\n\\(y\\)\n";
        assert_eq!(normalize_delimiters(fenced), "```\n\\(x\\)\n```\n$y$\n");
        // Escaped backslash isn't a delimiter.
        assert_eq!(normalize_delimiters(r"a \\(b\) c"), r"a \\(b\) c");
        assert!(matches!(stream("a \\(x", "a \\(x\\) b"), Streamed::Replace(s) if s == "a $x$ b"));
        assert!(matches!(stream("a $x$", "a $x$ b"), Streamed::Append(s) if s == " b"));
        // Doesn't cross a blank line.
        assert_eq!(normalize_delimiters("\\(a\n\nb\\)"), "\\(a\n\nb\\)");
    }
}
