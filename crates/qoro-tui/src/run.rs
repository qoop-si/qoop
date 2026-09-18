//! The loop: read what the user typed, read what the daemon said, paint when something
//! changed.
//!
//! Nothing here is on a timer. Every wake is an arrival, which is the same rule the daemon
//! holds itself to, and it is the reason an interface with eight terminals open costs
//! nothing while nobody is typing.

use std::io::{self, Write};
use std::path::Path;

use bytes::BytesMut;
use qoro_runtime::client::{self, Connection, Incoming, TerminalGuard};
use qoro_runtime::control::{AgentRow, ControlRequest, ControlResponse, EventMessage};
use qoro_runtime::daemon::{ClientWire, SnapshotWire};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::keys::{Action, Keys};
use crate::palette::{self, Palette};
use crate::screen::{self, Frame};
use crate::{sidebar, tabs};

/// What to ask for, given where the person has scrolled to.
///
/// A window's worth, plus the same again above it, so an unhurried scroll finds the next
/// rows already here rather than waiting for a round trip at every notch.
fn behind_request(terminal: u32, rows_back: usize, pane_rows: usize) -> ControlRequest {
    // At most a pane's worth of the window is ever read from behind the screen, so that is
    // what has to be covered. A margin above it is prefetch, so an unhurried scroll finds
    // the next rows already here rather than waiting for a round trip at every notch.
    //
    // The margin is whatever is left inside what the daemon will answer with. It has to be
    // this way round: the daemon keeps the oldest end of what it is asked for, so a request
    // larger than that ceiling loses its newest rows, which are exactly the ones next to
    // the screen.
    let margin = qoro_runtime::daemon::MAX_SCROLLBACK_ROWS
        .saturating_sub(pane_rows)
        .min(pane_rows);
    ControlRequest::Scrollback {
        id: "behind".to_string(),
        terminal,
        up: rows_back + margin,
        rows: pane_rows + margin,
    }
}

/// What a client has been told about the rows behind the screen.
///
/// A window's worth at a time, fetched as the person scrolls into it, because the whole
/// history of a terminal that has been running a week is not something to hold in a
/// client in order to draw twenty rows of it.
#[derive(Clone, Debug, Default)]
pub struct Behind {
    /// Which terminal these rows belong to. A client that kept another terminal's rows
    /// would paint them under this one's tab bar.
    pub terminal: u32,
    /// The index of `lines[0]`, counted from the oldest row ever written.
    pub from: u64,
    pub lines: Vec<String>,
    /// How many rows there are behind the screen in total.
    pub total: u64,
}

impl Behind {
    /// One row of the history, by its own index, if this client has it.
    pub fn row(&self, at: u64) -> Option<&str> {
        let offset = at.checked_sub(self.from)? as usize;
        self.lines.get(offset).map(String::as_str)
    }
}

/// How many rows one notch of the wheel moves, where the interface decides rather than
/// the program. Three is what a terminal sends and what a hand expects.
const WHEEL_ROWS: usize = 3;

/// What to say when something has been copied.
fn copied(text: &str) -> String {
    let lines = text.lines().count();
    if lines > 1 {
        return format!("copied {lines} lines");
    }
    let one = text.trim();
    // Shown, so a person can see they got the right thing, and cut so a long line does not
    // push the rest of the notice off the row.
    if crate::width::width(one) > 40 {
        format!(
            "copied {}\u{2026}",
            crate::selection::in_columns(one, 0..39)
        )
    } else {
        format!("copied {one}")
    }
}

/// One mouse report in the form a program that asked for the mouse expects.
///
/// SGR (`CSI ? 1006`), which every program that asks for the mouse these days understands
/// and which is the form this interface itself reads from the terminal above it. The
/// coordinates are the pane's own, one based, because the program believes its pane is the
/// whole screen.
fn mouse_report(button: u16, row: usize, col: usize, press: bool) -> Option<Vec<u8>> {
    Some(
        format!(
            "\x1b[<{button};{};{}{}",
            col + 1,
            row + 1,
            if press { 'M' } else { 'm' }
        )
        .into_bytes(),
    )
}

/// Send bytes to the focused terminal, if there is one.
async fn send_to_pane(
    state: &mut State,
    seqs: &mut std::collections::BTreeMap<u32, u32>,
    wr: &mut (impl tokio::io::AsyncWriteExt + Unpin),
    bytes: Vec<u8>,
) -> std::io::Result<()> {
    let focused = state.focused;
    if focused == 0 {
        return Ok(());
    }
    let frame = client::encode_command_to(
        focused,
        &ClientWire::Input { bytes },
        seqs.entry(focused).or_default(),
    )?;
    wr.write_all(&frame).await
}

/// Everything the screen is drawn from.
struct State {
    rows: Vec<AgentRow>,
    focused: u32,
    snapshot: Option<SnapshotWire>,
    cols: u16,
    lines: u16,
    message: Option<String>,
    /// A terminal to move to once the loop can write the subscription for it.
    pending_focus: Option<u32>,
    /// The workspaces this daemon holds, and which one is on screen.
    spaces: Vec<qoro_runtime::control::WorkspaceRow>,
    space: Option<i64>,
    /// What each terminal's state was at the last index, so a change into one that wants a
    /// person can be noticed. Without it the interface would have to compare a row against
    /// itself, which is a comparison that is always equal.
    was: std::collections::BTreeMap<u32, String>,
    /// Set when something started needing a person, cleared when the bell has rung.
    ring: bool,
    /// Spaces that have just been made and have nothing in them yet.
    ///
    /// A queue and not one slot. Two answers can land in the same read, and a slot kept the
    /// second and dropped the first, which left a space nobody could put a terminal in
    /// except by finding it and pressing `ctrl-a c`.
    wants_terminal_in: Vec<i64>,
    /// Whether this client has ever had a terminal on screen.
    adopted: bool,
    /// Set when there is nothing left to be an interface to.
    done: bool,
    /// The open naming box, when a person is being asked what to call something.
    prompt: Option<Prompt>,
    /// The open menu, and which of its items is selected.
    menu: Option<usize>,
    /// Whether the left column is folded away to a strip.
    collapsed: bool,
    /// An overlay showing something read only: the keys, or what this daemon is set to.
    reading: Option<Reading>,
    /// Spaces asked for and not yet answered for.
    ///
    /// Only the naming needs this, and the naming needs it badly: two presses of `ctrl-a w`
    /// before either answer came back both counted the same list and both produced
    /// `space 2`, so the panel showed two rows with one name on them.
    creating: usize,
    /// Which idle groups of the agents panel are expanded.
    fold: sidebar::Fold,
    /// The open search palette, or `None` when the terminal has the keyboard.
    palette: Option<Palette>,
    /// The start of a palette key sequence the read ended in the middle of.
    palette_pending: Vec<u8>,
    /// Something worth saying once, until the person does anything at all.
    ///
    /// Not a message: a message is cleared by the first render, which arrives in
    /// milliseconds, so a thing said that way is a thing nobody reads. This sits under
    /// everything urgent and goes on the first keystroke.
    hint: Option<String>,
    /// The row the top of the pane is pinned to, when the pane is not at the bottom.
    ///
    /// An absolute row number and not a distance, because a distance from the bottom is a
    /// distance from something that moves: a person scrolled back to read while an agent
    /// prints would be dragged down the screen by the output they are reading past. Pinned
    /// to a row, the text under their eyes stays still.
    ///
    /// `None` is the live screen, which is where every key press puts it back: typing is
    /// talking to the program, and the program is at the bottom.
    anchor: Option<u64>,
    /// The rows behind the screen, as far back as they have been asked for, and how many
    /// there are in total.
    behind: Behind,
    /// Text being selected with the mouse, or selected and still shown.
    ///
    /// Kept after the button comes up rather than cleared, because a person who has just
    /// copied something looks at it to check they got the right thing. The next press in
    /// the pane clears it.
    selection: Option<crate::selection::Selection>,
}

