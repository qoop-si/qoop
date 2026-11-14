//! One terminal on screen: the daemon's last screen for it, painted cell by cell.
//!
//! A snapshot row has one character per cell, and a wide character is followed by a
//! spacer cell holding a space. Each row is shaped whole (GPUI keeps a row's layout from
//! one frame to the next while its text and fonts are unchanged), but every glyph is then
//! painted at the column of the character it came from, never where the shaper put it: the
//! grid cannot drift, whatever the font's advance or a fallback glyph's width.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    App, Bounds, ContentMask, Element, ElementId, ElementInputHandler, Entity, EntityInputHandler,
    FocusHandle, Font, FontId, FontStyle, FontWeight, GlobalElementId, GlyphId, Hsla,
    InspectorElementId, IntoElement, LayoutId, Pixels, Point, Rgba, Style, TextRun, Window, fill,
    point, px, relative, size,
};
use qoop_domain::Viewport;
use qoop_runtime::client::viewport_for;
use qoop_runtime::control::TerminalStream;
use qoop_runtime::daemon::{ColourWire, SnapshotWire, SpanWire, attrs};

use crate::probe;

/// qoop's own ground and ink: the logo's background and its dots.
pub const BACKGROUND: [u8; 3] = [0x0f, 0x0f, 0x11];
pub const FOREGROUND: [u8; 3] = [0xfc, 0xf7, 0xf1];

/// The cell grid every pane uses: one font, one size.
#[derive(Clone, Debug)]
pub struct Cells {
    pub font: Font,
    pub font_size: Pixels,
    pub width: Pixels,
    pub height: Pixels,
}

impl Cells {
    /// How many columns and rows fit in `bounds`. The small slack keeps a pane sized to
    /// exactly N cells from reading as N - 1 after float rounding.
    pub fn fit(&self, bounds: Size2) -> (u16, u16) {
        let cols = (bounds.0 / self.width + 0.01).floor().clamp(1.0, 1000.0);
        let rows = (bounds.1 / self.height + 0.01).floor().clamp(1.0, 1000.0);
        (cols as u16, rows as u16)
    }
}

/// A width and a height in pixels.
pub type Size2 = (Pixels, Pixels);

/// The cell size of the terminal font: the advance of `m` across, 1.2 em down, both in
/// whole pixels so rows and columns never fall between device pixels.
pub fn cells(cx: &App, font_size: f32) -> Cells {
    let font = gpui::font("Menlo");
    let text = cx.text_system();
    let id = text.resolve_font(&font);
    let size = px(font_size);
    // Whole pixels across too: a fractional advance puts glyphs on four subpixel positions,
    // each rasterized on its own (Herdr moved to whole-pixel cells for the same reason).
    let width = text
        .advance(id, size, 'm')
        .map(|s| s.width.round())
        .unwrap_or(px((font_size * 0.6).round()))
        .max(px(1.));
    Cells {
        font,
        font_size: size,
        width,
        height: px((font_size * 1.2).ceil()),
    }
}

/// What one pane needs to paint itself, say its size and take typed text.
pub struct TerminalElement<V: 'static> {
    pub pane: usize,
    pub terminal: Option<TerminalStream>,
    pub screen: Option<Rc<SnapshotWire>>,
    pub cells: Cells,
    pub focused: bool,
    /// Drawn faded: the daemon is gone and this is the last screen it sent.
    pub stale: bool,
    /// The size last sent for this pane, so an equal size is never sent again: the daemon
    /// reads a repeated size as a nudge to repaint.
    pub sent: Rc<Cell<(u16, u16)>>,
    /// The part of a bigger grid this pane shows, kept so it moves only to follow the cursor.
    pub viewport: Rc<Cell<Option<Viewport>>>,
    /// Tells the terminal its new size.
    pub resize: Rc<dyn Fn(TerminalStream, u16, u16)>,
    /// Where text from the input method goes (dead keys, IME), for the focused pane.
    pub input: Option<(FocusHandle, Entity<V>)>,
}

