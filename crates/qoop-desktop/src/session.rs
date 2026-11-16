//! The session page: the space's tabs, the tab's panes as the daemon keeps them (read only),
//! the task strip, the corner note for another agent that turned Needs you, and d-review.
//!
//! The terminal keeps the keys: the strip, the note and the review are driven by clicks and
//! by the prefix (`ctrl-a x` dismisses the note, `ctrl-a tab` opens its agent); inside the
//! review, which takes the focus while open, `x` marks the finish read and `n` opens the next
//! file.

use std::collections::BTreeSet;
use std::path::Path;
use std::rc::Rc;

use gpui::{
    AnyElement, ClipboardItem, Context, FontFeatures, FontWeight, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Role, ScrollDelta, ScrollEvent,
    Window, div, prelude::*, px, relative,
};
use qoop_runtime::control::{ControlRequest, Pane, SplitDir, TerminalStream};
use qoop_runtime::daemon::{ModesWire, SnapshotWire};

use crate::app::{Desk, Page, Term};
use crate::element::TerminalElement;
use crate::link::{self, Up};
use crate::review::{self, Mark, Review};
use crate::state::{self, Kind};
use crate::theme::{DARK, MONO_FONT, TERMINAL_BG, Theme, c, ca};
use crate::ui::{self, Icon};

/// The task column's width, and the review's when it takes that place.
const STRIP_W: f32 = 300.;
const REVIEW_W: f32 = 560.;

/// The most diff rows the open file draws; the rest are in the editor.
const DIFF_ROWS: usize = 600;

/// What the wheel moves per notch, in rows.
const WHEEL_ROWS: f32 = 3.;

impl Desk {
    /// Hold `focus` and the other panes of its tab (the daemon's tree, on a session page),
    /// keeping what each pane already had, showing the new ones and hiding the ones gone.
    pub(crate) fn set_panes(&mut self, focus: Option<TerminalStream>) {
        let leaves = match (focus, self.page) {
            (Some(t), Page::Session(_)) => self.model.tree_of(t).leaves(),
            (Some(t), _) => vec![t],
            (None, _) => Vec::new(),
        };
        let mut old: Vec<Term> = self.term.take().into_iter().collect();
        old.append(&mut self.others);
        let before: BTreeSet<TerminalStream> = old.iter().map(|t| t.terminal).collect();
        let mut take = |t: TerminalStream| match old.iter().position(|o| o.terminal == t) {
            Some(i) => old.swap_remove(i),
            None => Term::new(t),
        };
        self.term = focus.map(&mut take);
        self.others = leaves
            .iter()
            .filter(|&&t| Some(t) != focus)
            .map(|&t| take(t))
            .collect();
        let gone: Vec<TerminalStream> = old.iter().map(|t| t.terminal).collect();
        let new: Vec<TerminalStream> = leaves
            .iter()
            .copied()
            .chain(focus)
            .filter(|t| !before.contains(t))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if !gone.is_empty() {
            let _ = self.up.send(Up::Hide(gone));
        }
        if !new.is_empty() {
            let _ = self.up.send(Up::Show(new));
        }
    }

    /// After a new agent list: the corner note for another agent that turned Needs you, gone
    /// once its agent no longer needs the owner; the panes follow a split made or undone.
    pub(crate) fn rows_changed(&mut self, before: &[qoop_runtime::control::AgentRow]) {
        let Page::Session(_) = self.page else {
            return;
        };
        let shown = self.term.as_ref().map(|t| t.terminal);
        let on_screen: Vec<TerminalStream> = self
            .term
            .iter()
            .chain(&self.others)
            .map(|t| t.terminal)
            .collect();
        if let Some(t) = self.model.turned_needs(before, &on_screen) {
            self.note = Some(t);
        }
        let needs = |t: TerminalStream| self.model.row(t).is_some_and(|r| Kind::of(r).needs_you());
        if self.note.is_some_and(|t| !needs(t)) {
            self.note = None;
        }
        if let Some(focus) = shown {
            let held: Vec<TerminalStream> = self
                .term
                .iter()
                .chain(&self.others)
                .map(|t| t.terminal)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let mut wanted = self.model.tree_of(focus).leaves();
            wanted.sort_unstable();
            if wanted != held {
                self.set_panes(Some(focus));
            }
        }
    }

    /// Read the open task of the tab on screen, by the tab's number (routed to its machine).
    pub(crate) fn ask_strip(&mut self) {
        let Page::Session(terminal) = self.page else {
            return;
        };
        let id = link::fresh_id("task");
        self.strip_ask = Some(id.clone());
        self.ask(ControlRequest::TaskRead {
            id,
            board: false,
            task: None,
            terminal: Some(terminal),
            token: None,
            before: None,
        });
    }

    fn pane_mut(&mut self, terminal: TerminalStream) -> Option<&mut Term> {
        self.term
            .iter_mut()
            .chain(self.others.iter_mut())
            .find(|t| t.terminal == terminal)
    }