impl State {
    fn frame(&self) -> Frame<'_> {
        Frame {
            rows: &self.rows,
            spaces: &self.spaces,
            space: self.space,
            focused: self.focused,
            snapshot: self.snapshot.as_ref(),
            cols: self.cols,
            lines: self.lines,
            message: self.message.as_deref(),
            hint: self.hint.as_deref(),
            fold: self.fold,
            palette: self.palette.as_ref(),
            prompt: self.prompt.as_ref(),
            menu: self.menu,
            collapsed: self.collapsed,
            reading: self.reading,
            selection: self.selection,
            scroll: self.scrolled_rows(),
            anchor: self.anchor.unwrap_or_default(),
            rows_back: self.rows_back(),
            behind: &self.behind,
        }
    }

    /// The tabs of the space on screen, in the order the daemon reports them.
    fn tabs(&self) -> Vec<AgentRow> {
        screen::in_space(&self.rows, self.space)
    }

    /// What a click at a one based screen position means.
    ///
    /// Asked of the same modules that drew the row, because both of them scroll to keep
    /// the focused entry visible: the row a person clicked is not the entry's position in
    /// the list, and computing it here would be a second copy of that arithmetic that
    /// starts out right and drifts.
    /// Where in the pane a screen position is, if it is in the pane at all.
    ///
    /// Zero based, which is what the snapshot's rows and the selection both count in. The
    /// pane starts under the tab bar and to the right of the column, and the two status
    /// rows at the foot are not it.
    fn in_pane(&self, row: u16, col: u16) -> Option<(usize, usize)> {
        let (pane_cols, pane_rows) = screen::pane_size_for(self.cols, self.lines, self.collapsed);
        let left = screen::left_for(self.cols, self.collapsed);
        let first_col = left + 1;
        if (col as usize) < first_col || (col as usize) >= first_col + pane_cols as usize {
            return None;
        }
        let within = (row as usize).checked_sub(screen::TABS + 1)?;
        (within < pane_rows as usize).then_some((within, col as usize - first_col))
    }

    /// How many of the pane's rows come from behind the screen.
    ///
    /// Derived rather than stored: what is fixed is the row the top of the pane is pinned
    /// to, and how much of the pane that fills changes by itself as the terminal prints.
    fn scrolled_rows(&self) -> usize {
        let (_, pane_rows) = screen::pane_size_for(self.cols, self.lines, self.collapsed);
        let Some(anchor) = self.anchor else {
            return 0;
        };
        self.live_behind()
            .saturating_sub(anchor)
            .min(pane_rows as u64) as usize
    }

    /// How far back from the newest row the top of the pane is.
    fn rows_back(&self) -> u64 {
        self.anchor
            .map(|anchor| self.live_behind().saturating_sub(anchor))
            .unwrap_or(0)
    }

    /// How many rows are behind this terminal's screen, as of the last thing it said.
    ///
    /// From the snapshot rather than from the last scrollback answer, because the snapshot
    /// is what arrives on every frame: a count that only moved when rows were asked for
    /// would be stale exactly while the terminal is busy.
    fn live_behind(&self) -> u64 {
        self.snapshot
            .as_ref()
            .map(|s| s.behind)
            .unwrap_or(self.behind.total)
    }

    /// Whether the program in the focused terminal has asked for the mouse.
    ///
    /// When it has, the mouse is its own: a drag is a drag in its own interface, and
    /// taking that away to draw a selection would break scrolling and clicking inside
    /// every agent. Shift is how a person says otherwise, which is what every terminal
    /// emulator already trains them to do.
    fn pane_wants_mouse(&self) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|s| s.modes.mouse_reporting)
    }

    /// Whether the program is on the alternate screen, where a wheel is a scroll of its
    /// own view rather than of the scrollback behind it.
    fn pane_is_full_screen(&self) -> bool {
        self.snapshot.as_ref().is_some_and(|s| s.modes.alt_screen)
    }

    /// Whether the program wants its arrows spelled `SS3 A` rather than `CSI A`.
    fn pane_application_cursor(&self) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|s| s.modes.application_cursor)
    }

    fn clicked(&self, row: u16, col: u16) -> Option<Clicked> {
        let (pane_cols, _) = screen::pane_size_for(self.cols, self.lines, self.collapsed);
        let left = screen::left_for(self.cols, self.collapsed);
        // Right of the column, which is the pane and the bar above it.
        if left == 0 || col as usize > left {
            if (row as usize) != screen::TABS {
                // The pane. A terminal application that wants the mouse asks for it, and
                // passing a click to one that did not would type its report at a prompt.
                return None;
            }
            // The bar starts where the pane starts, so the column it was clicked at is
            // counted from there and not from the edge of the screen.
            return match tabs::hit(
                &self.tabs(),
                self.focused,
                pane_cols as usize,
                (col as usize).saturating_sub(left + 1),
            ) {
                Some(tabs::Hit::Terminal(terminal)) => Some(Clicked::Terminal(terminal)),
                Some(tabs::Hit::New) => Some(Clicked::NewTerminal),
                None => None,
            };
        }
        // The column, which starts at the top row and runs down to the status line.
        let within = (row as usize).checked_sub(screen::SIDEBAR_TOP)?;
        let column_rows = (self.lines as usize).saturating_sub(screen::STATUS);
        let hit = if self.collapsed {
            sidebar::rows_collapsed(
                &self.spaces,
                &self.rows,
                self.space.unwrap_or_default(),
                self.focused,
                column_rows,
            )
            .into_iter()
            .nth(within)
            .and_then(|(_, hit)| hit)
        } else {
            sidebar::hit_folded(
                &self.spaces,
                &self.rows,
                self.space.unwrap_or_default(),
                self.focused,
                column_rows,
                &self.fold,
                within,
            )
        };
        // The row between the panels answers by which half was clicked, so the column has
        // to ask a second question the row itself cannot answer.
        let hit = hit.or_else(|| {
            let rows = sidebar::rows_folded(
                &self.spaces,
                &self.rows,
                self.space.unwrap_or_default(),
                self.focused,
                column_rows,
                &self.fold,
            );
            let is_footer = rows
                .get(within)
                .is_some_and(|(line, _)| line.text.contains("menu"));
            (is_footer && !self.collapsed).then(|| sidebar::footer_hit(col as usize))?
        });
        match hit? {
            sidebar::Hit::Space(space) => Some(Clicked::Space(space)),
            sidebar::Hit::Terminal(terminal) => Some(Clicked::Terminal(terminal)),
            sidebar::Hit::Fold(group) => Some(Clicked::Fold(group)),
            sidebar::Hit::New => Some(Clicked::NewTerminal),
            sidebar::Hit::Menu => Some(Clicked::Menu),
            sidebar::Hit::Collapse => Some(Clicked::Collapse),
        }
    }

    /// The tab after the focused one, wrapping, within the space on screen.
    ///
    /// Within the space, because the tab bar is what "next" refers to. Wrapping across
    /// every terminal on the daemon would move a person into a space they were not looking
    /// at, and the tab bar would change under them without their having asked.
    fn neighbour(&self, forward: bool) -> Option<u32> {
        let tabs = self.tabs();
        if tabs.is_empty() {
            return None;
        }
        let at = tabs
            .iter()
            .position(|r| r.terminal == self.focused)
            .unwrap_or(0);
        let n = tabs.len();
        let next = if forward {
            (at + 1) % n
        } else {
            (at + n - 1) % n
        };
        Some(tabs[next].terminal)
    }

    /// The space after the focused one, wrapping.
    fn next_space(&self, forward: bool) -> Option<i64> {
        if self.spaces.is_empty() {
            return None;
        }
        let at = self
            .spaces
            .iter()
            .position(|w| Some(w.id) == self.space)
            .unwrap_or(0);
        let n = self.spaces.len();
        let next = if forward {
            (at + 1) % n
        } else {
            (at + n - 1) % n
        };
        Some(self.spaces[next].id)
    }
}

/// What one read of stdin meant while the menu owns the keyboard.
enum MenuOutcome {
    Moved,
    Closed,
    Chose(usize),
    Passthrough(Vec<u8>),
}

/// Decide what one read of stdin meant while the menu is open.
///
/// Up and down move, enter chooses, esc closes, and the first letter of a row jumps to it.
/// A prefix or a bare detach closes the menu and hands the bytes on, so `ctrl-a d` still
/// detaches out of it.
fn menu_key(state: &mut State, selected: usize, bytes: &[u8]) -> MenuOutcome {
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        if byte == 0x1b && bytes.get(at + 1) == Some(&b'[') {
            // An arrow, or a mouse report to swallow whole.
            match bytes.get(at + 2) {
                Some(b'A') => {
                    state.menu = Some(selected.saturating_sub(1));
                    return MenuOutcome::Moved;
                }
                Some(b'B') => {
                    state.menu = Some((selected + 1).min(MENU.len() - 1));
                    return MenuOutcome::Moved;
                }
                _ => match bytes[at..].iter().position(|b| b.is_ascii_alphabetic()) {
                    Some(end) => {
                        at += end + 1;
                        continue;
                    }
                    None => break,
                },
            }
        }
        at += 1;
        match byte {
            b'\r' | b'\n' => {
                state.menu = None;
                return MenuOutcome::Chose(selected);
            }
            0x1b => {
                state.menu = None;
                return MenuOutcome::Closed;
            }
            0x01 | 0x11 => {
                state.menu = None;
                return MenuOutcome::Passthrough(bytes[at - 1..].to_vec());
            }
            letter => {
                if let Some(found) = MENU
                    .iter()
                    .position(|(name, _)| name.as_bytes().first() == Some(&letter))
                {
                    state.menu = Some(found);
                    return MenuOutcome::Moved;
                }
            }
        }
    }
    MenuOutcome::Moved
}

/// What the menu offers.
///
/// Everything here does something today. A menu row that opened nothing would be the one
/// place in the interface that lies about what the product can do, and a person who found
/// one would be right to stop trusting the rest.
pub const MENU: &[(&str, &str)] = &[
    ("keybinds", "every key, and what it does"),
    ("settings", "what this daemon is set to"),
    ("reload manifests", "re-read the agent detection overrides"),
    ("detach", "leave, and keep everything running"),
];

/// A read only overlay opened from the menu.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Reading {
    Keybinds,
    Settings,
}

/// A person being asked what to call the thing they just asked for.
///
/// Asked and not assumed. A space called `space 3` and a tab called `zsh` are both names
/// nobody chose, and a fleet of them is a list a person reads by counting rather than by
/// recognising. Empty is a real answer: it means "you name it", and the default is used.
pub struct Prompt {
    pub what: Naming,
    pub text: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Naming {
    Space,
    Terminal,
    /// Rename what already exists, which is the common case: a name given at creation is a
    /// guess, and the terminal somebody opened to "look at something" is the one they are
    /// still in an hour later doing the thing it should have been called after.
    RenameSpace(i64),
    RenameTerminal(u32),
}

impl Naming {
    /// What the box says it is asking for.
    pub fn asks(self) -> &'static str {
        match self {
            Naming::Space => "name the new space",
            Naming::Terminal => "name the new terminal",
            Naming::RenameSpace(_) => "rename this space",
            Naming::RenameTerminal(_) => "rename this tab",
        }
    }

