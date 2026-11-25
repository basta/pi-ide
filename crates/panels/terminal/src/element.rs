//! Paints alacritty's grid: background runs, text batched per style, the
//! cursor and IME preedit. Text is shaped per batch and placed at its column,
//! so the grid stays aligned even when a glyph comes from a fallback font.

use alacritty_terminal::grid::Dimensions as _;
use alacritty_terminal::index::Point as GridPoint;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::CursorShape;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

use crate::palette::Palette;
use crate::session::GridSize;
use crate::view::{Geometry, TerminalView};

pub const LINE_HEIGHT: f32 = 1.3;

pub struct TerminalElement {
    view: Entity<TerminalView>,
    focus: FocusHandle,
}

impl TerminalElement {
    pub fn new(view: Entity<TerminalView>, focus: FocusHandle) -> Self {
        Self { view, focus }
    }
}

pub struct Frame {
    background: Hsla,
    rects: Vec<(Bounds<Pixels>, Hsla)>,
    texts: Vec<(Point<Pixels>, ShapedLine)>,
    cursor: Option<PaintQuad>,
    cursor_text: Option<(Point<Pixels>, ShapedLine)>,
    preedit: Option<(Point<Pixels>, ShapedLine, Bounds<Pixels>)>,
    line_height: Pixels,
}

impl IntoElement for TerminalElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Style {
    fg: Hsla,
    bold: bool,
    italic: bool,
    underline: bool,
    undercurl: bool,
    strike: bool,
}