    /// A page of scrollback came: shown in place of the live screen while scrolled back.
    pub(crate) fn scrolled_back(
        &mut self,
        terminal: TerminalStream,
        total: u64,
        lines: Vec<String>,
    ) {
        let Some(term) = self.pane_mut(terminal).filter(|t| t.scroll > 0) else {
            return;
        };
        term.total = total;
        let (cols, rows) = term.sent.get();
        let most = (total as usize).saturating_sub(usize::from(rows));
        term.scroll = term.scroll.min(most.max(1));
        term.back = Some(Rc::new(page_of(lines, cols, rows)));
    }

    /// The wheel over a pane: `rows` up (positive) or down, through `Scrollback`. A full screen
    /// program has no scrollback of its own here.
    fn wheel(&mut self, terminal: TerminalStream, rows: i64) {
        let Some(term) = self.pane_mut(terminal) else {
            return;
        };
        if term.screen.as_ref().is_some_and(|s| s.modes.alt_screen) {
            return;
        }
        let height = usize::from(term.sent.get().1.max(1));
        let most = match term.total {
            0 => i64::MAX,
            total => (total as usize).saturating_sub(height) as i64,
        };
        let scroll = (term.scroll as i64 + rows).clamp(0, most.max(0)) as usize;
        if scroll == term.scroll {
            return;
        }
        term.scroll = scroll;
        term.selection = None;
        if scroll == 0 {
            term.back = None;
            return;
        }
        self.ask(ControlRequest::Scrollback {
            id: link::fresh_id("back"),
            terminal,
            up: scroll + height,
            rows: height,
        });
    }

    /// Back to the live screen, as typing does.
    pub(crate) fn back_to_live(&mut self) {
        if let Some(term) = self.term.as_mut() {
            term.scroll = 0;
            term.back = None;
            term.selection = None;
        }
    }

    fn cell_at(&self, term: &Term, at: Point<Pixels>) -> (usize, usize) {
        let area = term.area.get();
        let col = ((at.x - area.origin.x) / self.cells.width).floor().max(0.);
        let row = ((at.y - area.origin.y) / self.cells.height).floor().max(0.);
        (row as usize, col as usize)
    }

    fn pane_down(
        &mut self,
        terminal: TerminalStream,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.term.as_ref().map(|t| t.terminal) != Some(terminal) {
            self.open(Page::Session(terminal), window, cx);
            return;
        }
        window.focus(&self.term_focus, cx);
        let cell = self
            .term
            .as_ref()
            .map(|term| self.cell_at(term, event.position));
        if let (Some(term), Some(cell)) = (self.term.as_mut(), cell) {
            term.selection = Some((cell, cell));
        }
        cx.notify();
    }

    fn pane_drag(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let Some(cell) = self
            .term
            .as_ref()
            .filter(|t| t.selection.is_some())
            .map(|term| self.cell_at(term, event.position))
        else {
            return;
        };
        if let Some((_, end)) = self.term.as_mut().and_then(|t| t.selection.as_mut())
            && *end != cell
        {
            *end = cell;
            cx.notify();
        }
    }