    /// What an empty answer means here, which is not the same thing in all four cases.
    pub fn hint(self) -> &'static str {
        match self {
            Naming::Space | Naming::Terminal => {
                "enter to make it, esc to stop, empty for a default name"
            }
            Naming::RenameTerminal(_) => {
                "enter to rename, esc to stop, empty for the program's own name"
            }
            // A space with no name is a space nobody can point at, so there is nothing for
            // empty to mean and it is a cancel.
            Naming::RenameSpace(_) => "enter to rename, esc to stop",
        }
    }
}

/// What one read of stdin meant while the naming box owns the keyboard.
enum PromptOutcome {
    /// Nothing to send, and the box is still open.
    Typing,
    /// Closed with nothing done.
    Closed,
    /// Confirmed, with what was typed, and whatever was typed after the enter.
    ///
    /// A read is however many bytes happened to arrive, so the key that opens the next
    /// thing can land in the same one as the enter that closed this box. Dropped, those
    /// keystrokes vanished with nothing on screen to say so: measured, a second `ctrl-a w`
    /// typed straight after confirming the first never opened a box at all.
    Confirm(Naming, Option<String>, Vec<u8>),
    /// Closed, and these bytes belong to the key decoder rather than to the box.
    Passthrough(Vec<u8>),
}

/// Decide what one read of stdin meant while the naming box owns the keyboard.
///
/// Typing goes into the name, `enter` confirms it, `esc` is out, and backspace rubs out.
/// A prefix or a bare detach closes the box and hands the bytes on from there, so
/// `ctrl-a d` still detaches out of it, which is the same rule the palette follows.
fn prompt_key(state: &mut State, bytes: &[u8]) -> PromptOutcome {
    let Some(prompt) = state.prompt.as_mut() else {
        return PromptOutcome::Passthrough(bytes.to_vec());
    };
    let mut at = 0;
    while at < bytes.len() {
        let byte = &bytes[at];
        // An escape sequence, which here is never a name. A mouse report is the one that
        // matters: the box is opened by a click on the `+`, and the release of that very
        // click arrives a moment later. Read byte by byte its leading escape is a cancel,
        // so the box opened and shut again before anybody could type into it, which looked
        // like the button doing nothing at all.
        if *byte == 0x1b && bytes.get(at + 1) == Some(&b'[') {
            match bytes[at..].iter().position(|b| b.is_ascii_alphabetic()) {
                Some(end) => {
                    at += end + 1;
                    continue;
                }
                // Split across the read. Held rather than passed on, so half a report
                // cannot land in a name.
                None => break,
            }
        }
        at += 1;
        let at = at - 1;
        match byte {
            // Enter.
            b'\r' | b'\n' => {
                let what = prompt.what;
                let text = prompt.text.trim().to_string();
                state.prompt = None;
                return PromptOutcome::Confirm(
                    what,
                    (!text.is_empty()).then_some(text),
                    bytes[at + 1..].to_vec(),
                );
            }
            // Escape, which here is a cancel and never the start of a sequence: the box
            // holds no arrow keys, and waiting for a next read that may never come would
            // swallow the one key that closes it.
            0x1b => {
                state.prompt = None;
                return PromptOutcome::Closed;
            }
            // Backspace, both spellings a terminal sends.
            0x7f | 0x08 => {
                prompt.text.pop();
            }
            // The prefix, or a bare detach: not this box's keys.
            0x01 | 0x11 => {
                state.prompt = None;
                return PromptOutcome::Passthrough(bytes[at..].to_vec());
            }
            byte if (byte.is_ascii_graphic() || *byte == b' ')
                && crate::width::width(&prompt.text) < NAME_LIMIT =>
            {
                prompt.text.push(*byte as char);
            }
            // Anything else, a control byte or half a character, is not a name.
            _ => {}
        }
    }
    PromptOutcome::Typing
}

/// The most columns a name may take, which is the column it has to fit in.
const NAME_LIMIT: usize = crate::sidebar::WIDTH - 6;

/// What the interface drew where a person clicked.
enum Clicked {
    Menu,
    Collapse,
    Space(i64),
    Terminal(u32),
    NewTerminal,
    Fold(sidebar::Group),
}

/// What one read of stdin meant while the palette owns the keyboard.
#[derive(Debug)]
enum PaletteOutcome {
    /// The query or the selection moved: repaint.
    Updated,
    /// The palette closed with nothing picked: repaint.
    Closed,
    /// The palette closed with something picked: go there.
    Confirm(palette::Item),
    /// The palette closed and these bytes are ordinary input after all: feed
    /// them to the key decoder as though the palette had never been open.
    Passthrough(Vec<u8>),
}

/// Run the interface until the user detaches or the daemon goes away.
pub async fn run(socket: &Path) -> io::Result<()> {
    run_with_notice(socket, None).await
}

/// The interface, with something to say on its first frame.
///
/// The notice is how a first start says that an agent on this machine has no hook
/// installed. It is said once, on the row that says what just happened, and it goes the
/// moment anything else happens: nothing is blocked, nothing is asked, and an agent with no
/// hook is still read from its screen.
pub async fn run_with_notice(socket: &Path, notice: Option<String>) -> io::Result<()> {
    let (stream, leftover) = client::connect(socket, "qoro-tui").await?;
    let guard = TerminalGuard::enter_raw()?;
    // A panic must not leave the outer terminal raw either, which is the one failure a
    // person cannot recover from without knowing that `qoro reset` exists.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        TerminalGuard::restore();
        previous_hook(info);
    }));

    let outcome = drive(stream, leftover, notice).await;

    drop(guard);
    outcome
}

