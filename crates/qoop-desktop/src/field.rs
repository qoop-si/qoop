//! The one-line text field: typing through the input method (dead keys, IME, the marked text
//! drawn underlined until it commits), a caret, a selection, copy, cut, paste, and undo of the
//! last edit. No multi-line editing anywhere in the app.
//!
//! Enter and Escape are left to the field's owner, which listens for them around it.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, Element, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, Focusable, GlobalElementId, Hsla, InspectorElementId,
    IntoElement, KeyDownEvent, LayoutId, MouseButton, MouseDownEvent, Pixels, Point, Render, Role,
    ShapedLine, SharedString, Style, TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window,
    div, fill, point, prelude::*, px, relative, size,
};

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
    pub focus: FocusHandle,
    /// Painted geometry, for the input method's candidate window and for clicks.
    line: Option<ShapedLine>,
    bounds: Option<Bounds<Pixels>>,
    pub colors: FieldColors,
}

#[derive(Clone, Copy)]
pub struct FieldColors {
    pub text: Hsla,
    pub placeholder: Hsla,
    pub caret: Hsla,
    pub selection: Hsla,
}

impl Field {
    pub fn new(placeholder: &str, colors: FieldColors, cx: &mut Context<Self>) -> Self {
        Field {
            text: String::new(),
            anchor: 0,
            head: 0,
            marked: None,
            undo: None,
            placeholder: placeholder.to_string().into(),
            focus: cx.focus_handle(),
            line: None,
            bounds: None,
            colors,
        }
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

    fn move_to(&mut self, at: usize, extend: bool) {
        self.head = at;
        if !extend {
            self.anchor = at;
        }
    }

    fn key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let k = &event.keystroke;
        let m = &k.modifiers;
        let sel = self.selection();
        let handled = match (k.key.as_str(), m.platform, m.shift) {
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
                    // One line: a pasted line end would be a second line nobody sees.
                    let one: String = text
                        .chars()
                        .map(|c| if c.is_control() { ' ' } else { c })
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
        if let (Some(line), Some(bounds)) = (&self.line, self.bounds) {
            let at = line.closest_index_for_x(event.position.x - bounds.left());
            self.move_to(at.min(self.text.len()), event.modifiers.shift);
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
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let bytes = self.bytes_of(range);
        let line = self.line.as_ref()?;
        Some(Bounds::from_corners(
            point(bounds.left() + line.x_for_index(bytes.start), bounds.top()),
            point(bounds.left() + line.x_for_index(bytes.end), bounds.bottom()),
        ))
    }

    fn character_index_for_point(
        &mut self,
        at: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let (line, bounds) = (self.line.as_ref()?, self.bounds?);
        let at = line.closest_index_for_x(at.x - bounds.left());
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
        // A text field to a screen reader: its name, its hint and what is typed in it.
        div()
            .id("field")
            .role(Role::TextInput)
            .aria_label(self.placeholder.clone())
            .aria_placeholder(self.placeholder.clone())
            .aria_value(self.text.clone())
            .track_focus(&self.focus)
            .key_context("Field")
            .on_key_down(cx.listener(Self::key))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::click))
            .w_full()
            .h(px(22.))
            .child(FieldText(cx.entity()))
    }
}

/// The painted line: text, selection, caret, the marked text underlined.
struct FieldText(Entity<Field>);

impl IntoElement for FieldText {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

struct Laid {
    line: ShapedLine,
    selection: Option<Bounds<Pixels>>,
    caret: Option<Bounds<Pixels>>,
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
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
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
    ) -> Laid {
        let field = self.0.read(cx);
        let style = window.text_style();
        let empty = field.text.is_empty();
        let (text, color): (SharedString, Hsla) = if empty {
            (field.placeholder.clone(), field.colors.placeholder)
        } else {
            (field.text.clone().into(), field.colors.text)
        };
        let run = |len: usize, underline: bool| TextRun {
            len,
            font: style.font(),
            color,
            background_color: None,
            underline: underline.then_some(UnderlineStyle {
                thickness: px(1.),
                color: Some(color),
                wavy: false,
            }),
            strikethrough: None,
        };
        let runs = match field.marked.clone().filter(|_| !empty) {
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
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(text, font_size, &runs, None);
        let sel = field.selection();
        let x = |i: usize| bounds.left() + if empty { px(0.) } else { line.x_for_index(i) };
        let selection = (!sel.is_empty()).then(|| {
            Bounds::from_corners(
                point(x(sel.start), bounds.top()),
                point(x(sel.end), bounds.bottom()),
            )
        });
        let caret = (sel.is_empty() && field.focus.is_focused(window)).then(|| {
            Bounds::new(
                point(x(field.head), bounds.top()),
                size(px(1.5), bounds.size.height),
            )
        });
        Laid {
            line,
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
            (field.focus.clone(), field.colors)
        };
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.0.clone()), cx);
        if let Some(sel) = laid.selection {
            window.paint_quad(fill(sel, colors.selection));
        }
        let _ = laid.line.paint(
            bounds.origin,
            window.line_height(),
            TextAlign::Left,
            None,
            window,
            cx,
        );
        if let Some(caret) = laid.caret {
            window.paint_quad(fill(caret, colors.caret));
        }
        let line = laid.line.clone();
        self.0.update(cx, |field, _| {
            field.line = Some(line);
            field.bounds = Some(bounds);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{byte_at, units_at};

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
