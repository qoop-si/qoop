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
use qoop_runtime::daemon::{MAX_SCROLLBACK_ROWS, ModesWire, SnapshotWire};

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

    /// Machines whose link just came up: the mux let go of their subscriptions when the link
    /// dropped (or never took one made while it was down), so their panes on screen are shown
    /// again, which brings a fresh screen, told their size again, and the strip is read again
    /// when the tab on screen is theirs. Right after this Mac's daemon reconnects the first
    /// list counts every up link as back, so a remote pane already shown gets a second Show
    /// and one extra full screen: harmless, both ends keep sets.
    pub(crate) fn machines_back(&mut self, back: &[usize]) {
        let theirs = |t: TerminalStream| back.contains(&qoop_tui::mux::machine_of_terminal(t));
        let mut again = Vec::new();
        for term in self.term.iter().chain(&self.others) {
            if theirs(term.terminal) {
                term.sent.set((0, 0));
                again.push(term.terminal);
            }
        }
        if again.is_empty() {
            return;
        }
        let _ = self.up.send(Up::Show(again));
        if self.term.as_ref().is_some_and(|t| theirs(t.terminal)) {
            self.ask_strip();
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
        // No note while the Inbox is open: its card already shows the question.
        if !self.inbox
            && let Some(t) = self.model.turned_needs(before, &on_screen)
        {
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

    pub(crate) fn panes_mut(&mut self) -> impl Iterator<Item = &mut Term> {
        self.term.iter_mut().chain(self.others.iter_mut())
    }

    fn pane_mut(&mut self, terminal: TerminalStream) -> Option<&mut Term> {
        self.panes_mut().find(|t| t.terminal == terminal)
    }

    /// Rows of scrollback came: held by the pane that asked, unless a newer ask replaced it.
    pub(crate) fn scrolled_back(
        &mut self,
        terminal: TerminalStream,
        id: &str,
        from: u64,
        total: u64,
        lines: Vec<String>,
    ) {
        let next = self
            .pane_mut(terminal)
            .and_then(|t| t.came(id, from, total, lines));
        if let Some(next) = next {
            self.ask(next);
        }
    }

    /// The wheel over a pane: `rows` up (positive) or down. A full screen program has no
    /// scrollback of its own here.
    fn wheel(&mut self, terminal: TerminalStream, rows: i64) {
        let Some(term) = self.pane_mut(terminal) else {
            return;
        };
        if term.screen.as_ref().is_some_and(|s| s.modes.alt_screen) {
            return;
        }
        if let Some(ask) = term.wheel(rows) {
            self.ask(ask);
        }
    }

    /// Back to the live screen, as typing does.
    pub(crate) fn back_to_live(&mut self) {
        if let Some(term) = self.term.as_mut() {
            term.live();
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
            (None, Some(row)) => div()
                .size_full()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .child(self.question_side(row, t)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .child(self.task_column(t, cx)),
                )
                .into_any_element(),
            (None, None) => match self.questions(term.terminal, t, cx) {
                // The agent's question cards over its task.
                Some(questions) => div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(questions)
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .child(self.task_column(t, cx)),
                    )
                    .into_any_element(),
                None => self.task_column(t, cx).into_any_element(),
            },
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
                            .when_some(asking, |d, row| d.child(self.question_card(row, t, cx)))
                            .when_some(
                                self.closed_card
                                    .as_ref()
                                    .filter(|(on, _, _)| *on == term.terminal && asking.is_none()),
                                |d, (_, menu, line)| {
                                    d.child(self.closed_card_view(menu, line, t, cx))
                                },
                            ),
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
                            .text_color(c(t.t1))
                            .when(on, |d| ui::current(d, t))
                            .hover(|s| ui::paint(s, t.hover()))
                            .child(ui::mark(self.shown_kind(state::Entry::Live(tab)), t, 10.))
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
                    .when(self.review.is_some(), |d| ui::current(d, t))
                    .hover(|s| ui::paint(s, t.hover()))
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
                        .hover(|s| ui::paint(s, t.hover()))
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
        // Neutral, with its time, while its machine is not reached: as the card under it.
        let shown = row.map(|r| (self.shown_kind(state::Entry::Live(r)), self.shown_word(r)));
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
                    .when_some(shown, |h, (kind, word)| {
                        h.child(ui::mark(kind, &d, 10.))
                            .child(div().text_color(c(ui::word_ink(kind, &d))).child(word))
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
                        // Scrolled back, part of a row moves the page too.
                        if rows != 0 || term.scroll > 0 {
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
                        stale: self.term_stale(terminal),
                        sent: term.sent.clone(),
                        viewport: term.viewport.clone(),
                        resize: Rc::new(move |t, cols, rows| {
                            let _ = up.send(Up::Resize(t, cols, rows));
                        }),
                        input: is_focus.then(|| (self.term_focus.clone(), cx.entity())),
                        selection: term.selection,
                        area: Some(term.area.clone()),
                        lift: self.cells.height * term.lift,
                    }),
            )
            .into_any_element()
    }

    /// The agent's open question cards over its task: each folded to a row, one opened in place
    /// (the oldest until another is clicked). A waiting agent with no card says so. `None` when
    /// there is nothing to say.
    fn questions(
        &self,
        terminal: TerminalStream,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let row = self.model.row(terminal)?;
        let cards = self.cards_of(terminal, row.workspace);
        let frame = || {
            div()
                .id("tab-questions")
                .debug_selector(|| "tab-questions".to_string())
                .role(Role::Region)
                .aria_label("Questions")
                .flex_none()
                .max_h(px(520.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(6.))
        };
        if cards.is_empty() {
            let waits = row
                .task
                .as_ref()
                .is_some_and(|k| k.stage == "waiting-human");
            return waits.then(|| {
                frame()
                    .p(px(14.))
                    .rounded(px(14.))
                    .bg(c(t.card))
                    .border_1()
                    .border_color(ca(t.needs_line))
                    .child(quiet(
                        "Waiting on you, no question card: its last message says what.",
                        t,
                    ))
                    .into_any_element()
            });
        }
        let open = self
            .card_open
            .as_ref()
            .filter(|id| cards.iter().any(|c| c.card == **id))
            .cloned()
            .unwrap_or_else(|| cards[0].card.clone());
        let body = frame()
            .child(section("Questions", Some(cards.len().to_string()), t))
            .children(cards.iter().enumerate().map(|(i, card)| {
                if card.card == open {
                    return self.card_block(i, card, true, t, cx);
                }
                let id = card.card.clone();
                ui::control(
                    ("card-row", i as u64),
                    Role::Button,
                    format!("{}: {}", card.card, card.body.asks),
                    t,
                )
                .debug_selector({
                    let id = id.clone();
                    move || format!("card-row-{id}")
                })
                .flex()
                .items_center()
                .gap(px(8.))
                .min_h(px(28.))
                .px(px(8.))
                .rounded(px(8.))
                .text_size(px(12.5))
                .map(|d| ui::paint(d, t.rest()))
                .hover(|s| ui::paint(s, t.pressed()))
                .child(div().text_color(c(t.needs_text)).child(card.card.clone()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(card.body.asks.clone()),
                )
                .child(div().text_color(c(t.t2)).child(state::short_span(
                    self.now.saturating_sub(card.opened_at).max(0),
                )))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.card_open = Some(id.clone());
                    cx.notify();
                }))
                .into_any_element()
            }));
        Some(ui::scroll(body).into_any_element())
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
            // Not read yet: what the agent row already says, its task and how far it is.
            None => match row.and_then(|r| r.task.as_ref()) {
                Some(task) => body
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_size(px(13.5))
                                    .child(format!("Task #{}", task.id)),
                            )
                            .child(
                                div()
                                    .debug_selector(|| "task-progress".to_string())
                                    .text_size(px(12.))
                                    .text_color(c(t.t2))
                                    .child(if task.steps_total > 0 {
                                        format!("{} of {} done", task.steps_done, task.steps_total)
                                    } else {
                                        task.stage.clone()
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(c(t.t1))
                            .child(task.goal.clone()),
                    )
                    .child(quiet("Reading the task.", t)),
                None => body.child(quiet("Reading the task.", t)),
            },
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
        ui::scroll(
            div()
                .id("task-strip")
                .debug_selector(|| "task-strip".to_string())
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
                .child(body),
        )
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
            .w_full()
            .min_w_0()
            .overflow_hidden()
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
                    .flex_wrap()
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
                            .debug_selector(|| "note-open".to_string())
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
        let ring = *t;

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
                        "{}, +{added} -{removed}, against {}",
                        state::count(changes.files.len(), "file"),
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
                                    .hover(|s| ui::paint(s, t.hover()))
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
            .focus_visible(move |s| ui::focus_ring(s, &ring))
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
            .child(ui::scroll(
                div()
                    .id("review-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(body),
            ))
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
                // An added or removed row keeps its tint under the pointer, only stronger,
                // and every row keeps its ink.
                let tint = |alpha: u32| match l.mark {
                    Mark::Added => Some(ca((DARK.done << 8) | alpha)),
                    Mark::Removed => Some(ca((DARK.fail << 8) | alpha)),
                    _ => None,
                };
                let ground = tint(0x26);
                let hovered = tint(0x3d).unwrap_or(ca(DARK.fill));
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
                    .hover(move |s| s.bg(hovered))
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

/// The scrollback of a pane: a block of about three screens is asked for once and the view is
/// cut from it on every wheel step, so a step inside it paints in the next frame. The next
/// block is asked for while half a screen is still held past the view.
impl Term {
    fn height(&self) -> usize {
        usize::from(self.sent.get().1.max(1))
    }

    /// The number of the first row on screen while scrolled back.
    fn top(&self) -> u64 {
        let back = (self.scroll + self.height()) as u64;
        self.total.saturating_sub(back).max(self.floor)
    }

    /// Move the view `rows` up (positive) or down, and draw the part of a row in `wheel_left`;
    /// the `Scrollback` to send when the rows held run short. 0 is the live screen.
    pub(crate) fn wheel(&mut self, rows: i64) -> Option<ControlRequest> {
        let most = match self.total {
            0 => i64::MAX,
            total => {
                (total.saturating_sub(self.floor) as usize).saturating_sub(self.height()) as i64
            }
        };
        let want = self.scroll as i64 + rows;
        let scroll = want.clamp(0, most.max(0)) as usize;
        if scroll as i64 != want {
            // Past an end: what was left over of a row moves nothing.
            self.wheel_left = 0.;
        }
        let was = (self.scroll, self.lift);
        self.scroll = scroll;
        if scroll == 0 {
            if was.0 > 0 {
                self.live();
            }
            return None;
        }
        self.show();
        if (self.scroll, self.lift) != was {
            self.selection = None;
        }
        self.fetch()
    }

    /// An answer to a `Scrollback`: held when it answers the one out, else dropped, so a late
    /// answer for an older place never jumps the view back.
    pub(crate) fn came(
        &mut self,
        id: &str,
        from: u64,
        total: u64,
        lines: Vec<String>,
    ) -> Option<ControlRequest> {
        if !self.asked_is(id) {
            return None;
        }
        let (_, up) = self.asked.take()?;
        // Rows written since the last answer: the view keeps the rows it shows.
        if self.block.is_some() {
            self.scroll += total.saturating_sub(self.total) as usize;
        }
        self.total = total;
        // Answered from below the row asked for: the older rows are gone.
        if from > total.saturating_sub(up as u64) {
            self.floor = from;
        }
        let most = (total.saturating_sub(self.floor) as usize).saturating_sub(self.height());
        self.scroll = self.scroll.min(most.max(1));
        self.block = Some((from, lines));
        self.show();
        self.fetch()
    }

    pub(crate) fn asked_is(&self, id: &str) -> bool {
        self.asked.as_ref().is_some_and(|(asked, _)| asked == id)
    }

    /// Back to the live screen: the rows held go, and an answer still out is dropped.
    pub(crate) fn live(&mut self) {
        self.scroll = 0;
        self.back = None;
        self.lift = 0.;
        self.block = None;
        self.asked = None;
        self.selection = None;
    }

    /// The view cut from the block, when the block holds all of it (or all there is).
    fn show(&mut self) {
        let Some((from, lines)) = &self.block else {
            return;
        };
        let (top, height) = (self.top(), self.height());
        let end = from + lines.len() as u64;
        if top < *from || (top + height as u64 > end && end < self.total) {
            return;
        }
        // Part of a row moved: the row above comes in too, and the page is drawn up by the rest.
        let early = usize::from(top > *from && self.wheel_left > 0.);
        let start = ((top - from) as usize - early).min(lines.len());
        let rows = lines[start..(start + height + early).min(lines.len())].to_vec();
        let (cols, screen) = self.sent.get();
        self.back = Some(Rc::new(page_of(rows, cols, screen + early as u16)));
        self.lift = if early == 1 { 1. - self.wheel_left } else { 0. };
    }

    /// A `Scrollback` for three screens around the view, when none is out and the view is
    /// within half a screen of the block's edge, or outside it.
    fn fetch(&mut self) -> Option<ControlRequest> {
        if self.asked.is_some() {
            return None;
        }
        let height = self.height();
        if let Some((from, lines)) = &self.block {
            let (top, margin) = (self.top(), (height / 2) as u64);
            let end = from + lines.len() as u64;
            let above = *from > self.floor && top < from + margin;
            let below = end < self.total && top + height as u64 + margin > end;
            if !above && !below {
                return None;
            }
        }
        let rows = (3 * height).min(MAX_SCROLLBACK_ROWS);
        let up = self.scroll + height + rows.saturating_sub(height) / 2;
        let id = link::fresh_id("back");
        self.asked = Some((id.clone(), up));
        Some(ControlRequest::Scrollback {
            id,
            terminal: self.terminal,
            up,
            rows,
        })
    }
}

/// Whole rows to scroll for one wheel event: a notch (`Lines`) is `WHEEL_ROWS` rows, a
/// trackpad's pixels are counted by the row height. What does not make a whole row yet is
/// kept in `left` (0 up to 1: a move down borrows a row) for the next event, so a slow swipe
/// still scrolls.
fn wheel_rows(delta: ScrollDelta, row: Pixels, left: &mut f32) -> i64 {
    let rows = match delta {
        ScrollDelta::Lines(lines) => lines.y * WHEEL_ROWS,
        ScrollDelta::Pixels(pixels) => pixels.y / row,
    };
    let total = *left + rows;
    let whole = total.floor();
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

    /// Rows `from..to` of a history, each named by its number.
    fn rows(from: u64, to: u64) -> Vec<String> {
        (from..to).map(|n| format!("r{n}")).collect()
    }

    /// A pane of 10 rows over a history of `total` rows, scrolled back 3 with its block in.
    fn scrolled(total: u64) -> Term {
        let mut term = Term::new(1);
        term.sent.set((80, 10));
        let (id, up, asked) = ask(term.wheel(3));
        assert_eq!((up, asked), (23, 30), "three screens, one above the view");
        let from = total - up as u64;
        assert_eq!(term.came(&id, from, total, rows(from, total)), None);
        term
    }

    fn ask(request: Option<ControlRequest>) -> (String, usize, usize) {
        match request {
            Some(ControlRequest::Scrollback { id, up, rows, .. }) => (id, up, rows),
            other => panic!("not a Scrollback: {other:?}"),
        }
    }

    fn first(term: &Term) -> &str {
        &term.back.as_ref().expect("a page of scrollback").lines[0]
    }

    /// Steps inside the block are cut from it with no round trip; half a screen from its edge
    /// the next block is asked for, once, whatever the wheel does meanwhile.
    #[test]
    fn wheel_steps_inside_the_block_ask_nothing() {
        let mut term = scrolled(1000);
        assert_eq!(first(&term), "r987");
        for top in (982..987).rev() {
            assert_eq!(term.wheel(1), None, "row {top} is held");
            assert_eq!(first(&term), format!("r{top}"));
        }
        let (id, up, _) = ask(term.wheel(1));
        assert_eq!(up, 29);
        assert_eq!(first(&term), "r981", "still cut from the held rows");
        assert_eq!(term.wheel(3), None, "one ask out at a time");
        assert_eq!(term.came(&id, 971, 1000, rows(971, 1000)), None);
        assert_eq!(first(&term), "r978");
    }

    /// An answer that is not to the ask out (an older one, or one out when the view went back
    /// to the live screen) is dropped and never moves the view.
    #[test]
    fn a_stale_answer_is_dropped() {
        let mut term = scrolled(1000);
        for _ in 0..6 {
            term.wheel(1);
        }
        let (id, _) = term.asked.clone().expect("an ask out");
        assert_eq!(term.came("older", 500, 1000, rows(500, 530)), None);
        assert_eq!(first(&term), "r981");
        assert_eq!(term.wheel(-9), None);
        assert_eq!(term.scroll, 0, "back to the live screen");
        assert_eq!(term.came(&id, 971, 1000, rows(971, 1000)), None);
        assert!(term.back.is_none(), "the live screen stays");
        assert_eq!(term.scroll, 0);
    }

    /// Output that keeps coming while scrolled back does not move the rows on screen.
    #[test]
    fn new_output_keeps_the_view_still() {
        let mut term = scrolled(1000);
        for _ in 0..6 {
            term.wheel(1);
        }
        let (id, up) = term.asked.clone().expect("an ask out");
        assert_eq!(up, 29);
        // 4 rows were written before the daemon read: its block starts 4 rows later.
        assert_eq!(term.came(&id, 975, 1004, rows(975, 1004)), None);
        assert_eq!(term.scroll, 13);
        assert_eq!(first(&term), "r981");
    }

    /// Part of a row moves the page by pixels: the row above comes in, the page is drawn up by
    /// the rest of a row, and a selection made on the old place goes.
    #[test]
    fn part_of_a_row_lifts_the_page() {
        let mut term = scrolled(1000);
        term.selection = Some(((1, 0), (2, 4)));
        term.wheel_left = 0.25;
        assert_eq!(term.wheel(0), None);
        assert_eq!(first(&term), "r986");
        assert_eq!(
            term.back.as_ref().map(|b| (b.rows, b.lines.len())),
            Some((11, 11))
        );
        assert_eq!(term.lift, 0.75);
        assert_eq!(term.selection, None);
        // Down past a row boundary: a whole row back and the leftover still drawn.
        let mut left = term.wheel_left;
        let rows = wheel_rows(
            ScrollDelta::Pixels(point(px(0.), px(-8.))),
            px(16.),
            &mut left,
        );
        term.wheel_left = left;
        assert_eq!((rows, left), (-1, 0.75));
        term.wheel(rows);
        assert_eq!((term.scroll, first(&term), term.lift), (2, "r987", 0.25));
        // Back on the live screen nothing is lifted.
        term.wheel(-2);
        assert_eq!((term.scroll, term.lift), (0, 0.));
        assert!(term.back.is_none());
    }

    /// Once the daemon answers from its oldest kept row, the wheel stops there.
    #[test]
    fn the_oldest_kept_row_is_the_top() {
        let mut term = Term::new(1);
        term.sent.set((80, 10));
        let (id, up, _) = ask(term.wheel(30));
        assert_eq!(up, 50);
        // A history of 30 rows that kept only its last 25.
        assert_eq!(term.came(&id, 5, 30, rows(5, 30)), None);
        assert_eq!((term.floor, term.scroll), (5, 15));
        assert_eq!(first(&term), "r5");
        assert_eq!(term.wheel(1), None, "nothing older to ask for");
        assert_eq!(term.scroll, 15);
        assert_eq!(term.wheel(-2), None);
        assert_eq!(first(&term), "r7");
    }
}