struct Batch {
    col: usize,
    text: String,
    style: Style,
    ascii: bool,
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = Frame;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = gpui_kit::Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Frame {
        let theme = cx.theme();
        let font_size = theme.mono_font_size;
        let base_font = font(theme.mono_font_family.clone());
        let palette = Palette::from_theme(cx);
        let text_system = window.text_system().clone();
        let font_id = text_system.resolve_font(&base_font);
        let cell_width = text_system
            .advance(font_id, font_size, 'm')
            .map(|s| s.width)
            .unwrap_or(font_size * 0.6);
        let line_height = (font_size * LINE_HEIGHT).round();
        let size = GridSize {
            cols: ((bounds.size.width / cell_width).floor() as usize).max(2),
            rows: ((bounds.size.height / line_height).floor() as usize).max(1),
        };
        let geometry = Geometry {
            origin: bounds.origin,
            cell: gpui_kit::size(cell_width, line_height),
            size,
        };
        let focused = self.focus.is_focused(window);
        let (snapshot, preedit) = self.view.update(cx, |view, _| {
            view.set_geometry(geometry);
            (view.snapshot(&palette), view.preedit().map(str::to_string))
        });

        let mut frame = Frame {
            background: palette.background,
            rects: Vec::new(),
            texts: Vec::new(),
            cursor: None,
            cursor_text: None,
            preedit: None,
            line_height,
        };
        let Some(snapshot) = snapshot else {
            return frame;
        };
        let origin_of = |row: usize, col: usize| {
            point(
                bounds.origin.x + cell_width * col as f32,
                bounds.origin.y + line_height * row as f32,
            )
        };
        let shape = |text: String, style: Style, window: &mut Window| {
            let mut f = base_font.clone();
            if style.bold {
                f.weight = FontWeight::BOLD;
            }
            if style.italic {
                f.style = FontStyle::Italic;
            }
            let run = TextRun {
                len: text.len(),
                font: f,
                color: style.fg,
                background_color: None,
                underline: (style.underline || style.undercurl).then_some(UnderlineStyle {
                    color: Some(style.fg),
                    thickness: px(1.),
                    wavy: style.undercurl,
                }),
                strikethrough: style.strike.then_some(StrikethroughStyle {
                    color: Some(style.fg),
                    thickness: px(1.),
                }),
            };
            window
                .text_system()
                .shape_line(text.into(), font_size, &[run], None)
        };

        for (row, cells) in snapshot.rows.iter().enumerate() {
            let mut bg_run: Option<(usize, usize, Hsla)> = None;
            let mut batches: Vec<Batch> = Vec::new();
            for cell in cells {
                if cell.bg != palette.background {
                    match &mut bg_run {
                        Some((_, end, color)) if *end == cell.col && *color == cell.bg => {
                            *end = cell.col + cell.width;
                        }
                        _ => {
                            if let Some((start, end, color)) = bg_run.take() {
                                frame.rects.push((
                                    cell_bounds(origin_of(row, start), end - start, geometry.cell),
                                    color,
                                ));
                            }
                            bg_run = Some((cell.col, cell.col + cell.width, cell.bg));
                        }
                    }
                }
                if cell.text == " " && !cell.style.underline && !cell.style.strike {
                    continue;
                }
                let ascii = cell.text.is_ascii();
                match batches.last_mut() {
                    Some(b)
                        if ascii
                            && b.ascii
                            && b.style == cell.style
                            && b.col + b.text.len() <= cell.col =>
                    {
                        let gap = cell.col - (b.col + b.text.len());
                        b.text.extend(std::iter::repeat_n(' ', gap));
                        b.text.push_str(&cell.text);
                    }
                    _ => batches.push(Batch {
                        col: cell.col,
                        text: cell.text.clone(),
                        style: cell.style,
                        ascii,
                    }),
                }
            }
            if let Some((start, end, color)) = bg_run {
                frame.rects.push((
                    cell_bounds(origin_of(row, start), end - start, geometry.cell),
                    color,
                ));
            }
            for b in batches {
                let line = shape(b.text, b.style, window);
                frame.texts.push((origin_of(row, b.col), line));
            }
        }

        if let Some(c) = snapshot.cursor {
            let at = origin_of(c.row, c.col);
            let cell = gpui_kit::size(cell_width * c.width as f32, line_height);
            let block = Bounds::new(at, cell);
            frame.cursor = Some(match (c.shape, focused) {
                (CursorShape::Beam, true) => fill(
                    Bounds::new(at, gpui_kit::size(px(2.), line_height)),
                    palette.cursor,
                ),
                (CursorShape::Underline, true) => fill(
                    Bounds::new(
                        point(at.x, at.y + line_height - px(2.)),
                        gpui_kit::size(cell.width, px(2.)),
                    ),
                    palette.cursor,
                ),
                (CursorShape::Block, true) => {
                    if !c.text.trim().is_empty() {
                        let style = Style {
                            fg: palette.background,
                            ..c.style
                        };
                        frame.cursor_text = Some((at, shape(c.text.clone(), style, window)));
                    }
                    fill(block, palette.cursor)
                }
                _ => outline(block, palette.cursor, BorderStyle::Solid),
            });
            if let Some(text) = preedit.filter(|t| !t.is_empty()) {
                let style = Style {
                    fg: palette.foreground,
                    bold: false,
                    italic: false,
                    underline: true,
                    undercurl: false,
                    strike: false,
                };
                let line = shape(text, style, window);
                let width = line.width;
                frame.cursor = None;
                frame.cursor_text = None;
                frame.preedit = Some((
                    at,
                    line,
                    Bounds::new(at, gpui_kit::size(width, line_height)),
                ));
            }
        }
        frame
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        frame: &mut Frame,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.handle_input(
            &self.focus,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );
        window.paint_quad(fill(bounds, frame.background));
        for (r, color) in &frame.rects {
            window.paint_quad(fill(*r, *color));
        }
        let lh = frame.line_height;
        for (at, line) in &frame.texts {
            let _ = line.paint(*at, lh, TextAlign::Left, None, window, cx);
        }
        if let Some(cursor) = frame.cursor.take() {
            window.paint_quad(cursor);
        }
        if let Some((at, line)) = &frame.cursor_text {
            let _ = line.paint(*at, lh, TextAlign::Left, None, window, cx);
        }
        if let Some((at, line, under)) = &frame.preedit {
            window.paint_quad(fill(*under, frame.background));
            let _ = line.paint(*at, lh, TextAlign::Left, None, window, cx);
        }
    }
}

