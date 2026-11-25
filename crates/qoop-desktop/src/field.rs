//! The text field: one paragraph, wrapped to the field's width so nothing runs past its edge,
//! growing a line at a time up to [`MAX_LINES`] and scrolling past them to keep the caret's
//! line in view. Typing goes through the input method (dead keys, IME, the marked text drawn
//! underlined until it commits); a caret, a selection, word moves, copy, cut, paste, and undo
//! of the last edit.
//!
//! Enter and Escape are left to the field's owner, which listens for them around it. A field
//! that is told it is `multiline` takes Shift+Enter, Option+Enter and Ctrl+J as a new line, and
//! paragraphs stack, each wrapped to the width.

use std::ops::Range;

use gpui::{
    App, AvailableSpace, Bounds, ClipboardItem, ContentMask, Context, Element, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, FocusHandle, Focusable, Font, GlobalElementId,
    Hsla, InspectorElementId, IntoElement, KeyDownEvent, LayoutId, MouseButton, MouseDownEvent,
    Pixels, Point, Render, Role, SharedString, Style, TextAlign, TextRun, UTF16Selection,
    UnderlineStyle, Window, WrappedLine, div, fill, point, prelude::*, px, relative, size,
};

/// The most lines a field grows to; past them it scrolls inside, so a long paste never pushes
/// a sheet's buttons off the window.
pub const MAX_LINES: usize = 6;

/// How the field's text was last shaped, to find a point or a character again between frames.
#[derive(Clone)]
struct Shape {
    font: Font,
    font_size: Pixels,
    line_height: Pixels,
    bounds: Bounds<Pixels>,
}

use crate::theme::{self, c, ca};
use crate::ui::{self, Control};

/// The field's text and where the caret and selection are, as byte offsets into `text`.
pub struct Field {
    pub text: String,
    /// The selection, `anchor..head`; the caret is `head`. Equal when nothing is selected.
    anchor: usize,
    head: usize,
    /// What the input method is composing, as a byte range of `text`: drawn underlined.
    pub marked: Option<Range<usize>>,
    /// The text and caret before the last edit, for one undo.
    undo: Option<(String, usize)>,
    pub placeholder: SharedString,
    /// The field's name to a screen reader, apart from its hint: "Folder", not an example
    /// path. The hint names it when there is none.
    pub label: Option<SharedString>,
    pub focus: FocusHandle,
    /// Painted geometry, for the input method's candidate window and for clicks.
    shape: Option<Shape>,
    /// How many lines the text wraps to, and the first one shown when there are more than
    /// [`MAX_LINES`].
    pub rows: usize,
    pub top: usize,
    /// Whether a new line can be typed: the box is for words sent on Enter, which a person
    /// composes over several lines. Off, a line end can neither be typed nor pasted.
    pub multiline: bool,
}

/// The field's inks, from the theme the window draws in now (never kept: a field made before
/// a switch to light would keep dark's ink).
struct Inks {
    text: Hsla,
    placeholder: Hsla,
    caret: Hsla,
    selection: Hsla,
}

impl Inks {
    fn now(cx: &App) -> Self {
        let t = theme::current(cx);
        Inks {
            text: c(t.ink).into(),
            placeholder: c(t.t2).into(),
            caret: c(t.ink).into(),
            selection: ca(t.fill2).into(),
        }
    }
}

impl Field {
    pub fn new(placeholder: &str, cx: &mut Context<Self>) -> Self {
        Field {
            text: String::new(),
            anchor: 0,
            head: 0,
            marked: None,
            undo: None,
            placeholder: placeholder.to_string().into(),
            label: None,
            focus: cx.focus_handle(),
            shape: None,
            rows: 1,
            top: 0,
            multiline: false,
        }
    }

    /// Where the field was last painted, and its line height: what the tests measure.
    #[cfg(test)]
    pub fn drawn(&self) -> Option<Bounds<Pixels>> {
        self.shape.as_ref().map(|s| s.bounds)
    }

    #[cfg(test)]
    pub fn line_height(&self) -> Option<Pixels> {
        self.shape.as_ref().map(|s| s.line_height)
    }