    fn pane_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(term) = self.term.as_mut()
            && term.selection.is_some_and(|(a, b)| a == b)
        {
            term.selection = None;
            cx.notify();
        }
    }

    /// Cmd+C: the selected cells of what the pane shows, as text.
    pub(crate) fn copy(&self, cx: &mut Context<Self>) -> bool {
        let Some(term) = self.term.as_ref() else {
            return false;
        };
        let (Some((a, b)), Some(shown)) = (term.selection, term.shown()) else {
            return false;
        };
        let (top, left) = term
            .viewport
            .get()
            .map_or((0, 0), |v| (usize::from(v.row), usize::from(v.col)));
        let text = state::selected_text(
            &shown.lines,
            (a.0 + top, a.1 + left),
            (b.0 + top, b.1 + left),
        );
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        true
    }

    /// Cmd+V: the clipboard's text as a paste; the daemon brackets it when the program asked.
    pub(crate) fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        if let Some(term) = self.term.as_ref().filter(|_| self.connected)
            && !text.is_empty()
        {
            let _ = self.up.send(Up::Paste(term.terminal, text.into_bytes()));
        }
        self.back_to_live();
    }

    /// The tab on screen runs on this machine: its checkout can be read and opened here.
    pub(crate) fn here(&self, terminal: TerminalStream) -> Option<String> {
        let row = self.model.row(terminal)?;
        (state::machine_of(row.workspace) == 0)
            .then(|| row.cwd.clone())
            .flatten()
    }

    /// Changes: the review of the tab on screen, read now in this process; again closes it.
    pub(crate) fn toggle_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.review.take().is_some() {
            window.focus(&self.term_focus, cx);
            cx.notify();
            return;
        }
        let Page::Session(terminal) = self.page else {
            return;
        };
        let Some(cwd) = self.here(terminal) else {
            return;
        };
        let id = link::fresh_id("changes");
        self.review = Some(Review {
            terminal,
            id: id.clone(),
            read: None,
            open: 0,
            said: None,
        });
        let _ = self.up.send(Up::Changes { id, terminal, cwd });
        window.focus(&self.review_focus, cx);
        cx.notify();
    }

    /// Mark read: the finish on screen counts as seen, for that stop only.
    fn mark_read(&mut self) {
        let Some(terminal) = self.review.as_ref().map(|r| r.terminal) else {
            return;
        };
        let Some(stop) = self.model.row(terminal).and_then(|r| r.stop.clone()) else {
            return;
        };
        self.ask(ControlRequest::Seen {
            id: link::fresh_id("seen"),
            terminal,
            stop: Some(stop),
        });
    }

    fn review_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &event.keystroke;
        if k.modifiers.control || k.modifiers.platform || k.modifiers.alt {
            return;
        }
        match crate::keys::typed(k) {
            "x" => self.mark_read(),
            "n" => {
                if let Some(review) = self.review.as_mut() {
                    review.next_file();
                }
            }
            "escape" => {
                self.review = None;
                window.focus(&self.term_focus, cx);
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// A file of the review in the person's editor, at `line` when given.
    fn open_file(&mut self, path: &str, line: Option<u32>) {
        let Some(review) = self.review.as_mut() else {
            return;
        };
        let Some(Ok(changes)) = &review.read else {
            return;
        };
        match review::editor_for(
            Path::new(&changes.root),
            path,
            line,
            qoop_runtime::changes::tool,
        ) {
            Ok((program, args)) => {
                review.said = None;
                review::launch(&program, &args);
            }
            Err(why) => review.said = Some(why),
        }
    }

    /// `ctrl-a x`: the corner note goes; the agent still waits in the Inbox.
    pub(crate) fn dismiss_note(&mut self, cx: &mut Context<Self>) {
        if self.note.take().is_some() {
            cx.notify();
        }
    }

    /// `ctrl-a tab`: the corner note's agent opens.
    pub(crate) fn open_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(t) = self.note.take() {
            self.open(Page::Session(t), window, cx);
        }
    }

    /// The centre of a session page.
    pub fn session(&self, t: &Theme, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        let Some(term) = self.term.as_ref() else {
            return div().into_any_element();
        };
        let tree = self.model.tree_of(term.terminal);
        // The agent on screen asks something: its card under its screen, the panel beside.
        let asking = self
            .model
            .row(term.terminal)
            .filter(|r| r.menu.is_some() && Kind::of(r).needs_you());
        let right = match (&self.review, asking) {
            (Some(review), _) => self.review_panel(review, t, cx).into_any_element(),
            (None, Some(row)) => self.question_side(row, t),
            (None, None) => self.task_column(t, cx).into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(self.tab_row(term.terminal, t, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .flex()
                            .flex_col()
                            .gap(px(10.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .child(self.layout(&tree, focused, cx)),
                            )
                            .when_some(asking, |d, row| d.child(self.question_card(row, t, cx))),
                    )
                    .child(
                        div()
                            .flex_none()
                            .h_full()
                            .w(px(if self.review.is_some() {
                                REVIEW_W
                            } else {
                                STRIP_W
                            }))
                            .flex()
                            .flex_col()
                            .gap(px(10.))
                            .child(div().flex_1().min_h_0().flex().flex_col().child(right))
                            .when_some(self.note, |d, note| d.child(self.corner_note(note, t, cx))),
                    ),
            )
            .into_any_element()
    }

    /// The space's tabs, then Changes and Open in editor for a tab on this machine.
    fn tab_row(
        &self,
        shown: TerminalStream,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let row = self.model.row(shown);
        let space = row.and_then(|r| self.model.space(r.workspace));
        let tabs = row.map_or_else(Vec::new, |r| self.model.tabs_of(r.workspace));
        let tree = self.model.tree_of(shown).leaves();
        let here = self.here(shown);
        let totals = self.review.as_ref().and_then(|r| match &r.read {
            Some(Ok(changes)) => Some(changes.totals()),
            _ => None,
        });
        div()
            .flex_none()
            .h(px(36.))
            .flex()
            .items_center()
            .gap(px(4.))
            .when_some(space, |d, w| {
                d.child(
                    div()
                        .px(px(8.))
                        .text_size(px(12.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.space(state::space_slot(w)))
                        .whitespace_nowrap()
                        .child(w.name.clone()),
                )
            })
            .child(
                div()
                    .id("tabs")
                    .role(Role::TabList)
                    .aria_label("Tabs")
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .children(tabs.into_iter().map(|tab| {
                        let terminal = tab.terminal;
                        let on = tree.contains(&terminal);
                        let title = state::title_of(tab);
                        ui::control(("tab", terminal as u64), Role::Tab, title.clone(), t)
                            .aria_selected(on)
                            .flex()
                            .items_center()
                            .gap(px(7.))
                            .h(px(30.))
                            .max_w(px(220.))
                            .px(px(10.))
                            .rounded(px(9.))
                            .text_size(px(12.5))
                            .text_color(c(if on { t.ink } else { t.t1 }))
                            .when(on, |d| d.bg(ca(t.fill)))
                            .hover(|s| s.bg(ca(t.fill)))
                            .child(ui::mark(Kind::of(tab), t, 10.))
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(title),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open(Page::Session(terminal), window, cx)
                            }))
                    })),
            )
            .child(div().flex_1())
            .when_some(space.filter(|w| state::machine_of(w.id) != 0), |d, w| {
                d.child(quiet(
                    &format!("on {}: open it there", state::machine_name(w)),
                    t,
                ))
            })
            .when(here.is_some(), |d| {
                d.child(
                    ui::control(
                        "changes",
                        Role::Button,
                        match totals {
                            Some((a, r)) => format!("Changes, {a} added, {r} removed"),
                            None => "Changes".to_string(),
                        },
                        t,
                    )
                    .aria_expanded(self.review.is_some())
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h(px(30.))
                    .px(px(10.))
                    .rounded(px(9.))
                    .text_size(px(12.5))
                    .text_color(c(t.t1))
                    .when(self.review.is_some(), |d| d.bg(ca(t.fill2)))
                    .hover(|s| s.bg(ca(t.fill)))
                    .child("Changes")
                    .when_some(totals, |d, (a, r)| {
                        d.child(div().text_color(c(t.done_text)).child(format!("+{a}")))
                            .child(div().text_color(c(t.fail_text)).child(format!("-{r}")))
                    })
                    .child(ui::kbd(&format!("{} D", self.prefix_name()), t))
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_review(window, cx))),
                )
                .child(
                    ui::control("open-editor", Role::Button, "Open in editor", t)
                        .flex()
                        .items_center()
                        .h(px(30.))
                        .px(px(10.))
                        .rounded(px(9.))
                        .text_size(px(12.5))
                        .text_color(c(t.t1))
                        .hover(|s| s.bg(ca(t.fill)))
                        .child("Open in editor")
                        .on_click(cx.listener(move |this, _, _, _| {
                            if let Some(cwd) = this.here(shown) {
                                review::open_folder(cwd);
                            }
                        })),
                )
            })
    }

    /// The tab's panes, laid out as the daemon's tree has them.
    fn layout(&self, tree: &Pane, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        match tree {
            Pane::Leaf { terminal } => self.pane(*terminal, focused, cx),
            Pane::Split {
                dir,
                permille,
                first,
                second,
                ..
            } => {
                let share = (f32::from(*permille) / 1000.).clamp(0.05, 0.95);
                let across = *dir == SplitDir::V;
                div()
                    .size_full()
                    .flex()
                    .when(across, |d| d.flex_row())
                    .when(!across, |d| d.flex_col())
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_none()
                            .min_w_0()
                            .min_h_0()
                            .when(across, |d| d.w(relative(share)).h_full())
                            .when(!across, |d| d.h(relative(share)).w_full())
                            .child(self.layout(first, focused, cx)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .child(self.layout(second, focused, cx)),
                    )
                    .into_any_element()
            }
        }
    }

    /// One pane: its header (state, agent, mode, tab number) over its terminal.
    fn pane(&self, terminal: TerminalStream, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        let Some((index, term)) = self
            .term
            .iter()
            .chain(&self.others)
            .enumerate()
            .find(|(_, t)| t.terminal == terminal)
        else {
            return div().into_any_element();
        };
        // The terminal ground is dark in both themes, and so is what is drawn on it.
        let d = DARK;
        let is_focus = index == 0;
        let row = self.model.row(terminal);
        let kind = row.map(Kind::of);
        let mode = row
            .filter(|r| r.program.as_deref() == Some("claude"))
            .and(term.screen.as_ref())
            .and_then(|s| qoop_runtime::menu::mode_of(&s.lines));
        let up = self.up.clone();
        let label = row.map_or_else(|| format!("tab {terminal}"), state::title_of);
        div()
            .size_full()
            .flex()
            .flex_col()
            .rounded(px(12.))
            .overflow_hidden()
            .bg(c(TERMINAL_BG))
            .border_1()
            .border_color(ca(if is_focus && !self.others.is_empty() {
                d.line
            } else {
                d.hair
            }))
            .child(
                div()
                    .flex_none()
                    .h(px(30.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .text_size(px(12.))
                    .text_color(c(d.t2))
                    .when_some(kind, |h, kind| {
                        h.child(ui::mark(kind, &d, 10.)).child(
                            div()
                                .text_color(c(ui::word_ink(kind, &d)))
                                .child(kind.word()),
                        )
                    })
                    .when_some(row.and_then(|r| state::age_of(r, self.now)), |h, age| {
                        h.child(age)
                    })
                    .when_some(row.and_then(|r| r.program.clone()), |h, program| {
                        h.child(div().text_color(c(d.t1)).child(program))
                    })
                    .when_some(mode, |h, mode| {
                        h.child(
                            div()
                                .id(("mode", terminal as u64))
                                .role(Role::Status)
                                .aria_label(format!("Mode: {mode}"))
                                .px(px(7.))
                                .rounded_full()
                                .border_1()
                                .border_color(ca(d.line))
                                .text_size(px(11.))
                                .text_color(c(d.t1))
                                .child(mode),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        div()
                            .font_family(MONO_FONT)
                            .text_size(px(11.))
                            .child(format!("tab {}", qoop_tui::mux::daemon_terminal(terminal))),
                    ),
            )
            .child(
                div()
                    .id(("pane", terminal as u64))
                    .role(Role::Terminal)
                    .aria_label(format!("Terminal, {label}"))
                    .key_context("Terminal")
                    .when(is_focus, |d| {
                        d.track_focus(&self.term_focus)
                            .on_key_down(cx.listener(Self::term_key))
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event, window, cx| {
                            this.pane_down(terminal, event, window, cx)
                        }),
                    )
                    .on_mouse_move(cx.listener(Self::pane_drag))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::pane_up))
                    .on_scroll(cx.listener(move |this, event: &ScrollEvent, _, cx| {
                        let height = this.cells.height;
                        let Some(term) = this.pane_mut(terminal) else {
                            return;
                        };
                        let rows = wheel_rows(event.delta, height, &mut term.wheel_left);
                        if rows != 0 {
                            this.wheel(terminal, rows);
                            cx.notify();
                        }
                    }))
                    .flex_1()
                    .min_h_0()
                    .p(px(8.))
                    .child(TerminalElement {
                        pane: index,
                        terminal: Some(terminal),
                        screen: term.shown(),
                        cells: self.cells.clone(),
                        focused: focused && is_focus,
                        stale: !self.connected,
                        sent: term.sent.clone(),
                        viewport: term.viewport.clone(),
                        resize: Rc::new(move |t, cols, rows| {
                            let _ = up.send(Up::Resize(t, cols, rows));
                        }),
                        input: is_focus.then(|| (self.term_focus.clone(), cx.entity())),
                        selection: term.selection,
                        area: Some(term.area.clone()),
                    }),
            )
            .into_any_element()
    }

    /// The task strip: the tab's open task, its plan steps with their times (or the vertical
    /// timeline), and the evidence as recorded.
    fn task_column(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let row = self
            .term
            .as_ref()
            .and_then(|term| self.model.row(term.terminal));
        let needs = row.is_some_and(|r| Kind::of(r).needs_you());
        let body = div().flex().flex_col().gap(px(10.));
        let body = match &self.strip {
            None => body.child(quiet("Reading the task.", t)),
            Some(strip) => match strip.heading() {
                None => body.child(quiet("No task for this terminal.", t)),
                Some((title, progress)) => {
                    let goal = strip
                        .task
                        .as_ref()
                        .map(|k| k.goal.clone())
                        .unwrap_or_default();
                    let current = strip.current();
                    let evidence = strip.evidence();
                    body.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_size(px(13.5))
                                    .child(title),
                            )
                            .child(div().text_size(px(12.)).text_color(c(t.t2)).child(progress))
                            .child(div().flex_1())
                            .when(!strip.steps.is_empty(), |d| {
                                d.child(
                                    ui::link_button("timeline", "Timeline", t)
                                        .aria_toggled(ui::toggled(self.timeline))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.timeline = !this.timeline;
                                            cx.notify();
                                        })),
                                )
                            }),
                    )
                    .child(div().text_size(px(13.)).text_color(c(t.t1)).child(goal))
                    .when(!strip.steps.is_empty(), |d| {
                        d.child(section(
                            "Plan",
                            Some(format!("{} of {}", strip.done(), strip.steps.len())),
                            t,
                        ))
                        .children(strip.steps.iter().enumerate().map(|(i, step)| {
                            let now = current == Some(i);
                            let time = match step.done_at {
                                Some(at) => state::clock(at, self.zone),
                                None if now => "now".to_string(),
                                None => String::new(),
                            };
                            step_row(
                                &step.text,
                                time,
                                step.done_at.is_some(),
                                now && needs,
                                self.timeline,
                                t,
                            )
                        }))
                    })
                    .when(!evidence.is_empty(), |d| {
                        d.child(section("Evidence", None, t))
                            .children(evidence.into_iter().map(|e| {
                                let time = state::clock(e.at, self.zone);
                                div()
                                    .flex()
                                    .gap(px(8.))
                                    .text_size(px(12.5))
                                    .child(
                                        div()
                                            .mt(px(5.))
                                            .size(px(7.))
                                            .flex_none()
                                            .rounded_full()
                                            .bg(c(if e.failed { t.fail } else { t.done })),
                                    )
                                    .child(div().flex_1().min_w_0().text_color(c(t.t1)).child(
                                        format!(
                                            "{}{}",
                                            if e.failed { "failed: " } else { "" },
                                            e.text
                                        ),
                                    ))
                                    .child(div().text_color(c(t.t3)).child(time))
                            }))
                    })
                }
            },
        };
        div()
            .id("task-strip")
            .role(Role::Complementary)
            .aria_label("Task")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p(px(14.))
            .rounded(px(14.))
            .bg(c(t.card))
            .border_1()
            .border_color(ca(t.hair))
            .child(body)
    }

    /// The corner note: another agent turned Needs you while this session is open. Shown once;
    /// it then waits in the Inbox.
    fn corner_note(
        &self,
        terminal: TerminalStream,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(row) = self.model.row(terminal) else {
            return div().into_any_element();
        };
        let title = state::title_of(row);
        let space = self.model.space(row.workspace);
        let menu = row.menu.as_ref();
        let prefix = self.prefix_name();
        div()
            .id("corner-note")
            .debug_selector(|| "corner-note".to_string())
            .role(Role::Alert)
            .aria_label(format!("Needs you: {title}"))
            .flex_none()
            .p(px(12.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .rounded(px(14.))
            .bg(c(t.card2))
            .border_1()
            .border_color(ca(t.needs_line))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .text_size(px(12.5))
                    .child(ui::mark(Kind::of(row), t, 11.))
                    .child(div().text_color(c(t.needs_text)).child("Needs you"))
                    .when_some(state::age_of(row, self.now), |d, age| {
                        d.child(div().text_color(c(t.t2)).child(age))
                    })
                    .child(div().flex_1())
                    .child(
                        ui::control("note-close", Role::Button, "Dismiss", t)
                            .size(px(24.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.))
                            .child(ui::icon(Icon::Close, 13., c(t.t2)))
                            .on_click(cx.listener(|this, _, _, cx| this.dismiss_note(cx))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(13.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title.clone()),
                    )
                    .when_some(space, |d, w| {
                        d.child(
                            div()
                                .text_size(px(12.))
                                .text_color(t.space(state::space_slot(w)))
                                .child(w.name.clone()),
                        )
                    }),
            )
            .when_some(menu.filter(|m| !m.question.is_empty()), |d, m| {
                d.child(
                    div()
                        .text_size(px(13.))
                        .text_color(c(t.t1))
                        .child(m.question.clone()),
                )
            })
            .when_some(menu, |d, m| {
                d.child(
                    div()
                        .id("note-options")
                        .role(Role::Group)
                        .aria_label("Answers")
                        .flex()
                        .flex_col()
                        .gap(px(3.))
                        .children(
                            state::offers(m)
                                .iter()
                                .take(5)
                                .enumerate()
                                .map(|(n, o)| self.offer_button(terminal, o, ("note", n), t, cx)),
                        ),
                )
            })
            .when_some(
                self.said_line(&state::Slot::Agent(terminal), t, cx),
                |d, line| d.child(line),
            )
            .child(quiet(
                "Clicks only; the terminal keeps the keys. Shown once, then it waits in the Inbox.",
                t,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        ui::pill("note-dismiss", "Dismiss", t)
                            .h(px(28.))
                            .child("Dismiss")
                            .child(ui::kbd(&format!("{prefix} x"), t))
                            .on_click(cx.listener(|this, _, _, cx| this.dismiss_note(cx))),
                    )
                    .child(div().flex_1())
                    .child(
                        ui::pill("note-open", &format!("Open {title}"), t)
                            .h(px(28.))
                            .child("Open")
                            .child(ui::kbd(&format!("{prefix} tab"), t))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_note(window, cx)),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// d-review: the files the tab's checkout changed against its base, one open.
    fn review_panel(&self, review: &Review, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let ring = c(t.ink);

        let row = self.model.row(review.terminal);
        let agent = row
            .and_then(|r| r.program.clone())
            .unwrap_or_else(|| "the agent".into());
        let head = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(14.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(row.map_or_else(String::new, state::title_of)),
            )
            .child(ui::kbd("esc", t))
            .child(
                ui::control("review-close", Role::Button, "Close the review", t)
                    .size(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .child(ui::icon(Icon::Close, 14., c(t.t2)))
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_review(window, cx))),
            );
        let body = div().flex().flex_col().gap(px(8.));
        let body = match &review.read {
            None => body.child(quiet("Reading the changes.", t)),
            Some(Err(why)) => body.child(quiet(why, t)),
            Some(Ok(changes)) => {
                let (added, removed) = changes.totals();
                let steps = self
                    .strip
                    .as_ref()
                    .map(|s| s.steps.clone())
                    .unwrap_or_default();
                body.when(!steps.is_empty(), |d| {
                    let done = steps.iter().filter(|s| s.done_at.is_some()).count();
                    d.child(section(
                        "Timeline",
                        Some(format!("{done} of {}", steps.len())),
                        t,
                    ))
                    .children(steps.iter().map(|s| {
                        let time = s
                            .done_at
                            .map(|at| state::clock(at, self.zone))
                            .unwrap_or_default();
                        step_row(&s.text, time, s.done_at.is_some(), false, true, t)
                    }))
                })
                .child(section(
                    "Changes",
                    Some(format!(
                        "{} files, +{added} -{removed}, against {}",
                        changes.files.len(),
                        changes.base
                    )),
                    t,
                ))
                .when(changes.files.is_empty(), |d| {
                    d.child(quiet(&format!("No changes against {}.", changes.base), t))
                })
                .children(changes.files.iter().enumerate().map(|(i, file)| {
                    let open = review.open == i;
                    let path = file.path.clone();
                    let opened = path.clone();
                    let diff = changes.diff_of(&file.path);
                    div()
                        .flex()
                        .flex_col()
                        // The file's button and its Open in editor side by side: a link
                        // inside a button is flattened away for a screen reader.
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .child(
                                    ui::control(
                                        ("file", i as u64),
                                        Role::Button,
                                        file.path.clone(),
                                        t,
                                    )
                                    .aria_expanded(open)
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .min_h(px(30.))
                                    .px(px(6.))
                                    .rounded(px(8.))
                                    .hover(|s| s.bg(ca(t.fill)))
                                    .child(ui::icon(Icon::Chevron, 13., c(t.t2)))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .text_ellipsis()
                                            .font_family(MONO_FONT)
                                            .text_size(px(12.))
                                            .child(file.path.clone()),
                                    )
                                    .when_some(review::counts(file), |d, n| {
                                        d.child(
                                            div().text_size(px(12.)).text_color(c(t.t2)).child(n),
                                        )
                                    })
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            if let Some(r) = this.review.as_mut() {
                                                r.open = i;
                                            }
                                            cx.notify();
                                        },
                                    )),
                                )
                                .child(
                                    ui::link_button(("file-editor", i as u64), "Open in editor", t)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.open_file(&opened, None);
                                            cx.notify();
                                        })),
                                ),
                        )
                        .when(open, |d| {
                            d.child(self.file_body(file.status.as_str(), &path, diff, t, cx))
                        })
                }))
                .when(changes.cut, |d| {
                    d.child(quiet(
                        "The diff was cut short: the rest is in your editor.",
                        t,
                    ))
                })
            }
        };
        div()
            .id("review")
            .role(Role::Region)
            .aria_label("Changes")
            .focus_visible(move |s| s.border_color(ring))
            .track_focus(&self.review_focus)
            .on_key_down(cx.listener(Self::review_key))
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(10.))
            .p(px(14.))
            .rounded(px(14.))
            .bg(c(t.card))
            .border_1()
            .border_color(ca(t.hair))
            .child(head)
            .child(
                div()
                    .id("review-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(body),
            )
            .when_some(review.said.clone(), |d, said| d.child(quiet(&said, t)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(12.5))
                    .text_color(c(t.t2))
                    .child(
                        div()
                            .flex_1()
                            .child(format!("{agent} does git when you ask it.")),
                    )
                    .child(
                        ui::pill("mark-read", "Mark read", t)
                            .h(px(28.))
                            .child("Mark read")
                            .child(ui::kbd("x", t))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.mark_read();
                                cx.notify();
                            })),
                    )
                    .child(
                        ui::pill("next-file", "Next file", t)
                            .h(px(28.))
                            .child("Next file")
                            .child(ui::kbd("n", t))
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(r) = this.review.as_mut() {
                                    r.next_file();
                                }
                                cx.notify();
                            })),
                    ),
            )
    }

    /// The open file: its diff rows with their line numbers, a click opening the editor at a
    /// row; ligatures off, so code reads as typed.
    fn file_body(
        &self,
        status: &str,
        path: &str,
        diff: Option<&review::FileDiff>,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let note = match diff {
            _ if status == "?" => Some("New, not in the diff yet: open it in your editor."),
            Some(d) if d.binary => Some("Binary, open in your editor."),
            None => Some("No rows to show: open it in your editor."),
            Some(_) => None,
        };
        if let Some(note) = note {
            return div()
                .px(px(28.))
                .py(px(4.))
                .child(quiet(note, t))
                .into_any_element();
        }
        let lines = diff.map_or(&[][..], |d| d.lines.as_slice());
        let more = lines.len().saturating_sub(DIFF_ROWS);
        div()
            .flex()
            .flex_col()
            .rounded(px(8.))
            .overflow_hidden()
            .bg(c(TERMINAL_BG))
            .font_family(MONO_FONT)
            .font_features(FontFeatures::disable_ligatures())
            .text_size(px(12.))
            .text_color(c(DARK.t1))
            .children(lines.iter().take(DIFF_ROWS).enumerate().map(|(k, l)| {
                let file = path.to_string();
                let line = l.line;
                let ground = match l.mark {
                    Mark::Added => Some(ca((DARK.done << 8) | 0x26)),
                    Mark::Removed => Some(ca((DARK.fail << 8) | 0x26)),
                    _ => None,
                };
                let sign = match l.mark {
                    Mark::Added => "+",
                    Mark::Removed => "-",
                    _ => " ",
                };
                div()
                    .id(("diff-row", k as u64))
                    .flex()
                    .px(px(6.))
                    .cursor_pointer()
                    .when_some(ground, |d, g| d.bg(g))
                    .hover(|s| s.bg(ca(DARK.fill)))
                    .when(l.mark == Mark::Hunk, |d| d.text_color(c(DARK.t3)))
                    .child(div().w(px(40.)).flex_none().text_color(c(DARK.t3)).child(
                        if l.mark == Mark::Hunk {
                            String::new()
                        } else {
                            line.to_string()
                        },
                    ))
                    .child(div().w(px(14.)).flex_none().child(sign))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_nowrap()
                            .child(l.text.clone()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_file(&file, Some(line));
                        cx.notify();
                    }))
            }))
            .when(more > 0, |d| {
                d.child(
                    div()
                        .px(px(6.))
                        .py(px(4.))
                        .text_color(c(DARK.t2))
                        .child(format!("{more} more rows in your editor.")),
                )
            })
            .into_any_element()
    }
}