fn cell_bounds(at: Point<Pixels>, cols: usize, cell: Size<Pixels>) -> Bounds<Pixels> {
    Bounds::new(at, gpui_kit::size(cell.width * cols as f32, cell.height))
}

/// What the element needs from the grid, copied out under the lock.
pub struct Snapshot {
    rows: Vec<Vec<SnapCell>>,
    cursor: Option<SnapCursor>,
}

struct SnapCell {
    col: usize,
    width: usize,
    text: String,
    bg: Hsla,
    style: Style,
}

struct SnapCursor {
    row: usize,
    col: usize,
    width: usize,
    shape: CursorShape,
    text: String,
    style: Style,
}

pub fn snapshot<T: alacritty_terminal::event::EventListener>(
    term: &alacritty_terminal::Term<T>,
    palette: &Palette,
) -> Snapshot {
    let content = term.renderable_content();
    let offset = content.display_offset as i32;
    let rows_n = term.screen_lines();
    let colors = content.colors;
    let selection = content.selection;
    let mut rows: Vec<Vec<SnapCell>> = (0..rows_n).map(|_| Vec::new()).collect();
    let cursor_point = content.cursor.point;
    let cursor_shape = content.cursor.shape;
    let mut cursor = None;
    for indexed in content.display_iter {
        let row = indexed.point.line.0 + offset;
        if row < 0 || row as usize >= rows_n {
            continue;
        }
        let cell = indexed.cell;
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        let mut fg = palette.resolve(cell.fg, colors);
        let mut bg = palette.resolve(cell.bg, colors);
        if cell.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cell.flags.contains(Flags::DIM) {
            fg = fg.opacity(0.66);
        }
        if cell.flags.contains(Flags::HIDDEN) {
            fg = bg;
        }
        if selection.is_some_and(|s| s.contains(indexed.point)) {
            bg = palette.selection;
        }
        let mut text = String::new();
        text.push(cell.c);
        if let Some(extra) = cell.zerowidth() {
            text.extend(extra);
        }
        let style = Style {
            fg,
            bold: cell.flags.contains(Flags::BOLD),
            italic: cell.flags.contains(Flags::ITALIC),
            underline: cell.flags.intersects(
                Flags::UNDERLINE
                    | Flags::DOUBLE_UNDERLINE
                    | Flags::DOTTED_UNDERLINE
                    | Flags::DASHED_UNDERLINE,
            ),
            undercurl: cell.flags.contains(Flags::UNDERCURL),
            strike: cell.flags.contains(Flags::STRIKEOUT),
        };
        let width = if cell.flags.contains(Flags::WIDE_CHAR) {
            2
        } else {
            1
        };
        let col = indexed.point.column.0;
        if indexed.point == cursor_point && cursor_shape != CursorShape::Hidden {
            cursor = Some(SnapCursor {
                row: row as usize,
                col,
                width,
                shape: cursor_shape,
                text: text.clone(),
                style,
            });
        }
        rows[row as usize].push(SnapCell {
            col,
            width,
            text,
            bg,
            style,
        });
    }
    Snapshot { rows, cursor }
}

/// The screen cell under `position`, and which half of it.
pub fn grid_point(
    g: &Geometry,
    position: Point<Pixels>,
    display_offset: usize,
) -> (GridPoint, alacritty_terminal::index::Side) {
    use alacritty_terminal::index::{Column, Line, Side};
    let x = f32::from(position.x - g.origin.x).max(0.);
    let y = f32::from(position.y - g.origin.y).max(0.);
    let cw = f32::from(g.cell.width);
    let col = ((x / cw) as usize).min(g.size.cols - 1);
    let row = ((y / f32::from(g.cell.height)) as usize).min(g.size.rows - 1);
    let side = if x - col as f32 * cw > cw / 2. {
        Side::Right
    } else {
        Side::Left
    };
    (
        GridPoint::new(Line(row as i32 - display_offset as i32), Column(col)),
        side,
    )
}