    /// The text wrapped as it was last painted, for a point or a character's place.
    fn wrapped(&self, window: &Window) -> Option<(Wrapped, &Shape)> {
        let shape = self.shape.as_ref()?;
        let runs = [plain_run(
            self.text.len(),
            shape.font.clone(),
            Hsla::default(),
        )];
        let line = wrap(
            window,
            self.text.clone().into(),
            shape.font_size,
            &runs,
            shape.bounds.size.width,
        );
        Some((line, shape))
    }

    /// A point in the window, as a byte of `text`.
    fn index_at(&self, at: Point<Pixels>, window: &Window) -> Option<usize> {
        let (line, shape) = self.wrapped(window)?;
        let local = at - shape.bounds.origin + point(px(0.), shape.line_height * self.top);
        Some(line.index_at(local, shape.line_height).min(self.text.len()))
    }

    /// Put `text` in the field with the caret after it, as a field filled for editing reads:
    /// typing then adds to the end, never in front of what is there.
    pub fn set_text(&mut self, text: &str) {
        self.text = text.to_string();
        self.anchor = self.text.len();
        self.head = self.text.len();
        self.marked = None;
        self.undo = None;
    }

    /// Select everything typed: a name opened for renaming is replaced by what is typed next.
    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.head = self.text.len();
    }

    pub fn selection(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    /// Keep the text as it is now for one undo, unless a composition is under way: then the
    /// text from before it began is the one an undo brings back, never the half-typed romaji.
    fn keep_undo(&mut self) {
        if self.marked.is_none() {
            self.undo = Some((self.text.clone(), self.head));
        }
    }

    /// Put `new` in place of `range`, keeping one step of undo.
    fn replace(&mut self, range: Range<usize>, new: &str) {
        self.keep_undo();
        self.text.replace_range(range.clone(), new);
        self.head = range.start + new.len();
        self.anchor = self.head;
        self.marked = None;
    }

    fn prev(&self, at: usize) -> usize {
        self.text[..at].char_indices().last().map_or(0, |(i, _)| i)
    }

    fn next(&self, at: usize) -> usize {
        self.text[at..]
            .chars()
            .next()
            .map_or(at, |c| at + c.len_utf8())
    }

    /// The start of the word before `at`, as Option+Left finds it: past the spaces and
    /// punctuation, then back over the letters and digits.
    fn word_before(&self, at: usize) -> usize {
        let mut i = at;
        let mut word = false;
        for (j, c) in self.text[..at].char_indices().rev() {
            if c.is_alphanumeric() {
                word = true;
            } else if word {
                break;
            }
            i = j;
        }
        i
    }

    /// The end of the word after `at`, as Option+Right finds it.
    fn word_after(&self, at: usize) -> usize {
        let mut word = false;
        for (j, c) in self.text[at..].char_indices() {
            if c.is_alphanumeric() {
                word = true;
            } else if word {
                return at + j;
            }
        }
        self.text.len()
    }

    fn move_to(&mut self, at: usize, extend: bool) {
        self.head = at;
        if !extend {
            self.anchor = at;
        }
    }

    /// The caret one shown line down or up, at the same distance from the left: above the first
    /// line it goes to the start, below the last to the end.
    fn move_line(&mut self, down: bool, extend: bool, window: &Window) {
        let Some((wrapped, shape)) = self.wrapped(window) else {
            return;
        };
        let lh = shape.line_height;
        let here = wrapped
            .position_for_index(self.head, lh)
            .unwrap_or_default();
        let row = (here.y / lh).round() as usize;
        let to = match (down, row) {
            (false, 0) => 0,
            (true, row) if row + 1 >= wrapped.rows() => self.text.len(),
            (down, _) => {
                let y = here.y + if down { lh * 1.5 } else { -lh * 0.5 };
                wrapped.index_at(point(here.x, y), lh)
            }
        };
        self.move_to(to.min(self.text.len()), extend);
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &event.keystroke;
        let m = &k.modifiers;
        let sel = self.selection();
        let handled = match (k.key.as_str(), m.platform, m.shift) {
            _ if self.multiline && crate::keys::newline(k) => {
                self.replace(sel, "\n");
                true
            }
            ("up" | "down", false, extend) if self.multiline && !m.alt => {
                self.move_line(k.key == "down", extend, window);
                true
            }
            // Option deletes the word before, Cmd everything before the caret.
            ("backspace", cmd, _) if (cmd || m.alt) && sel.is_empty() => {
                let from = if cmd { 0 } else { self.word_before(self.head) };
                if from < self.head {
                    self.replace(from..self.head, "");
                }
                true
            }
            ("left", false, extend) if m.alt => {
                self.move_to(self.word_before(self.head), extend);
                true
            }
            ("right", false, extend) if m.alt => {
                self.move_to(self.word_after(self.head), extend);
                true
            }
            ("backspace", _, _) => {
                let range = if sel.is_empty() {
                    self.prev(self.head)..self.head
                } else {
                    sel
                };
                if !range.is_empty() {
                    self.replace(range, "");
                }
                true
            }
            ("delete", _, _) => {
                let range = if sel.is_empty() {
                    self.head..self.next(self.head)
                } else {
                    sel
                };
                if !range.is_empty() {
                    self.replace(range, "");
                }
                true
            }
            ("left", true, extend) | ("home", _, extend) => {
                self.move_to(0, extend);
                true
            }
            ("right", true, extend) | ("end", _, extend) => {
                self.move_to(self.text.len(), extend);
                true
            }
            ("left", false, extend) => {
                let at = if !extend && !sel.is_empty() {
                    sel.start
                } else {
                    self.prev(self.head)
                };
                self.move_to(at, extend);
                true
            }
            ("right", false, extend) => {
                let at = if !extend && !sel.is_empty() {
                    sel.end
                } else {
                    self.next(self.head)
                };
                self.move_to(at, extend);
                true
            }
            ("a", true, false) => {
                self.anchor = 0;
                self.head = self.text.len();
                true
            }
            ("c" | "x", true, false) => {
                if !sel.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(
                        self.text[sel.clone()].to_string(),
                    ));
                    if k.key == "x" {
                        self.replace(sel, "");
                    }
                }
                true
            }
            ("v", true, false) => {
                if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                    // A pasted line end is a space, Enter belongs to the owner; a multi-line
                    // field keeps the lines.
                    let text = text.replace("\r\n", "\n").replace('\r', "\n");
                    let one: String = text
                        .chars()
                        .map(|c| match c {
                            '\n' if self.multiline => c,
                            c if c.is_control() => ' ',
                            c => c,
                        })
                        .collect();
                    self.replace(sel, &one);
                }
                true
            }
            ("z", true, false) => {
                if let Some((text, head)) = self.undo.take() {
                    self.text = text;
                    self.head = head.min(self.text.len());
                    self.anchor = self.head;
                    self.marked = None;
                }
                true
            }
            _ => false,
        };
        if handled {
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn click(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        if let Some(at) = self.index_at(event.position, window) {
            self.move_to(at, event.modifiers.shift);
        }
        cx.notify();
    }

    // The input method counts in UTF-16 units; the field keeps byte offsets. These two are
    // the only crossings.

    /// A range the input method names, as bytes of `text`, start before end.
    fn bytes_of(&self, units: Range<usize>) -> Range<usize> {
        let (a, b) = (
            byte_at(&self.text, units.start),
            byte_at(&self.text, units.end),
        );
        a.min(b)..a.max(b)
    }

    /// A byte range of `text`, in the input method's units.
    fn units_of(&self, bytes: Range<usize>) -> Range<usize> {
        units_at(&self.text, bytes.start)..units_at(&self.text, bytes.end)
    }

    /// What an edit from the input method replaces: the range it names, else what is being
    /// composed, else the selection (AppKit's rule for a replacement range of "not found").
    fn target(&self, units: Option<Range<usize>>) -> Range<usize> {
        match (units, &self.marked) {
            (Some(units), _) => self.bytes_of(units),
            (None, Some(marked)) => marked.clone(),
            (None, None) => self.selection(),
        }
    }
}