impl<V: EntityInputHandler> IntoElement for TerminalElement<V> {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

/// One glyph, placed on the grid.
struct Glyph {
    at: Point<Pixels>,
    font: FontId,
    id: GlyphId,
    emoji: bool,
    ink: Hsla,
}

pub struct Painted {
    origin: Point<Pixels>,
    glyphs: Vec<Glyph>,
    quads: Vec<(Bounds<Pixels>, Hsla)>,
    /// Lines under and through text, painted over it.
    marks: Vec<(Bounds<Pixels>, Hsla)>,
    /// The cursor cell (two for a wide character) and the glyphs drawn in it.
    cursor: Option<(Bounds<Pixels>, Vec<usize>)>,
    frame: probe::Frame,
}

impl<V: EntityInputHandler> Element for TerminalElement<V> {
    type RequestLayoutState = ();
    type PrepaintState = Painted;

    fn id(&self) -> Option<ElementId> {
        Some(ElementId::Integer(self.pane as u64))
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _layout: &mut (),
        window: &mut Window,
        _cx: &mut App,
    ) -> Painted {
        let mut frame = probe::Frame::start();
        let bounds = on_device_pixels(bounds, window.scale_factor());
        let fit = self.cells.fit((bounds.size.width, bounds.size.height));
        if let Some(terminal) = self.terminal
            && fit != self.sent.get()
        {
            self.sent.set(fit);
            (self.resize)(terminal, fit.0, fit.1);
        }
        let mut painted = Painted {
            origin: bounds.origin,
            glyphs: Vec::new(),
            quads: Vec::new(),
            marks: Vec::new(),
            cursor: None,
            frame: probe::Frame::start(),
        };
        if let Some(screen) = self.screen.clone() {
            // A grid bigger than the pane (another client owns its size) is shown through a
            // window that follows the cursor, never wrapped: ADR-007.
            let view = viewport_for(&screen, self.viewport.get(), fit.0, fit.1);
            self.viewport.set(Some(view));
            layout(&screen, view, &self.cells, &mut painted, window);
        }
        frame.pause();
        painted.frame = frame;
        painted
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector: Option<&InspectorElementId>,
        pane: Bounds<Pixels>,
        _layout: &mut (),
        painted: &mut Painted,
        window: &mut Window,
        cx: &mut App,
    ) {
        painted.frame.resume();
        let shown = self.screen.is_some();
        warm_step(self.pane, shown, &self.cells, painted.origin, window);
        if let Some((focus, view)) = &self.input {
            window.handle_input(focus, ElementInputHandler::new(pane, view.clone()), cx);
        }
        window.with_content_mask(Some(ContentMask { bounds: pane }), |window| {
            window.paint_quad(fill(pane, rgb(BACKGROUND)));
            for (rect, bg) in &painted.quads {
                window.paint_quad(fill(*rect, *bg));
            }
            for glyph in &painted.glyphs {
                paint_glyph(glyph, glyph.ink, self.cells.font_size, window);
            }
            for (rect, ink) in &painted.marks {
                window.paint_quad(fill(*rect, *ink));
            }
            if let Some((cell, under)) = &painted.cursor {
                let ink = rgb(FOREGROUND);
                if self.focused {
                    // A block, and the character under it again in the ground colour.
                    window.paint_quad(fill(*cell, ink));
                    for &i in under {
                        paint_glyph(
                            &painted.glyphs[i],
                            rgb(BACKGROUND),
                            self.cells.font_size,
                            window,
                        );
                    }
                } else {
                    window.paint_quad(gpui::outline(*cell, ink, gpui::BorderStyle::Solid));
                }
            }
            if self.stale {
                window.paint_quad(fill(pane, rgb(BACKGROUND).opacity(0.6)));
            }
        });
        let frame = std::mem::replace(&mut painted.frame, probe::Frame::start());
        probe::painted(frame, self.pane, shown, window);
    }
}

fn paint_glyph(glyph: &Glyph, ink: Hsla, size: Pixels, window: &mut Window) {
    let _ = if glyph.emoji {
        window.paint_emoji(glyph.at, glyph.font, glyph.id, size)
    } else {
        window.paint_glyph(glyph.at, glyph.font, glyph.id, size, ink)
    };
}

thread_local! {
    /// How many of `FACES` are warm. The glyph atlas and the faces are per process, so
    /// warming them once is enough.
    static WARM_STEP: Cell<usize> = const { Cell::new(0) };
    /// A frame with a screen in it has been painted.
    static SHOWN: Cell<bool> = const { Cell::new(false) };
}

/// What a terminal draws most: printable ASCII, box drawing, blocks, the symbols agents
/// use, plus one wide character and one emoji so their fallback fonts are loaded.
const WARM: &str = concat!(
    " !\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`",
    "abcdefghijklmnopqrstuvwxyz{|}~",
    "─│┌┐└┘├┤┬┴┼╭╮╯╰═║█▀▄░▒▓•·…→←↑↓✓✗●○◆⏺⎿❯›",
    "中✅",
);

/// The faces warmed, one per frame: regular, bold, italic, bold italic.
const FACES: [u8; 4] = [0, attrs::BOLD, attrs::ITALIC, attrs::BOLD | attrs::ITALIC];

/// One step of the warm-up, in pane 0 only, so a frame never pays for more than one face.
///
/// Measured before it: the first busy frame of a process cost 18 to 22 ms against 2 to 3
/// for every later one, paying to resolve the bold and italic faces, to load the fallback
/// fonts and to rasterize each new glyph. Nothing is warmed before the first screen has
/// been painted, so the start is not delayed; then one face per frame, each frame asking
/// for the next.
fn warm_step(pane: usize, shown: bool, cells: &Cells, origin: Point<Pixels>, window: &mut Window) {
    if pane != 0 {
        return;
    }
    let step = WARM_STEP.get();
    if SHOWN.get() && step < FACES.len() {
        WARM_STEP.set(step + 1);
        let started = probe::Frame::start();
        warm(cells, origin, FACES[step], window);
        probe::note(|| serde_json::json!({"ev": "warm", "face": step, "cpu_us": started.cpu_us()}));
        if step + 1 < FACES.len() {
            another_frame(window);
        }
    }
    if shown && !SHOWN.replace(true) {
        another_frame(window);
    }
}

/// Ask for one more frame even if no screen changes. Through `on_next_frame` because a
/// window cannot be marked dirty while it draws.
fn another_frame(window: &Window) {
    window.on_next_frame(|window, _| window.refresh());
}

/// Rasterize every `WARM` glyph of one face into the atlas, clipped to nothing. Glyph by
/// glyph, because a line skips the glyphs its content mask hides and would stay cold.
fn warm(cells: &Cells, origin: Point<Pixels>, bits: u8, window: &mut Window) {
    let ink = rgb(FOREGROUND);
    let nowhere = ContentMask {
        bounds: Bounds::new(origin, size(px(0.), px(0.))),
    };
    let run = text_run(WARM.len(), ink, bits, cells);
    let line = window
        .text_system()
        .shape_line(WARM.into(), cells.font_size, &[run], None);
    window.with_content_mask(Some(nowhere), |window| {
        for (col, shaped) in line
            .runs
            .iter()
            .flat_map(|r| r.glyphs.iter().map(move |g| (r, g)))
            .enumerate()
        {
            let (run, glyph) = shaped;
            let glyph = Glyph {
                at: point(origin.x + cells.width * col as f32, origin.y + cells.height),
                font: run.font_id,
                id: glyph.id,
                emoji: glyph.is_emoji,
                ink,
            };
            paint_glyph(&glyph, ink, cells.font_size, window);
        }
    });
}

/// The pane's grid moved onto whole device pixels, so with a whole-pixel cell width every
/// glyph lands on the same subpixel position and is rasterized once, not once per position.
fn on_device_pixels(bounds: Bounds<Pixels>, scale: f32) -> Bounds<Pixels> {
    let snap = |v: Pixels| (v * scale).ceil() / scale;
    let origin = point(snap(bounds.origin.x), snap(bounds.origin.y));
    let size = size(
        bounds.size.width - (origin.x - bounds.origin.x),
        bounds.size.height - (origin.y - bounds.origin.y),
    );
    Bounds::new(origin, size)
}

/// Every row of `screen` inside `view`: backgrounds, glyphs on their columns, underline and
/// strike, and the cursor.
fn layout(
    screen: &SnapshotWire,
    view: Viewport,
    cells: &Cells,
    out: &mut Painted,
    window: &mut Window,
) {
    let (first_col, last_col) = (
        usize::from(view.col),
        usize::from(view.col) + usize::from(view.cols),
    );
    let x_of = |col: usize| out.origin.x + cells.width * (col - first_col) as f32;
    for (n, row) in (usize::from(view.row)..screen.lines.len())
        .take(usize::from(view.rows))
        .enumerate()
    {
        let text = &screen.lines[row];
        let spans = screen.styles.get(row).map(Vec::as_slice).unwrap_or(&[]);
        let y = out.origin.y + cells.height * n as f32;
        for span in spans {
            let (start, end) = (
                span.at.max(first_col),
                span.at.saturating_add(span.len).min(last_col),
            );
            if start >= end {
                continue;
            }
            if let (_, Some(bg)) = inks(span.fg, span.bg, span.attrs) {
                out.quads.push((
                    Bounds::new(
                        point(x_of(start), y),
                        size(cells.width * (end - start) as f32, cells.height),
                    ),
                    bg,
                ));
            }
        }
        if text.trim_end().is_empty() {
            continue;
        }
        // Byte offset of each cell, plus the end: runs are in cells, a glyph names a byte.
        let mut offsets: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
        offsets.push(text.len());
        let cols = offsets.len() - 1;
        let line = window.text_system().shape_line(
            text.clone().into(),
            cells.font_size,
            &font_runs(spans, &offsets, cells),
            None,
        );
        let baseline = y + (cells.height - line.ascent - line.descent) / 2. + line.ascent;
        let cursor_here = screen.modes.cursor_visible && screen.cursor.0 == row;
        for run in &line.runs {
            for (k, glyph) in run.glyphs.iter().enumerate() {
                let col = offsets.partition_point(|&b| b < glyph.index).min(cols);
                if col < first_col || col >= last_col {
                    continue;
                }
                let span = span_at(spans, col);
                let ink = inks(
                    span.and_then(|s| s.fg),
                    span.and_then(|s| s.bg),
                    span.map_or(0, |s| s.attrs),
                )
                .0;
                if cursor_here && col == screen.cursor.1 {
                    // Two cells when the font's own advance spans two: a wide character.
                    let next = run
                        .glyphs
                        .get(k + 1)
                        .map_or(glyph.position.x + line.width, |g| g.position.x);
                    let wide = next - glyph.position.x > cells.width * 1.5;
                    let width = if wide { cells.width * 2. } else { cells.width };
                    out.cursor = Some((
                        Bounds::new(point(x_of(col), y), size(width, cells.height)),
                        vec![out.glyphs.len()],
                    ));
                }
                out.glyphs.push(Glyph {
                    at: point(x_of(col), baseline),
                    font: run.font_id,
                    id: glyph.id,
                    emoji: glyph.is_emoji,
                    ink,
                });
            }
        }
        for span in spans {
            let (start, end) = (
                span.at.max(first_col),
                span.at.saturating_add(span.len).min(last_col).min(cols),
            );
            if start >= end || span.attrs & (attrs::UNDERLINE | attrs::STRIKE) == 0 {
                continue;
            }
            let ink = inks(span.fg, span.bg, span.attrs).0;
            let width = cells.width * (end - start) as f32;
            if span.attrs & attrs::UNDERLINE != 0 {
                out.marks.push((
                    Bounds::new(point(x_of(start), baseline + px(1.)), size(width, px(1.))),
                    ink,
                ));
            }
            if span.attrs & attrs::STRIKE != 0 {
                let mid = baseline - line.ascent * 0.3;
                out.marks.push((
                    Bounds::new(point(x_of(start), mid), size(width, px(1.))),
                    ink,
                ));
            }
        }
    }
    // A cursor on a blank cell: the block with nothing under it.
    let (row, col) = screen.cursor;
    if out.cursor.is_none()
        && screen.modes.cursor_visible
        && (usize::from(view.row)..usize::from(view.row) + usize::from(view.rows)).contains(&row)
        && (first_col..last_col).contains(&col)
    {
        let y = out.origin.y + cells.height * (row - usize::from(view.row)) as f32;
        out.cursor = Some((
            Bounds::new(point(x_of(col), y), size(cells.width, cells.height)),
            Vec::new(),
        ));
    }
}

/// The run covering `col`, if one does. Runs come in column order and do not overlap.
fn span_at(spans: &[SpanWire], col: usize) -> Option<&SpanWire> {
    spans
        .iter()
        .find(|s| s.at <= col && col < s.at.saturating_add(s.len))
}

/// The font of each stretch of the row, for shaping: bold and italic change the glyphs,
/// colour does not, so the shaped layout survives a colour change.
fn font_runs(spans: &[SpanWire], offsets: &[usize], cells: &Cells) -> Vec<TextRun> {
    let cols = offsets.len() - 1;
    let byte = |col: usize| offsets[col.min(cols)];
    let ink = rgb(FOREGROUND);
    let mut runs = Vec::with_capacity(spans.len() * 2 + 1);
    let mut at = 0usize;
    for span in spans {
        let start = span.at.min(cols).max(at);
        let end = span.at.saturating_add(span.len).min(cols);
        if start > at {
            runs.push(text_run(byte(start) - byte(at), ink, 0, cells));
            at = start;
        }
        if end > at {
            runs.push(text_run(byte(end) - byte(at), ink, span.attrs, cells));
            at = end;
        }
    }
    if cols > at {
        runs.push(text_run(byte(cols) - byte(at), ink, 0, cells));
    }
    runs
}

fn text_run(len: usize, color: Hsla, bits: u8, cells: &Cells) -> TextRun {
    let mut font = cells.font.clone();
    if bits & attrs::BOLD != 0 {
        font.weight = FontWeight::BOLD;
    }
    if bits & attrs::ITALIC != 0 {
        font.style = FontStyle::Italic;
    }
    TextRun {
        len,
        font,
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

/// A run's text colour and its background, if it has one of its own.
fn inks(fg: Option<ColourWire>, bg: Option<ColourWire>, bits: u8) -> (Hsla, Option<Hsla>) {
    let mut ink = fg.map_or(rgb(FOREGROUND), resolve);
    let mut back = bg.map(resolve);
    if bits & attrs::INVERSE != 0 {
        let swapped = back.unwrap_or(rgb(BACKGROUND));
        back = Some(ink);
        ink = swapped;
    }
    if bits & attrs::DIM != 0 {
        ink = ink.opacity(0.6);
    }
    (ink, back)
}

pub fn rgb([r, g, b]: [u8; 3]) -> Hsla {
    Rgba {
        r: f32::from(r) / 255.,
        g: f32::from(g) / 255.,
        b: f32::from(b) / 255.,
        a: 1.,
    }
    .into()
}

fn resolve(c: ColourWire) -> Hsla {
    match c {
        ColourWire::Rgb { r, g, b } => rgb([r, g, b]),
        ColourWire::Indexed { i } => rgb(indexed(i)),
    }
}

/// The 256-colour table as xterm defines it: its 16 default colours, the 6x6x6 cube, the
/// grey ramp. A standard, not a theme; qoop's own ground and ink are above. The palette a
/// person sees in the end is a design pick for the owner.
fn indexed(i: u8) -> [u8; 3] {
    const BASE: [[u8; 3]; 16] = [
        [0x00, 0x00, 0x00],
        [0xcd, 0x00, 0x00],
        [0x00, 0xcd, 0x00],
        [0xcd, 0xcd, 0x00],
        [0x00, 0x00, 0xee],
        [0xcd, 0x00, 0xcd],
        [0x00, 0xcd, 0xcd],
        [0xe5, 0xe5, 0xe5],
        [0x7f, 0x7f, 0x7f],
        [0xff, 0x00, 0x00],
        [0x00, 0xff, 0x00],
        [0xff, 0xff, 0x00],
        [0x5c, 0x5c, 0xff],
        [0xff, 0x00, 0xff],
        [0x00, 0xff, 0xff],
        [0xff, 0xff, 0xff],
    ];
    match i {
        0..=15 => BASE[i as usize],
        16..=231 => {
            let n = i - 16;
            let step = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            [step(n / 36), step((n / 6) % 6), step(n % 6)]
        }
        _ => {
            let v = 8 + (i - 232) * 10;
            [v, v, v]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Asked = Rc<std::cell::RefCell<Vec<(TerminalStream, u16, u16)>>>;

    #[test]
    fn the_colour_table_matches_xterm() {
        assert_eq!(indexed(1), [0xcd, 0, 0]);
        assert_eq!(indexed(16), [0, 0, 0]);
        assert_eq!(indexed(196), [255, 0, 0]);
        assert_eq!(indexed(231), [255, 255, 255]);
        assert_eq!(indexed(232), [8, 8, 8]);
        assert_eq!(indexed(255), [238, 238, 238]);
    }

    #[test]
    fn inverse_swaps_and_fills_the_default_background() {
        let red = Some(ColourWire::Indexed { i: 196 });
        let (ink, back) = inks(red, None, attrs::INVERSE);
        assert_eq!(ink, rgb(BACKGROUND));
        assert_eq!(back, Some(rgb([255, 0, 0])));
    }

    fn screen(
        lines: &[&str],
        cursor: (usize, usize),
        cols: usize,
        rows: usize,
    ) -> Rc<SnapshotWire> {
        Rc::new(SnapshotWire {
            cols,
            rows,
            cursor,
            lines: lines.iter().map(|l| l.to_string()).collect(),
            styles: vec![vec![SpanWire {
                at: 3,
                len: 1,
                fg: None,
                bg: Some(ColourWire::Indexed { i: 2 }),
                attrs: attrs::BOLD | attrs::UNDERLINE,
                link: None,
            }]],
            links: Vec::new(),
            modes: Default::default(),
            title: String::new(),
            behind: 0,
        })
    }

    fn element(
        screen: Rc<SnapshotWire>,
        asked: Asked,
        sent: Rc<Cell<(u16, u16)>>,
    ) -> TerminalElement<crate::field::Field> {
        TerminalElement {
            pane: 1,
            terminal: Some(7),
            screen: Some(screen),
            cells: Cells {
                font: gpui::font("Menlo"),
                font_size: px(12.),
                width: px(8.),
                height: px(16.),
            },
            focused: true,
            stale: false,
            sent,
            viewport: Rc::new(Cell::new(None)),
            resize: Rc::new(move |t, c, r| asked.borrow_mut().push((t, c, r))),
            input: None,
        }
    }

    /// Headless, through GPUI's test platform: every glyph sits on its column whatever the
    /// shaper's advance, a coloured run gets its background and underline, the cursor is
    /// placed, and the terminal is asked for the pane's size once.
    #[gpui::test]
    fn a_pane_paints_on_the_grid_and_sizes_its_terminal(cx: &mut gpui::TestAppContext) {
        let asked = Asked::default();
        let sent = Rc::new(Cell::new((0, 0)));
        let window = cx.add_empty_window();
        let snap = screen(&["abcdef", "$ ls"], (1, 4), 100, 30);
        for _ in 0..2 {
            let e = element(snap.clone(), asked.clone(), sent.clone());
            let (_, painted) =
                window.draw(point(px(0.), px(0.)), size(px(800.), px(480.)), |_, _| e);
            let xs: Vec<Pixels> = painted.glyphs.iter().take(6).map(|g| g.at.x).collect();
            let want: Vec<Pixels> = (0..6).map(|c| px(8.) * c as f32).collect();
            assert_eq!(xs, want, "glyphs sit on their columns");
            assert_eq!(painted.quads.len(), 1, "one run has a background");
            assert_eq!(
                painted.quads[0].0.origin.x,
                px(24.),
                "the run starts at column 3"
            );
            assert_eq!(painted.marks.len(), 1, "and is underlined");
            let (cursor, _) = painted.cursor.as_ref().expect("the cursor is shown");
            assert_eq!(cursor.origin, point(px(32.), px(16.)));
        }
        assert_eq!(asked.borrow().first(), Some(&(7, 100, 30)));
        assert!(
            asked.borrow().len() == 1,
            "an unchanged size is not sent again"
        );
    }

    /// A grid taller than the pane (another client owns the size) is shown around the
    /// cursor, not cut off at the top.
    #[gpui::test]
    fn a_bigger_grid_follows_the_cursor(cx: &mut gpui::TestAppContext) {
        let up = Asked::default();
        let window = cx.add_empty_window();
        let lines: Vec<String> = (0..40).map(|i| format!("row {i}")).collect();
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        let snap = screen(&lines, (39, 0), 80, 40);
        let e = element(snap, up, Rc::new(Cell::new((0, 0))));
        let (_, painted) = window.draw(point(px(0.), px(0.)), size(px(640.), px(160.)), |_, _| e);
        let (cursor, _) = painted
            .cursor
            .as_ref()
            .expect("the cursor row is on screen");
        assert_eq!(cursor.origin.y, px(16.) * 9., "the last of ten rows");
    }
}