/// A quiet line of text.
fn quiet(text: &str, t: &Theme) -> impl IntoElement {
    div()
        .text_size(px(12.5))
        .text_color(c(t.t2))
        .child(text.to_string())
}

/// A section heading with its count on the right: "Plan  2 of 4".
fn section(title: &str, count: Option<String>, t: &Theme) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .pt(px(4.))
        .text_size(px(11.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(c(t.t2))
        .child(div().flex_1().child(title.to_string()))
        .when_some(count, |d, n| d.child(n))
}

/// A plan step: its done mark, its words (wrapping) and its time; "asks you" under the current
/// step of an agent that needs the owner. As a timeline row the time comes first.
fn step_row(
    text: &str,
    time: String,
    done: bool,
    asks: bool,
    timeline: bool,
    t: &Theme,
) -> impl IntoElement {
    let mark = div()
        .mt(px(3.))
        .size(px(11.))
        .flex_none()
        .rounded(px(if timeline { 6. } else { 3. }))
        .border_1()
        .border_color(c(if done { t.done } else { t.t3 }))
        .when(done, |d| d.bg(c(t.done)));
    let time = div()
        .flex_none()
        .font_family(MONO_FONT)
        .text_size(px(11.5))
        .text_color(c(t.t3))
        .child(time);
    let words = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .child(
            div()
                .text_color(c(if done { t.t2 } else { t.t1 }))
                .child(text.to_string()),
        )
        .when(asks, |d| {
            d.child(
                div()
                    .text_size(px(12.))
                    .text_color(c(t.needs_text))
                    .child("asks you"),
            )
        });
    let row = div().flex().gap(px(8.)).text_size(px(12.5));
    if timeline {
        row.child(time.w(px(40.))).child(mark).child(words)
    } else {
        row.child(mark).child(words).child(time)
    }
}

/// Whole rows to scroll for one wheel event: a notch (`Lines`) is `WHEEL_ROWS` rows, a
/// trackpad's pixels are counted by the row height. What does not make a whole row yet is
/// kept in `left` for the next event, so a slow swipe still scrolls.
fn wheel_rows(delta: ScrollDelta, row: Pixels, left: &mut f32) -> i64 {
    let rows = match delta {
        ScrollDelta::Lines(lines) => lines.y * WHEEL_ROWS,
        ScrollDelta::Pixels(pixels) => pixels.y / row,
    };
    let total = *left + rows;
    let whole = total.trunc();
    *left = total - whole;
    whole as i64
}

/// A page of scrollback rows as a screen: no styles, no cursor.
fn page_of(lines: Vec<String>, cols: u16, rows: u16) -> SnapshotWire {
    let modes = ModesWire {
        cursor_visible: false,
        ..ModesWire::default()
    };
    SnapshotWire {
        cols: usize::from(cols),
        rows: usize::from(rows),
        cursor: (0, 0),
        lines,
        styles: Vec::new(),
        links: Vec::new(),
        modes,
        title: String::new(),
        behind: 0,
    }
}

#[cfg(test)]
mod tests {
    use gpui::{point, px};

    use super::*;

    /// A notch of a mouse wheel scrolls three rows; a slow trackpad swipe scrolls once its
    /// pixels make a whole row, and both directions keep what is left over.
    #[test]
    fn every_wheel_event_counts() {
        let mut left = 0.;
        assert_eq!(
            wheel_rows(ScrollDelta::Lines(point(0., 1.)), px(16.), &mut left),
            3
        );
        assert_eq!(
            wheel_rows(ScrollDelta::Lines(point(0., -1.)), px(16.), &mut left),
            -3
        );
        let slow = ScrollDelta::Pixels(point(px(0.), px(6.)));
        let rows: Vec<i64> = (0..6)
            .map(|_| wheel_rows(slow, px(16.), &mut left))
            .collect();
        assert_eq!(rows, [0, 0, 1, 0, 0, 1], "6 px at a time of 16 px rows");
        assert_eq!(rows.iter().sum::<i64>(), 2, "36 px is two rows and a bit");
    }
}