/// The byte where UTF-16 offset `units` falls in `text`, on a character's start: an offset
/// inside a character (half a surrogate pair) is that character's start, one past the end is
/// the end.
fn byte_at(text: &str, units: usize) -> usize {
    let mut through = 0;
    text.char_indices()
        .find(|(_, c)| {
            through += c.len_utf16();
            through > units
        })
        .map_or(text.len(), |(at, _)| at)
}

/// The UTF-16 offset of byte `at` of `text`, the whole length for anything past it.
fn units_at(text: &str, at: usize) -> usize {
    text.get(..at).unwrap_or(text).encode_utf16().count()
}

impl EntityInputHandler for Field {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let bytes = self.bytes_of(range);
        *adjusted = Some(self.units_of(bytes.clone()));
        self.text.get(bytes).map(str::to_string)
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.units_of(self.selection()),
            reversed: self.head < self.anchor,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.clone().map(|m| self.units_of(m))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let typed: String = text.chars().filter(|c| !c.is_control()).collect();
        self.replace(self.target(range), &typed);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let at = self.target(range);
        // The first mark of a composition is where its undo starts.
        self.keep_undo();
        self.text.replace_range(at.clone(), new_text);
        self.marked = (!new_text.is_empty()).then(|| at.start..at.start + new_text.len());
        // The caret (or selection) the input method wants, counted inside the new text.
        let end = new_text.len();
        let (from, to) = new_selected.map_or((end, end), |units| {
            (byte_at(new_text, units.start), byte_at(new_text, units.end))
        });
        self.anchor = at.start + from;
        self.head = at.start + to;
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // The candidate window sits under the range's first line.
        let bytes = self.bytes_of(range);
        let (line, shape) = self.wrapped(window)?;
        let lh = shape.line_height;
        let scrolled = point(px(0.), lh * self.top);
        let start = line.position_for_index(bytes.start, lh)? - scrolled;
        let end = line
            .position_for_index(bytes.end, lh)
            .map(|p| p - scrolled)
            .filter(|p| p.y == start.y)
            .map_or(bounds.size.width, |p| p.x);
        Some(Bounds::from_corners(
            bounds.origin + start,
            bounds.origin + point(end, start.y + lh),
        ))
    }

    fn character_index_for_point(
        &mut self,
        at: Point<Pixels>,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let at = self.index_at(at, window)?;
        Some(units_at(&self.text, at))
    }
}