async fn drive(
    stream: tokio::net::UnixStream,
    leftover: BytesMut,
    notice: Option<String>,
) -> io::Result<()> {
    let (mut rd, mut wr) = stream.into_split();
    let (cols, lines) = client::outer_size();
    let mut state = State {
        rows: Vec::new(),
        focused: 0,
        snapshot: None,
        cols,
        lines,
        // What the caller wanted said, or that this is connecting. A notice about a
        // missing hook is worth more than the word "connecting", which is gone by the time
        // anybody reads it.
        message: Some("connecting".to_string()),
        hint: notice,
        pending_focus: None,
        spaces: Vec::new(),
        space: None,
        was: std::collections::BTreeMap::new(),
        ring: false,
        prompt: None,
        menu: None,
        collapsed: false,
        reading: None,
        wants_terminal_in: Vec::new(),
        adopted: false,
        creating: 0,
        done: false,
        fold: sidebar::Fold::default(),
        palette: None,
        palette_pending: Vec::new(),
        selection: None,
        anchor: None,
        behind: Behind::default(),
    };

    // One sequence per terminal, because a chunk sequence is per stream: sharing a counter
    // across terminals makes the numbers on any one of them non consecutive, and the
    // daemon's reassembler is right to refuse that.
    let mut seqs: std::collections::BTreeMap<u32, u32> = std::collections::BTreeMap::new();
    let mut control_seq = 0u32;
    let mut request = 0u64;
    let mut keys = Keys::default();
    let mut connection = Connection::new();
    let mut inbuf = leftover;
    inbuf.reserve(256 * 1024);
    let mut read = [0u8; 64 * 1024];

    let (tx, mut stdin_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    client::spawn_stdin_reader(tx);
    let mut sigwinch =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    // An interval, and not an acknowledgement sent only when something arrives. A client
    // that has taken a render and then goes quiet has nothing to reply to, so it never
    // says it kept up, and after thirty seconds the daemon is right to conclude it stopped
    // reading (ADR-013). Measured before this was here: the interface exited on its own
    // after a minute of sitting still, with no error, which reads as a crash.
    let mut ack_tick = tokio::time::interval(client::ACK_INTERVAL);
    ack_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // Ask what is there before drawing anything, so the first frame is the real index
    // rather than an empty one that fills in a moment later.
    let mut ask = |request: &mut u64, what: ControlRequest| -> io::Result<BytesMut> {
        *request += 1;
        let _ = what;
        client::encode_control_seq(&what, &mut control_seq)
    };
    wr.write_all(&ask(
        &mut request,
        ControlRequest::ListAgents { id: "index".into() },
    )?)
    .await?;
    wr.write_all(&ask(
        &mut request,
        ControlRequest::ListWorkspaces {
            id: "spaces".into(),
        },
    )?)
    .await?;

    let (pane_cols, pane_rows) = screen::pane_size(state.cols, state.lines);
    wr.write_all(&client::encode_command_to(
        1,
        &ClientWire::Resize {
            cols: pane_cols,
            rows: pane_rows,
        },
        seqs.entry(1).or_default(),
    )?)
    .await?;

    let mut out = io::stdout();
    // The alternate screen, so that detaching gives the user their scrollback back, and
    // mouse reporting, so a click on the index picks a terminal.
    //
    // `1002` is press, release and motion while a button is held. Not `1003`, which
    // reports the pointer moving over an idle screen: that is a report per row crossed for
    // nothing, and this interface costs no wakeups while nothing happens. Not `1000`
    // either, which is what this used to be and which has no motion at all: without motion
    // a drag is a press and a release in two different places, and a person dragging
    // across a line of an answer selects nothing.
    // `1006` asks for those reports in the extended form, because the original one cannot
    // express a column past 223 and this interface is regularly wider than that.
    out.write_all(b"\x1b[?1049h\x1b[?1002h\x1b[?1006h")?;
    out.flush()?;
    let mut dirty = true;

    let outcome: io::Result<()> = async {
        loop {
            if dirty {
                let mut painted = screen::paint(&state.frame());
                if state.ring {
                    // After the frame, so the terminal has the new colour on screen when
                    // the person looks up.
                    painted.push('\x07');
                    state.ring = false;
                }
                out.write_all(painted.as_bytes())?;
                out.flush()?;
                dirty = false;
            }

            tokio::select! {
                biased;
                _ = ack_tick.tick() => {
                    if let Some(ack) = connection.renders().due_ack(std::time::Instant::now()) {
                        wr.write_all(&ack).await?;
                    }
                }
                _ = sigterm.recv() => return Ok(()),
                _ = sighup.recv() => return Ok(()),
                _ = sigwinch.recv() => {
                    let (cols, lines) = client::outer_size();
                    state.cols = cols;
                    state.lines = lines;
                    let (pane_cols, pane_rows) = screen::pane_size(cols, lines);
                    // The focused terminal only. A resize is a claim about the window the
                    // person is looking at, and sending it to every terminal would reflow
                    // the ones they are not.
                    let focused = state.focused;
                    wr.write_all(&client::encode_command_to(
                        focused,
                        &ClientWire::Resize { cols: pane_cols, rows: pane_rows },
                        seqs.entry(focused).or_default(),
                    )?).await?;
                    // A taller window reads further back, and what was fetched for the
                    // shorter one stops at the row the shorter one needed: without asking
                    // again, growing the window while scrolled back fills the new rows at
                    // the top with blanks until the person turns the wheel.
                    if state.anchor.is_some() && focused != 0 {
                        let want = behind_request(
                            focused,
                            state.rows_back() as usize,
                            pane_rows as usize,
                        );
                        wr.write_all(&client::encode_control_seq(&want, &mut control_seq)?)
                            .await?;
                    }
                    dirty = true;
                }
                typed = stdin_rx.recv() => {
                    let Some(bytes) = typed else { return Ok(()) };
                    // The palette owns the keyboard while it is open: typing filters,
                    // enter goes, esc is back. A prefix or a detach inside it closes
                    // the palette and the bytes from there on are ordinary input, so
                    // `ctrl-a d` still detaches.
                    let mut feed: Option<Vec<u8>> = None;
                    if state.palette.is_some() {
                        match palette_key(&mut state, &bytes) {
                            PaletteOutcome::Updated | PaletteOutcome::Closed => {
                                dirty = true;
                            }
                            PaletteOutcome::Confirm(item) => {
                                match item {
                                    palette::Item::Terminal(picked) => {
                                        if let Some(out) = focus(
                                            &mut state, picked, &mut control_seq, &mut seqs,
                                        )? {
                                            wr.write_all(&out).await?;
                                        }
                                    }
                                    palette::Item::Space(space) => {
                                        if let Some(out) = enter_space(
                                            &mut state, space, &mut control_seq, &mut seqs,
                                        )? {
                                            wr.write_all(&out).await?;
                                        }
                                    }
                                    palette::Item::NewTerminal => {
                                        state.prompt = Some(Prompt {
                                            what: Naming::Terminal,
                                            text: String::new(),
                                        });
                                    }
                                    palette::Item::NewSpace => {
                                        state.prompt = Some(Prompt {
                                            what: Naming::Space,
                                            text: String::new(),
                                        });
                                    }
                                }
                                dirty = true;
                            }
                            PaletteOutcome::Passthrough(rest) => {
                                feed = Some(rest);
                            }
                        }
                    } else if let Some(selected) = state.menu {
                        // The menu owns the keyboard while it is open, the way the palette
                        // and the naming box do, and hands a prefix back for the same
                        // reason.
                        match menu_key(&mut state, selected, &bytes) {
                            MenuOutcome::Moved | MenuOutcome::Closed => dirty = true,
                            MenuOutcome::Chose(item) => {
                                match MENU[item].0 {
                                    "keybinds" => state.reading = Some(Reading::Keybinds),
                                    "settings" => state.reading = Some(Reading::Settings),
                                    "reload manifests" => {
                                        request += 1;
                                        wr.write_all(&client::encode_control_seq(
                                            &ControlRequest::ReloadAgentManifests {
                                                id: format!("reload-{request}"),
                                            },
                                            &mut control_seq,
                                        )?).await?;
                                    }
                                    "detach" => return Ok(()),
                                    _ => {}
                                }
                                dirty = true;
                            }
                            MenuOutcome::Passthrough(rest) => feed = Some(rest),
                        }
                    } else if state.reading.is_some() {
                        // Anything at all closes a thing that is only being read.
                        state.reading = None;
                        dirty = true;
                    } else if state.prompt.is_some() {
                        // The naming box owns the keyboard the same way the palette does,
                        // and hands a prefix or a detach back for the same reason.
                        match prompt_key(&mut state, &bytes) {
                            PromptOutcome::Typing | PromptOutcome::Closed => dirty = true,
                            PromptOutcome::Confirm(what, name, rest) => {
                                if !rest.is_empty() {
                                    feed = Some(rest);
                                }
                                request += 1;
                                let ask = match what {
                                    Naming::RenameTerminal(terminal) => {
                                        ControlRequest::RenameTerminal {
                                            id: format!("rename-{request}"),
                                            terminal,
                                            // Empty hands the tab back to its program's
                                            // name, which is the only way to undo a name
                                            // without inventing a second key for it.
                                            name,
                                        }
                                    }
                                    Naming::RenameSpace(workspace) => {
                                        match name {
                                            Some(name) => ControlRequest::RenameWorkspace {
                                                id: format!("rename-{request}"),
                                                workspace,
                                                name,
                                            },
                                            // A space with no name is a space nobody can
                                            // point at, so an empty answer is a cancel.
                                            None => {
                                                dirty = true;
                                                continue;
                                            }
                                        }
                                    }
                                    Naming::Terminal => ControlRequest::CreateTerminal {
                                        id: format!("new-{request}"),
                                        cwd: None,
                                        // The space the person is looking at.
                                        workspace: state.space,
                                        name,
                                    },
                                    Naming::Space => ControlRequest::CreateWorkspace {
                                        id: format!("space-{request}"),
                                        // Empty means "you name it", which is the numbered
                                        // name this counted before anybody was asked.
                                        name: name.unwrap_or_else(|| {
                                            format!(
                                                "space {}",
                                                state.spaces.len() + state.creating + 1
                                            )
                                        }),
                                    },
                                };
                                if matches!(what, Naming::Space) {
                                    state.creating += 1;
                                }
                                wr.write_all(&client::encode_control_seq(
                                    &ask, &mut control_seq,
                                )?).await?;
                                state.message = Some(match what {
                                    Naming::Terminal => "starting a terminal".to_string(),
                                    Naming::Space => "starting a space".to_string(),
                                    Naming::RenameSpace(_) | Naming::RenameTerminal(_) => {
                                        "renamed".to_string()
                                    }
                                });
                                dirty = true;
                            }
                            PromptOutcome::Passthrough(rest) => feed = Some(rest),
                        }
                    } else {
                        feed = Some(bytes);
                    }
                    if let Some(input) = feed {
                    for action in keys.feed(&input) {
                        match action {
                            Action::Input(bytes) => {
                                // The hint has been read or it has not; either way the
                                // person is working now.
                                state.hint = None;
                                // Typing is talking to the program, and the program is at
                                // the bottom. A key pressed while scrolled back would
                                // answer into a view of an hour ago, and the person would
                                // not see what they typed.
                                if state.anchor.take().is_some() {
                                    dirty = true;
                                }
                                // Nothing focused is a real state: a space with no
                                // terminals in it. Typing into stream zero addresses a
                                // terminal that does not exist, and typing into the one
                                // that was focused before addresses a terminal in a space
                                // the person has left and cannot see.
                                let focused = state.focused;
                                if focused != 0 {
                                    wr.write_all(&client::encode_command_to(
                                        focused,
                                        &ClientWire::Input { bytes },
                                        seqs.entry(focused).or_default(),
                                    )?).await?;
                                }
                            }
                            Action::Detach => return Ok(()),
                            Action::Click { row, col, shift } => {
                                // In the pane, and the program in it has not taken the
                                // mouse, or the person said this one is theirs: a press
                                // starts a selection rather than reaching anything.
                                if let Some((pane_row, pane_col)) = state.in_pane(row, col) {
                                    if state.pane_wants_mouse() && !shift {
                                        if let Some(bytes) =
                                            mouse_report(0, pane_row, pane_col, true)
                                        {
                                            send_to_pane(&mut state, &mut seqs, &mut wr, bytes)
                                                .await?;
                                        }
                                    } else {
                                        state.selection =
                                            Some(crate::selection::Selection::start(
                                                pane_row, pane_col,
                                            ));
                                        dirty = true;
                                    }
                                    continue;
                                }
                                // A press anywhere else puts down whatever was selected:
                                // the selection belongs to the pane it was made in.
                                if state.selection.take().is_some() {
                                    dirty = true;
                                }
                                match state.clicked(row, col) {
                                    Some(Clicked::Terminal(picked)) => {
                                        if let Some(bytes) = focus(
                                            &mut state, picked, &mut control_seq, &mut seqs,
                                        )? {
                                            wr.write_all(&bytes).await?;
                                            dirty = true;
                                        }
                                    }
                                    Some(Clicked::NewTerminal) => {
                                        state.prompt = Some(Prompt {
                                            what: Naming::Terminal,
                                            text: String::new(),
                                        });
                                        dirty = true;
                                    }
                                    Some(Clicked::Space(space)) => {
                                        if let Some(bytes) = enter_space(
                                            &mut state, space, &mut control_seq, &mut seqs,
                                        )? {
                                            wr.write_all(&bytes).await?;
                                        }
                                        dirty = true;
                                    }
                                    Some(Clicked::Fold(group)) => {
                                        state.fold.open(group);
                                        dirty = true;
                                    }
                                    Some(Clicked::Menu) => {
                                        state.menu = Some(0);
                                        dirty = true;
                                    }
                                    Some(Clicked::Collapse) => {
                                        state.collapsed = !state.collapsed;
                                        // The pane's width changed with the column, and a
                                        // child that is not told reflows its output against
                                        // a width it no longer has.
                                        let (cols, rows) = screen::pane_size_for(
                                            state.cols,
                                            state.lines,
                                            state.collapsed,
                                        );
                                        let focused = state.focused;
                                        if focused != 0 {
                                            wr.write_all(&client::encode_command_to(
                                                focused,
                                                &ClientWire::Resize { cols, rows },
                                                seqs.entry(focused).or_default(),
                                            )?).await?;
                                        }
                                        dirty = true;
                                    }
                                    None => {}
                                }
                            }
                            Action::Drag { row, col, shift } => {
                                let Some((pane_row, pane_col)) = state.in_pane(row, col) else {
                                    continue;
                                };
                                if let Some(selection) = state.selection.as_mut() {
                                    selection.extend(pane_row, pane_col);
                                    dirty = true;
                                } else if state.pane_wants_mouse() && !shift {
                                    // 32 is the motion flag on the left button, which is
                                    // what a program tracking a drag is listening for.
                                    if let Some(bytes) =
                                        mouse_report(32, pane_row, pane_col, true)
                                    {
                                        send_to_pane(&mut state, &mut seqs, &mut wr, bytes)
                                            .await?;
                                    }
                                }
                            }
                            Action::Release { row, col, shift } => {
                                let inside = state.in_pane(row, col);
                                if let Some(selection) = state.selection {
                                    // Copied when the button comes up, with no key to
                                    // press: a selection that has to be confirmed is a
                                    // selection a person makes twice.
                                    // Read from the window as it is drawn, not from the
                                    // live screen: scrolled back, the rows under the
                                    // pointer are rows that went past, and copying the
                                    // live screen instead would hand the person text they
                                    // never saw.
                                    let (pane_cols, pane_rows) = screen::pane_size_for(
                                        state.cols,
                                        state.lines,
                                        state.collapsed,
                                    );
                                    let window: Vec<String> = (0..pane_rows as usize)
                                        .map(|row| {
                                            screen::window_row(
                                                state.snapshot.as_ref(),
                                                &state.behind,
                                                state.scrolled_rows(),
                                                state.anchor.unwrap_or_default(),
                                                row,
                                            )
                                            .0
                                            .to_string()
                                        })
                                        .collect();
                                    let text = selection.text(&window, pane_cols as usize);
                                    if !text.is_empty() {
                                        let mut out = std::io::stdout();
                                        crate::selection::copy(&text, &mut out)?;
                                        state.message = Some(copied(&text));
                                        dirty = true;
                                    } else {
                                        // A click, not a drag. Nothing was selected, so
                                        // nothing stays highlighted.
                                        state.selection = None;
                                    }
                                } else if let Some((pane_row, pane_col)) = inside
                                    && state.pane_wants_mouse()
                                    && !shift
                                    && let Some(bytes) =
                                        mouse_report(0, pane_row, pane_col, false)
                                {
                                    send_to_pane(&mut state, &mut seqs, &mut wr, bytes).await?;
                                }
                            }
                            Action::Wheel { row, col, up, shift } => {
                                let Some((pane_row, pane_col)) = state.in_pane(row, col) else {
                                    continue;
                                };
                                if state.pane_wants_mouse() && !shift {
                                    // 64 up, 65 down: the program scrolls its own view,
                                    // which is the only view of it that exists.
                                    let button = if up { 64 } else { 65 };
                                    if let Some(bytes) =
                                        mouse_report(button, pane_row, pane_col, true)
                                    {
                                        send_to_pane(&mut state, &mut seqs, &mut wr, bytes)
                                            .await?;
                                    }
                                } else if state.pane_is_full_screen() {
                                    // A full screen program that did not ask for the
                                    // mouse still has a list in it to move through, and
                                    // the arrow keys are what every terminal sends one on
                                    // a wheel. Three rows a notch, which is what a
                                    // terminal does and what a person's hand expects.
                                    // The spelling depends on what the program asked
                                    // for: in application cursor mode an arrow is `SS3 A`
                                    // and `CSI A` is not recognised at all. Measured
                                    // against a real `less`, which ignores the second.
                                    let key: &[u8] = match (state.pane_application_cursor(), up) {
                                        (true, true) => b"\x1bOA",
                                        (true, false) => b"\x1bOB",
                                        (false, true) => b"\x1b[A",
                                        (false, false) => b"\x1b[B",
                                    };
                                    let mut bytes = Vec::with_capacity(key.len() * WHEEL_ROWS);
                                    for _ in 0..WHEEL_ROWS {
                                        bytes.extend_from_slice(key);
                                    }
                                    send_to_pane(&mut state, &mut seqs, &mut wr, bytes).await?;
                                } else {
                                    // A shell on the ordinary screen. Arrow keys here
                                    // would walk the command history, so the wheel moves
                                    // this client's own view back through what has
                                    // scrolled past instead.
                                    let (_, pane_rows) = screen::pane_size_for(
                                        state.cols,
                                        state.lines,
                                        state.collapsed,
                                    );
                                    // The row the top of the pane is pinned to. Moving
                                    // the wheel moves the pin, and everything else about
                                    // the view follows from where it is.
                                    let total = state.live_behind();
                                    let at = state.anchor.unwrap_or(total);
                                    let step = WHEEL_ROWS as u64;
                                    let next = if up {
                                        at.saturating_sub(step)
                                    } else {
                                        (at + step).min(total)
                                    };
                                    // Back at the newest row is not scrolled at all, and
                                    // is recorded as such: it is the state a key press
                                    // returns to and the state the pane is drawn in.
                                    let next = (next < total).then_some(next);
                                    if next != state.anchor {
                                        state.anchor = next;
                                        dirty = true;
                                    }
                                    // Asked for whether or not the pin moved: the first
                                    // wheel of all is the one that discovers what is
                                    // there to be read.
                                    if state.focused != 0 {
                                        let want = behind_request(
                                            state.focused,
                                            state.rows_back() as usize,
                                            pane_rows as usize,
                                        );
                                        wr.write_all(&client::encode_control_seq(
                                            &want,
                                            &mut control_seq,
                                        )?)
                                        .await?;
                                    }
                                }
                            }
                            Action::Redraw => dirty = true,
                            Action::Next | Action::Previous => {
                                let forward = matches!(action, Action::Next);
                                if let Some(next) = state.neighbour(forward)
                                    && let Some(bytes) = focus(
                                        &mut state, next, &mut control_seq, &mut seqs,
                                    )?
                                {
                                    wr.write_all(&bytes).await?;
                                    dirty = true;
                                }
                            }
                            Action::Tab(n) => {
                                // Counted from one, because that is the position on the bar
                                // a person is reading off. A number past the end does
                                // nothing: there is no tab there to go to.
                                if let Some(picked) =
                                    state.tabs().get(n - 1).map(|r| r.terminal)
                                    && let Some(bytes) = focus(
                                        &mut state, picked, &mut control_seq, &mut seqs,
                                    )?
                                {
                                    wr.write_all(&bytes).await?;
                                    dirty = true;
                                }
                            }
                            Action::NextSpace | Action::PreviousSpace => {
                                let forward = matches!(action, Action::NextSpace);
                                if let Some(space) = state.next_space(forward) {
                                    if let Some(bytes) = enter_space(
                                        &mut state, space, &mut control_seq, &mut seqs,
                                    )? {
                                        wr.write_all(&bytes).await?;
                                    }
                                    dirty = true;
                                }
                            }
                            Action::Create => {
                                state.prompt = Some(Prompt {
                                    what: Naming::Terminal,
                                    text: String::new(),
                                });
                                dirty = true;
                            }
                            Action::CreateSpace => {
                                state.prompt = Some(Prompt {
                                    what: Naming::Space,
                                    text: String::new(),
                                });
                                dirty = true;
                            }
                            // Prefilled with the name it has, because a rename is usually
                            // an edit and starting from empty makes the common case retype
                            // what was already right.
                            Action::Rename if state.focused != 0 => {
                                let current = state
                                    .rows
                                    .iter()
                                    .find(|r| r.terminal == state.focused)
                                    .and_then(|r| r.name.clone())
                                    .unwrap_or_default();
                                state.prompt = Some(Prompt {
                                    what: Naming::RenameTerminal(state.focused),
                                    text: current,
                                });
                                dirty = true;
                            }
                            Action::Rename => {}
                            Action::Menu => {
                                state.menu = Some(0);
                                dirty = true;
                            }
                            Action::Collapse => {
                                state.collapsed = !state.collapsed;
                                // The pane's width changed with the column, and a child
                                // that is not told reflows its output against a width it no
                                // longer has.
                                let (cols, rows) = screen::pane_size_for(
                                    state.cols,
                                    state.lines,
                                    state.collapsed,
                                );
                                let focused = state.focused;
                                if focused != 0 {
                                    wr.write_all(&client::encode_command_to(
                                        focused,
                                        &ClientWire::Resize { cols, rows },
                                        seqs.entry(focused).or_default(),
                                    )?).await?;
                                }
                                dirty = true;
                            }
                            Action::RenameSpace => {
                                if let Some(space) = state.space {
                                    let current = state
                                        .spaces
                                        .iter()
                                        .find(|s| s.id == space)
                                        .map(|s| s.name.clone())
                                        .unwrap_or_default();
                                    state.prompt = Some(Prompt {
                                        what: Naming::RenameSpace(space),
                                        text: current,
                                    });
                                    dirty = true;
                                }
                            }

                            Action::Close if state.focused == 0 => {}
                            Action::Close => {
                                request += 1;
                                wr.write_all(&client::encode_control_seq(
                                    &ControlRequest::CloseTerminal {
                                        id: format!("close-{request}"),
                                        terminal: state.focused,
                                    },
                                    &mut control_seq,
                                )?).await?;
                            }
                            Action::Palette => {
                                state.palette = Some(Palette::open());
                                dirty = true;
                            }
                            Action::FoldAll => {
                                state.fold.toggle_all();
                                dirty = true;
                            }
                        }
                    }
                    }
                }
                n = rd.read(&mut read) => {
                    let n = n?;
                    if n == 0 {
                        return Ok(());
                    }
                    inbuf.extend_from_slice(&read[..n]);
                    let mut refresh = false;
                    // Separate from the index on purpose. A space list is one SQLite query
                    // behind the daemon's shared store lock, run inline in the connection's
                    // read loop, and asking for one every time an agent changed state put a
                    // query behind every keystroke on a busy daemon. The spaces themselves
                    // only change when a terminal or a workspace does.
                    let mut refresh_spaces = false;
                    for message in connection.drain(&mut inbuf)? {
                        match message {
                            Incoming::Render { terminal, snapshot } => {
                                // The very first render adopts its terminal, which is how
                                // the interface has something on screen before any answer
                                // has arrived. Once. After that, nothing focused means
                                // nothing focused: a render arriving from a subscription
                                // this client has just dropped would otherwise paint
                                // another space's terminal under this space's tab bar, and
                                // silently put the keyboard back in it.
                                if terminal == state.focused
                                    || (state.focused == 0 && !state.adopted)
                                {
                                    state.adopted = true;
                                    state.focused = terminal;
                                    state.snapshot = Some(snapshot);
                                    state.message = None;
                                    dirty = true;
                                }
                            }
                            Incoming::Event(EventMessage::AgentChanged { .. }) => {
                                // What changed is not in the message on purpose, so the
                                // answer comes from the one place that has it.
                                refresh = true;
                            }
                            Incoming::Control(response) => {
                                // Only the answers that change what terminals exist ask for
                                // a new index. Asking after every answer includes asking
                                // after the index itself, which is a request per response
                                // forever: measured, the interface spent 82 percent of a
                                // core repainting an idle screen, and gates G1.20, G1.8 and
                                // G1.9 all went red at once.
                                refresh |= matches!(
                                    response,
                                    ControlResponse::TerminalCreated { .. }
                                        | ControlResponse::TerminalClosed { .. }
                                        | ControlResponse::WorkspaceCreated { .. }
                                        | ControlResponse::Renamed { .. }
                                );
                                refresh_spaces |= matches!(
                                    response,
                                    ControlResponse::TerminalCreated { .. }
                                        | ControlResponse::TerminalClosed { .. }
                                        | ControlResponse::WorkspaceCreated { .. }
                                        // A renamed space is a changed space list, and
                                        // nothing else will say so.
                                        | ControlResponse::Renamed { .. }
                                );
                                apply(&mut state, response, &mut dirty);
                            }
                        }
                    }
                    if let Some(ack) = connection.renders().due_ack(std::time::Instant::now()) {
                        wr.write_all(&ack).await?;
                    }
                    for space in std::mem::take(&mut state.wants_terminal_in) {
                        request += 1;
                        wr.write_all(&client::encode_control_seq(
                            &ControlRequest::CreateTerminal {
                                id: format!("new-{request}"),
                                cwd: None,
                                workspace: Some(space),
                                // The terminal a new space comes up with, which nobody
                                // was asked about: they named the space.
                                name: None,
                            },
                            &mut control_seq,
                        )?).await?;
                    }
                    if let Some(next) = state.pending_focus.take()
                        && let Some(bytes) =
                            focus(&mut state, next, &mut control_seq, &mut seqs)?
                    {
                        wr.write_all(&bytes).await?;
                        dirty = true;
                    }
                    if state.done {
                        return Ok(());
                    }
                    if refresh {
                        request += 1;
                        wr.write_all(&client::encode_control_seq(
                            &ControlRequest::ListAgents { id: format!("index-{request}") },
                            &mut control_seq,
                        )?).await?;
                    }
                    if refresh_spaces {
                        request += 1;
                        wr.write_all(&client::encode_control_seq(
                            &ControlRequest::ListWorkspaces { id: format!("spaces-{request}") },
                            &mut control_seq,
                        )?).await?;
                    }
                }
            }
        }
    }
    .await;

    // Off in the reverse order, and before leaving the alternate screen: a terminal left
    // in mouse reporting mode sends escape sequences into whatever the person runs next.
    let _ = out.write_all(b"\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?1049l");
    let _ = out.flush();
    outcome
}

/// Move the keyboard to another terminal: unsubscribe, subscribe, and size it to the pane,
/// all in one write.
///
/// Decide what one read of stdin meant while the palette owns the keyboard.
///
/// Typing filters, `enter` picks the selected row, `esc` is back, `up` and `down`
/// move, backspace rubs out. A mouse report is swallowed: clicks do nothing while
/// the palette is open rather than reaching a terminal nobody is looking at. A
/// prefix or a bare detach closes the palette and hands the bytes from there on
/// back to the key decoder, so `ctrl-a d` still detaches.
///
/// Closes the palette itself on [`PaletteOutcome::Closed`],
/// [`PaletteOutcome::Confirm`] and [`PaletteOutcome::Passthrough`], so the caller
/// never has to remember to.
///
/// A trailing escape is a cancel rather than the start of a split sequence. Arrow
/// keys arrive atomically in practice, and holding a lone `esc` for a next read
/// that may never come would swallow the one key that closes the palette.
fn palette_key(state: &mut State, bytes: &[u8]) -> PaletteOutcome {
    let mut buf = std::mem::take(&mut state.palette_pending);
    buf.extend_from_slice(bytes);
    let Some(palette) = state.palette.as_mut() else {
        return PaletteOutcome::Closed;
    };
    let close = |state: &mut State| {
        state.palette = None;
        state.palette_pending.clear();
    };
    let mut i = 0;
    while i < buf.len() {
        match buf[i] {
            0x1b => match buf.get(i + 1) {
                Some(b'[') => match buf.get(i + 2) {
                    Some(b'A') => {
                        let count = palette.items(&state.rows, &state.spaces).len();
                        palette.move_by(-1, count);
                        i += 3;
                    }
                    Some(b'B') => {
                        let count = palette.items(&state.rows, &state.spaces).len();
                        palette.move_by(1, count);
                        i += 3;
                    }
                    Some(b'<') => {
                        // A mouse report, swallowed whole. Split across the read, it
                        // is stashed like the key decoder stashes its own.
                        if let Some(end) = buf[i..].iter().position(|b| *b == b'M' || *b == b'm') {
                            i += end + 1;
                        } else {
                            state.palette_pending = buf[i..].to_vec();
                            break;
                        }
                    }
                    // Any other sequence (arrows with modifiers, function keys):
                    // nothing to do with it here, and it must not reach a shell.
                    Some(_) => i += 3,
                    // Split mid sequence: hold it for the next read.
                    None => {
                        state.palette_pending = buf[i..].to_vec();
                        break;
                    }
                },
                // `esc` then anything else, or a lone `esc`: back out.
                Some(_) | None => {
                    close(state);
                    return PaletteOutcome::Closed;
                }
            },
            b'\r' | b'\n' => {
                if let Some(item) = palette.picked(&state.rows, &state.spaces) {
                    close(state);
                    return PaletteOutcome::Confirm(item);
                }
                // Nothing matches: stay open rather than going nowhere.
                i += 1;
            }
            0x7f | 0x08 => {
                palette.backspace();
                i += 1;
            }
            0x01 => {
                // `ctrl-a space` toggles the palette shut. Any other command after
                // the prefix, and a prefix split across the read, belong to the
                // key decoder.
                if buf.get(i + 1) == Some(&b' ') {
                    close(state);
                    return PaletteOutcome::Closed;
                } else if i + 1 >= buf.len() {
                    state.palette_pending = buf[i..].to_vec();
                    break;
                } else {
                    close(state);
                    return PaletteOutcome::Passthrough(buf[i..].to_vec());
                }
            }
            0x11 => {
                close(state);
                return PaletteOutcome::Passthrough(buf[i..].to_vec());
            }
            0x20..=0x7e => {
                palette.push(buf[i] as char);
                i += 1;
            }
            b if b >= 0x80 => {
                let mut j = i;
                while j < buf.len() && buf[j] >= 0x80 {
                    j += 1;
                }
                match std::str::from_utf8(&buf[i..j]) {
                    Ok(text) => {
                        for c in text.chars() {
                            palette.push(c);
                        }
                        i = j;
                    }
                    Err(error) => {
                        let valid = error.valid_up_to();
                        if valid > 0
                            && let Ok(text) = std::str::from_utf8(&buf[i..i + valid])
                        {
                            for c in text.chars() {
                                palette.push(c);
                            }
                        }
                        match error.error_len() {
                            // Invalid bytes: dropped, like a mistyped command.
                            Some(len) => i += valid + len,
                            // Cut mid character: held for the next read.
                            None => {
                                state.palette_pending = buf[i + valid..].to_vec();
                                break;
                            }
                        }
                    }
                }
            }
            // Any other control byte: swallowed. Passing it through would send it
            // to a shell, where a slip of the finger could run something.
            _ => i += 1,
        }
    }
    // Consumed either way, and a repaint either way: the query, the selection or
    // nothing at all moved, and replaying bytes at a shell is how a keystroke
    // appears where nobody sent it.
    PaletteOutcome::Updated
}

/// `None` when there is nothing to do, so a click on the tab a person is already on does
/// not throw away the screen they are looking at and ask the daemon to redraw it.
///
/// One helper and not the same nine lines at each of the four places that move the focus.
/// Those four drifted apart once already: a click cleared the message and a key binding did
/// not, so the same move left different text on the status line depending on how it was
/// made.
fn focus(
    state: &mut State,
    to: u32,
    control_seq: &mut u32,
    seqs: &mut std::collections::BTreeMap<u32, u32>,
) -> io::Result<Option<BytesMut>> {
    if to == 0 || to == state.focused {
        return Ok(None);
    }
    // Another terminal's screen, and another terminal's history. Both are dropped rather
    // than carried, or the new tab opens scrolled into rows that are not its own.
    state.anchor = None;
    state.behind = Behind::default();
    state.selection = None;
    let mut out = switch(state.focused, to, control_seq)?;
    let (pane_cols, pane_rows) = screen::pane_size(state.cols, state.lines);
    out.extend_from_slice(&client::encode_command_to(
        to,
        &ClientWire::Resize {
            cols: pane_cols,
            rows: pane_rows,
        },
        seqs.entry(to).or_default(),
    )?);
    state.focused = to;
    // Dropped rather than kept: it belongs to the terminal being left, and painting it
    // under the new tab would show a person the wrong screen until the first render
    // arrives.
    state.snapshot = None;
    state.message = None;
    // The space follows the terminal. A click in the agents panel can land on an agent in
    // another space, which is the point of that panel crossing spaces, and leaving the bar
    // on the old space would show tabs that do not contain the focused one.
    state.space = state
        .rows
        .iter()
        .find(|r| r.terminal == to)
        .map(|r| r.workspace)
        .or(state.space);
    Ok(Some(out))
}

/// Put a space on screen, and the keyboard in one of its terminals.
///
/// A space with nothing in it is still entered. It is how a person gets somewhere to press
/// `ctrl-a c`, and refusing to go there would make an empty space unreachable, which is the
/// one state every new space starts in.
fn enter_space(
    state: &mut State,
    space: i64,
    control_seq: &mut u32,
    seqs: &mut std::collections::BTreeMap<u32, u32>,
) -> io::Result<Option<BytesMut>> {
    state.space = Some(space);
    match state.tabs().first().map(|r| r.terminal) {
        Some(first) => focus(state, first, control_seq, seqs),
        None => unfocus(
            state,
            "this space is empty, ctrl-a c starts a terminal",
            control_seq,
        ),
    }
}

/// Take the keyboard out of every terminal, because this space has none to put it in.
///
/// The unsubscribe is the point. Leaving the old terminal focused kept every keystroke
/// going to a terminal in a space the person had left and was not looking at, kept its
/// output painting under this space's tab bar, and left `ctrl-a x` closing it. Nothing on
/// screen said any of that was happening.
fn unfocus(state: &mut State, why: &str, control_seq: &mut u32) -> io::Result<Option<BytesMut>> {
    let was = std::mem::replace(&mut state.focused, 0);
    state.snapshot = None;
    state.message = Some(why.to_string());
    if was == 0 {
        return Ok(None);
    }
    Ok(Some(client::encode_control_seq(
        &ControlRequest::Unsubscribe {
            id: format!("hide-{was}"),
            terminals: vec![was],
        },
        control_seq,
    )?))
}

/// Stop watching one terminal and start watching another, in one write.
///
/// Both halves together, because a client that subscribes before it unsubscribes is a
/// client that receives renders for a terminal nobody is looking at, and hidden output not
/// costing anything is the whole point of the subscription.
fn switch(from: u32, to: u32, control_seq: &mut u32) -> io::Result<BytesMut> {
    // Zero is not a terminal, it is this client having nothing on screen, so there is
    // nothing to stop watching.
    let mut out = if from == 0 {
        BytesMut::new()
    } else {
        client::encode_control_seq(
            &ControlRequest::Unsubscribe {
                id: format!("hide-{from}"),
                terminals: vec![from],
            },
            control_seq,
        )?
    };
    out.extend_from_slice(&client::encode_control_seq(
        &ControlRequest::Subscribe {
            id: format!("show-{to}"),
            terminals: vec![to],
        },
        control_seq,
    )?);
    Ok(out)
}

/// Fold one answer into what is on screen.
fn apply(state: &mut State, response: ControlResponse, dirty: &mut bool) {
    match response {
        ControlResponse::Scrollback {
            terminal,
            from,
            total,
            lines,
            ..
        } => {
            // Answers for a terminal this client has since left are dropped rather than
            // kept: painting them would put another terminal's output under this one's
            // tab bar.
            if terminal == state.focused {
                state.behind = Behind {
                    terminal,
                    from,
                    lines,
                    total,
                };
                // What there is may be less than what was asked for, so the view is
                // pulled back to what exists rather than showing blank rows above the
                // oldest line.
                // The pin is deliberately not moved to meet the answer. `from` is where
                // this answer happens to start, not the oldest row there is, and a person
                // turning the wheel quickly has several requests out at once: pulling the
                // pin up to a shallow answer's `from` made the view jump forward and then
                // back as the deeper answer landed. A row the client does not hold is
                // drawn blank for a frame instead, and the next answer fills it.
                let _ = from;
                *dirty = true;
            }
        }
        ControlResponse::Workspaces { workspaces, .. } => {
            if state.space.is_none() {
                state.space = workspaces.first().map(|w| w.id);
            }
            state.spaces = workspaces;
            *dirty = true;
        }
        ControlResponse::WorkspaceCreated { workspace, .. } => {
            state.creating = state.creating.saturating_sub(1);
            state.space = Some(workspace);
            state.snapshot = None;
            // Asked for here rather than by the caller, because a space with no terminal in
            // it shows an empty pane and a bar with only a `+` on it, and the first thing
            // anybody does in a new space is start a terminal.
            state.wants_terminal_in.push(workspace);
            *dirty = true;
        }
        ControlResponse::Agents { agents, .. } => {
            if state.focused == 0 && !state.adopted {
                // Only before this client has ever had a terminal. The daemon subscribes a
                // fresh client to what is already open, so this is the bootstrap and no
                // subscription of our own is needed. Later, nothing focused is a space with
                // nothing in it, and reaching into another space's terminal here is how a
                // person ends up typing somewhere they cannot see.
                if let Some(first) = agents
                    .iter()
                    .find(|r| state.space.is_none_or(|s| r.workspace == s))
                {
                    state.adopted = true;
                    state.focused = first.terminal;
                }
            }
            // A terminal can go away without this client having asked: its shell exited, or
            // somebody closed it from another client. The daemon says so by changing what
            // the index holds and in no other way, so this is the only place that can catch
            // the keyboard up with it. Measured before this: typing `exit` took the tab off
            // the bar and left the focus on a terminal that no longer existed, with the dead
            // screen still painted and every keystroke going nowhere.
            if state.focused != 0 && !agents.iter().any(|r| r.terminal == state.focused) {
                state.focused = 0;
                state.snapshot = None;
                state.pending_focus = agents
                    .iter()
                    .find(|r| state.space.is_none_or(|s| r.workspace == s))
                    .map(|r| r.terminal);
                if state.pending_focus.is_none() {
                    state.message =
                        Some("this space is empty, ctrl-a c starts a terminal".to_string());
                }
            }
            // Nothing left anywhere. The interface is a way into terminals, so with none of
            // them it is a screen a person has to know a key to escape from. Ending is what
            // every multiplexer does when its last window closes, and the daemon stays: it
            // is the client that goes.
            state.done = state.adopted && agents.is_empty();

            // A terminal that has just started needing a person. The bell is the local
            // notification: it is what the person's own terminal emulator turns into a
            // badge or a bounce, and it is the one channel that works with no account, no
            // daemon of somebody else's, and no network.
            for row in &agents {
                let before = state.was.get(&row.terminal);
                let wants_someone = matches!(row.state.as_str(), "blocked" | "failed" | "done");
                if wants_someone && before.is_some_and(|b| b != &row.state) {
                    state.ring = true;
                    state.message = Some(format!("terminal {} is {}", row.terminal, row.state));
                }
            }
            state.was = agents
                .iter()
                .map(|r| (r.terminal, r.state.clone()))
                .collect();
            state.rows = agents;
            *dirty = true;
        }
        ControlResponse::TerminalCreated { terminal, .. } => {
            // The caller subscribes: a new terminal that the client is not watching draws
            // the terminal it came from, which is what the first version did.
            state.pending_focus = Some(terminal);
            state.snapshot = None;
            *dirty = true;
        }
        ControlResponse::TerminalClosed { terminal, .. } => {
            if terminal == state.focused {
                state.snapshot = None;
                // From this space, and through the subscription path, which is why it is a
                // pending focus rather than an assignment. Picking any terminal from
                // `rows` put the keyboard in another space without moving the tab bar
                // there, and assigning it directly left the client unsubscribed, so the
                // pane stayed empty until something else happened to move the focus.
                state.focused = 0;
                state.pending_focus = state
                    .rows
                    .iter()
                    .find(|r| {
                        r.terminal != terminal && state.space.is_none_or(|s| r.workspace == s)
                    })
                    .map(|r| r.terminal);
                if state.pending_focus.is_none() {
                    state.message =
                        Some("this space is empty, ctrl-a c starts a terminal".to_string());
                }
            }
            *dirty = true;
        }
        ControlResponse::Failed { reason, .. } => {
            // Shown rather than swallowed. A refusal a person cannot see is a client that
            // looks broken instead of a daemon that said no.
            state.message = Some(reason);
            *dirty = true;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qoro_runtime::control::WorkspaceRow;

    fn space(id: i64, name: &str) -> WorkspaceRow {
        WorkspaceRow {
            id,
            name: name.to_string(),
            terminals: 1,
        }
    }

    fn agent(terminal: u32, workspace: i64, program: &str, state: &str) -> AgentRow {
        AgentRow {
            terminal,
            name: None,
            workspace,
            agent: true,
            program: Some(program.to_string()),
            behind: None,
            state: state.to_string(),
            generation: None,
        }
    }

    fn state() -> State {
        State {
            rows: vec![
                agent(1, 1, "codex", "blocked"),
                agent(2, 2, "claude", "working"),
            ],
            focused: 2,
            snapshot: None,
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            pending_focus: None,
            spaces: vec![space(1, "app"), space(2, "webshop")],
            space: Some(2),
            was: std::collections::BTreeMap::new(),
            ring: false,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            wants_terminal_in: Vec::new(),
            adopted: true,
            creating: 0,
            done: false,
            fold: sidebar::Fold::default(),
            palette: Some(Palette::open()),
            palette_pending: Vec::new(),
            selection: None,
            anchor: None,
            behind: Behind::default(),
        }
    }

    #[test]
    fn typing_filters_and_enter_picks_the_top_match() {
        let mut state = state();
        assert!(matches!(
            palette_key(&mut state, b"blocked"),
            PaletteOutcome::Updated
        ));
        assert_eq!(
            state.palette.as_ref().map(|p| p.query.as_str()),
            Some("blocked")
        );
        assert!(matches!(
            palette_key(&mut state, b"\r"),
            PaletteOutcome::Confirm(palette::Item::Terminal(1))
        ));
        assert_eq!(state.palette, None, "confirming leaves the palette");
    }

    #[test]
    fn esc_closes_and_backspace_rubs_out() {
        let mut state = state();
        assert!(matches!(
            palette_key(&mut state, b"mar"),
            PaletteOutcome::Updated
        ));
        assert!(matches!(
            palette_key(&mut state, &[0x7f]),
            PaletteOutcome::Updated
        ));
        assert_eq!(state.palette.as_ref().map(|p| p.query.as_str()), Some("ma"));
        assert!(matches!(
            palette_key(&mut state, b"\x1b"),
            PaletteOutcome::Closed
        ));
        assert_eq!(state.palette, None);
    }

    #[test]
    fn arrows_move_the_selection() {
        let mut state = state();
        assert!(matches!(
            palette_key(&mut state, b"\x1b[B"),
            PaletteOutcome::Updated
        ));
        assert_eq!(state.palette.as_ref().map(|p| p.selected), Some(1));
        assert!(matches!(
            palette_key(&mut state, b"\x1b[A"),
            PaletteOutcome::Updated
        ));
        assert_eq!(state.palette.as_ref().map(|p| p.selected), Some(0));
    }

    #[test]
    fn a_prefix_hands_control_back_to_the_key_decoder() {
        let mut state = state();
        match palette_key(&mut state, b"\x01d") {
            PaletteOutcome::Passthrough(rest) => assert_eq!(rest, b"\x01d"),
            other => panic!("a detach closed nothing: {other:?}"),
        }
        assert_eq!(state.palette, None);
    }

    #[test]
    fn ctrl_a_space_toggles_the_palette_shut() {
        let mut state = state();
        assert!(matches!(
            palette_key(&mut state, b"\x01 "),
            PaletteOutcome::Closed
        ));
        assert_eq!(state.palette, None);
    }

    #[test]
    fn enter_with_no_match_stays_open() {
        let mut state = state();
        assert!(matches!(
            palette_key(&mut state, b"zzz-no-such-thing\r"),
            PaletteOutcome::Updated
        ));
        assert!(state.palette.is_some());
    }

    /// What is asked for has to cover what the window will read, or the rows come back
    /// blank: the first version asked for a range on the wrong side of the window and
    /// every scrolled row was empty.
    #[test]
    fn the_rows_asked_for_cover_the_rows_the_window_will_read() {
        // A small window, and one tall enough that a margin no longer fits inside what the
        // daemon will answer with: a 4K screen at a small font is three hundred rows.
        for (pane_rows, scroll) in [20usize, 300]
            .into_iter()
            .flat_map(|rows| [1usize, 3, 19, 20, 21, 100, 999].map(move |s| (rows, s)))
        {
            let ControlRequest::Scrollback { up, rows, .. } = behind_request(1, scroll, pane_rows)
            else {
                panic!("a scrollback request");
            };
            // What the painter reads, in rows back from the newest: the top of the window
            // is `scroll` back, and it reads down from there for as much of the window as
            // is history rather than screen.
            let newest_needed = scroll.saturating_sub(pane_rows.min(scroll)) + 1;
            let oldest_needed = scroll;
            // What the request covers, in the same terms.
            let covers_oldest = up;
            let covers_newest = up.saturating_sub(rows) + 1;
            assert!(
                covers_oldest >= oldest_needed,
                "at {scroll} back the request stops short of the top of the window: \
                 covers {covers_oldest} back, needs {oldest_needed}"
            );
            assert!(
                covers_newest <= newest_needed,
                "at {scroll} back the request stops short of the bottom of the window: \
                 covers down to {covers_newest} back, needs {newest_needed}"
            );
            // And it is bounded by what the daemon will answer with. Past that ceiling
            // the daemon keeps the oldest end of the range, so the rows next to the
            // screen, which are the ones the window actually needs, are the ones dropped.
            assert!(
                rows <= qoro_runtime::daemon::MAX_SCROLLBACK_ROWS,
                "{rows} rows asked for at a {pane_rows} row window, past the ceiling"
            );
        }
    }

    /// A row of the history is found by its own number, and one that was never fetched is
    /// absent rather than another row.
    #[test]
    fn the_rows_behind_are_addressed_by_their_own_number() {
        let behind = Behind {
            terminal: 1,
            from: 90,
            lines: vec!["a".into(), "b".into(), "c".into()],
            total: 93,
        };
        assert_eq!(behind.row(90), Some("a"));
        assert_eq!(behind.row(92), Some("c"));
        assert_eq!(behind.row(89), None, "a row from before what was fetched");
        assert_eq!(behind.row(93), None, "a row from after it");
        assert_eq!(Behind::default().row(0), None);
    }

    /// The view is pinned to a row, so output arriving does not drag a reader down the
    /// screen.
    ///
    /// The first version kept a distance from the bottom instead, which is a distance from
    /// something that moves: while an agent printed, every new row shifted the text under
    /// the person's eyes by one.
    #[test]
    fn a_reader_holds_their_place_while_the_terminal_keeps_printing() {
        let mut state = bare_state();
        state.cols = 80;
        state.lines = 26;
        let (_, pane_rows) = crate::screen::pane_size_for(state.cols, state.lines, false);
        let pane_rows = pane_rows as u64;

        // A hundred rows behind the screen, and the person pins the pane ten back.
        state.snapshot = Some(snapshot_with_behind(100));
        state.anchor = Some(90);
        assert_eq!(state.rows_back(), 10);
        assert_eq!(state.scrolled_rows(), 10);

        // Twenty more rows are printed while they read. The pin does not move, so the same
        // rows are still under their eyes; what changes is how much of the pane they fill.
        state.snapshot = Some(snapshot_with_behind(120));
        assert_eq!(state.anchor, Some(90), "the pin moved");
        assert_eq!(state.rows_back(), 30);
        assert_eq!(
            state.scrolled_rows(),
            30.min(pane_rows) as usize,
            "the rows read from behind the screen did not follow the output"
        );

        // Enough output and the whole pane is rows that went past, and no more than that.
        state.snapshot = Some(snapshot_with_behind(10_000));
        assert_eq!(state.scrolled_rows(), pane_rows as usize);

        // At the bottom there is no pin and nothing is read from behind at all.
        state.anchor = None;
        assert_eq!(state.scrolled_rows(), 0);
        assert_eq!(state.rows_back(), 0);
    }

    /// A state with nothing in it, for the arithmetic above.
    fn bare_state() -> State {
        State {
            rows: Vec::new(),
            focused: 1,
            snapshot: None,
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            pending_focus: None,
            spaces: Vec::new(),
            space: None,
            was: Default::default(),
            ring: false,
            wants_terminal_in: Vec::new(),
            adopted: true,
            done: false,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            creating: 0,
            fold: sidebar::Fold::default(),
            palette: None,
            palette_pending: Vec::new(),
            selection: None,
            anchor: None,
            behind: Behind::default(),
        }
    }

    fn snapshot_with_behind(behind: u64) -> SnapshotWire {
        SnapshotWire {
            cols: 80,
            rows: 24,
            cursor: (0, 0),
            lines: Vec::new(),
            styles: Vec::new(),
            modes: Default::default(),
            behind,
        }
    }
}