impl Focusable for Field {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Field {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::current(cx);
        // A text field to a screen reader: its name, its hint and what is typed in it. Its
        // edge shows where to type (3:1 on any ground), the focus ring when the caret is in it.
        Control::new(
            div()
                .id("field")
                .role(Role::TextInput)
                .aria_label(
                    self.label
                        .clone()
                        .unwrap_or_else(|| self.placeholder.clone()),
                )
                .aria_placeholder(self.placeholder.clone())
                .aria_value(self.text.clone())
                .key_context("Field")
                .on_key_down(cx.listener(Self::key))
                .on_mouse_down(MouseButton::Left, cx.listener(Self::click))
                .w_full()
                .px(px(9.))
                .py(px(5.))
                .rounded(px(8.))
                .border_1()
                .border_color(c(t.picked_edge()))
                .cursor_text()
                .focus(move |s| ui::focus_ring(s, &t))
                .child(FieldText(cx.entity())),
        )
        .track_focus(&self.focus)
    }
}

/// One run of `len` bytes in `font` and `color`, underlined or not.
fn plain_run(len: usize, font: Font, color: Hsla) -> TextRun {
    TextRun {
        len,
        font,
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

/// The text shaped and wrapped to a width: one wrapped line for each paragraph (a `\n` ends
/// one), stacked, so a position in the text is a point and a point is a position, across them.
struct Wrapped {
    lines: Vec<WrappedLine>,
}

impl Wrapped {
    /// Each paragraph with the byte it starts at and the shown line it starts on.
    fn paragraphs(&self) -> impl Iterator<Item = (usize, usize, &WrappedLine)> {
        let (mut start, mut row) = (0, 0);
        self.lines.iter().map(move |line| {
            let at = (start, row, line);
            start += line.len() + 1;
            row += rows_of(line);
            at
        })
    }

    /// How many shown lines the text takes.
    fn rows(&self) -> usize {
        self.lines.iter().map(rows_of).sum::<usize>().max(1)
    }

    /// Where byte `index` is drawn, from the text's top left.
    fn position_for_index(&self, index: usize, line_height: Pixels) -> Option<Point<Pixels>> {
        self.paragraphs()
            .find(|(start, _, line)| index <= start + line.len())
            .and_then(|(start, row, line)| {
                let at = line.position_for_index(index - start, line_height)?;
                Some(at + point(px(0.), line_height * row))
            })
    }

    /// The byte nearest `at`, from the text's top left: the last paragraph's end for a point
    /// below the text.
    fn index_at(&self, at: Point<Pixels>, line_height: Pixels) -> usize {
        let shown = (at.y / line_height).floor().max(0.) as usize;
        let mut lines = self.paragraphs().peekable();
        while let Some((start, row, line)) = lines.next() {
            if shown < row + rows_of(line) || lines.peek().is_none() {
                let last = line_height * (rows_of(line) - 1) + line_height * 0.5;
                let local = point(
                    at.x.max(px(0.)),
                    (at.y - line_height * row).max(px(0.)).min(last),
                );
                let i = line
                    .closest_index_for_position(local, line_height)
                    .unwrap_or_else(|i| i);
                return start + i.min(line.len());
            }
        }
        0
    }

    fn paint(&self, origin: Point<Pixels>, line_height: Pixels, window: &mut Window, cx: &mut App) {
        for (_, row, line) in self.paragraphs() {
            let at = origin + point(px(0.), line_height * row);
            let _ = line.paint(at, line_height, TextAlign::Left, None, window, cx);
        }
    }
}

/// `text` shaped and wrapped to `width`.
fn wrap(
    window: &Window,
    text: SharedString,
    font_size: Pixels,
    runs: &[TextRun],
    width: Pixels,
) -> Wrapped {
    let lines = window
        .text_system()
        .shape_text(text, font_size, runs, Some(width), None)
        .map(|lines| lines.into_vec())
        .unwrap_or_default();
    Wrapped { lines }
}

/// How many lines `line` wraps to.
fn rows_of(line: &WrappedLine) -> usize {
    line.wrap_boundaries().len() + 1
}

/// The first line to show so the caret's line is in view, moving `top` no more than it must.
fn top_for(top: usize, caret_row: usize, rows: usize) -> usize {
    let top = top
        .min(caret_row)
        .max((caret_row + 1).saturating_sub(MAX_LINES));
    top.min(rows.saturating_sub(MAX_LINES))
}

/// The painted text: wrapped, with the selection, the caret, the marked text underlined.
struct FieldText(Entity<Field>);

impl IntoElement for FieldText {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

struct Laid {
    line: Wrapped,
    shape: Shape,
    /// Where the first shown line is drawn: above the field's top when it is scrolled.
    origin: Point<Pixels>,
    top: usize,
    rows: usize,
    selection: Vec<Bounds<Pixels>>,
    caret: Option<Bounds<Pixels>>,
}

impl FieldText {
    /// What is drawn (the text, or the hint when there is none) and its ink.
    fn text(&self, cx: &App) -> (SharedString, Hsla, bool) {
        let inks = Inks::now(cx);
        let field = self.0.read(cx);
        if field.text.is_empty() {
            (field.placeholder.clone(), inks.placeholder, true)
        } else {
            (field.text.clone().into(), inks.text, false)
        }
    }
}

impl Element for FieldText {
    type RequestLayoutState = ();
    type PrepaintState = Laid;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        // As wide as the field, as high as the lines the text wraps to there, up to the cap.
        let (text, _, _) = self.text(cx);
        let style = window.text_style();
        let font = style.font();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let mut layout = Style::default();
        layout.size.width = relative(1.).into();
        let id = window.request_measured_layout(layout, move |known, available, window, _| {
            let width = known.width.or(match available.width {
                AvailableSpace::Definite(width) => Some(width),
                _ => None,
            });
            let rows = width.map_or(1, |width| {
                let runs = [plain_run(text.len(), font.clone(), Hsla::default())];
                wrap(window, text.clone(), font_size, &runs, width).rows()
            });
            size(width.unwrap_or_default(), line_height * rows.min(MAX_LINES))
        });
        (id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Laid {
        let (text, color, empty) = self.text(cx);
        let field = self.0.read(cx);
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let lh = window.line_height();
        let run = |len: usize, underline: bool| TextRun {
            underline: underline.then_some(UnderlineStyle {
                thickness: px(1.),
                color: Some(color),
                wavy: false,
            }),
            ..plain_run(len, style.font(), color)
        };
        let runs: Vec<TextRun> = match field.marked.clone().filter(|_| !empty) {
            Some(m) => [
                run(m.start, false),
                run(m.len(), true),
                run(text.len() - m.end, false),
            ]
            .into_iter()
            .filter(|r| r.len > 0)
            .collect(),
            None => vec![run(text.len(), false)],
        };
        let line = wrap(window, text, font_size, &runs, bounds.size.width);
        let rows = line.rows();
        let at = |i: usize| {
            if empty {
                Point::default()
            } else {
                line.position_for_index(i, lh).unwrap_or_default()
            }
        };
        let row = |p: Point<Pixels>| (p.y / lh).round() as usize;
        let top = top_for(field.top, row(at(field.head)), rows);
        let origin = bounds.origin - point(px(0.), lh * top);
        let sel = field.selection();
        // A selection over wrapped lines: its first line from where it starts, whole lines
        // between, its last line to where it ends.
        let mut selection = Vec::new();
        if !sel.is_empty() {
            let (a, b) = (at(sel.start), at(sel.end));
            let right = bounds.size.width;
            let mut y = a.y;
            while y <= b.y {
                let from = if y == a.y { a.x } else { px(0.) };
                let to = if y == b.y { b.x } else { right };
                selection.push(Bounds::from_corners(
                    origin + point(from, y),
                    origin + point(to, y + lh),
                ));
                y += lh;
            }
        }
        let caret = (sel.is_empty() && field.focus.is_focused(window))
            .then(|| Bounds::new(origin + at(field.head), size(px(1.5), lh)));
        Laid {
            line,
            shape: Shape {
                font: style.font(),
                font_size,
                line_height: lh,
                bounds,
            },
            origin,
            top,
            rows,
            selection,
            caret,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        laid: &mut Laid,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (focus, colors) = {
            let field = self.0.read(cx);
            (field.focus.clone(), Inks::now(cx))
        };
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.0.clone()), cx);
        let lh = laid.shape.line_height;
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for sel in &laid.selection {
                window.paint_quad(fill(*sel, colors.selection));
            }
            laid.line.paint(laid.origin, lh, window, cx);
            if let Some(caret) = laid.caret {
                window.paint_quad(fill(caret, colors.caret));
            }
        });
        let (shape, top, rows) = (laid.shape.clone(), laid.top, laid.rows);
        self.0.update(cx, |field, _| {
            field.shape = Some(shape);
            field.top = top;
            field.rows = rows;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{byte_at, top_for, units_at};

    /// The shown lines follow the caret and move no more than they must.
    #[test]
    fn the_shown_lines_follow_the_caret() {
        assert_eq!(top_for(0, 3, 4), 0, "fits: never scrolled");
        assert_eq!(top_for(0, 9, 10), 4, "the caret on the last line shown");
        assert_eq!(top_for(4, 6, 10), 4, "still in view: unmoved");
        assert_eq!(top_for(4, 2, 10), 2, "above: its line first");
        assert_eq!(
            top_for(4, 3, 5),
            0,
            "text got shorter: no empty lines below"
        );
    }

    /// `a`, a two-byte letter, a four-byte emoji (two UTF-16 units), `b`.
    const TEXT: &str = "a\u{e9}\u{1f600}b";

    #[test]
    fn input_method_offsets_land_on_character_starts() {
        let bytes: Vec<usize> = (0..7).map(|u| byte_at(TEXT, u)).collect();
        assert_eq!(bytes, [0, 1, 3, 3, 7, 8, 8], "half an emoji is its start");
        let units: Vec<usize> = [0, 1, 3, 7, 8, 99].map(|b| units_at(TEXT, b)).to_vec();
        assert_eq!(units, [0, 1, 2, 4, 5, 5]);
    }
}
