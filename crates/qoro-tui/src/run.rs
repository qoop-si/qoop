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
use qoro_runtime::daemon::{ClientWire, SnapshotWire, StoredImage};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::keys::{Action, Keys};
use crate::layout::{self, Layout};
use crate::palette::{self, Palette};
use crate::screen::{self, Frame};
use crate::{present, sidebar, tabs};

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

/// What to say when something has been copied, given how it left.
///
/// A system clipboard write is a promise kept by this machine, so it says
/// "copied". An OSC 52 escape is a request to the terminal in front of the
/// person, which that terminal may swallow: saying "copied" for one is how a
/// person ends up pasting nothing. The notice names the path instead.
fn copied(text: &str, via: crate::selection::CopyVia) -> String {
    let what = short_copied(text);
    match via {
        crate::selection::CopyVia::System => format!("copied {what}"),
        crate::selection::CopyVia::TerminalEscape => {
            format!("sent to terminal clipboard ({what})")
        }
    }
}

/// The short form of what was copied: a line count, or the text itself cut to
/// the notice row.
fn short_copied(text: &str) -> String {
    let lines = text.lines().count();
    if lines > 1 {
        return format!("{lines} lines");
    }
    let one = text.trim();
    // Shown, so a person can see they got the right thing, and cut so a long line does not
    // push the rest of the notice off the row.
    if crate::width::width(one) > 40 {
        format!("{}\u{2026}", crate::selection::in_columns(one, 0..39))
    } else {
        one.to_string()
    }
}

/// Where this interface run starts numbering its requests: nanoseconds
/// since the epoch, so no later run reuses an id an earlier one sent.
fn request_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// The notice a new interface shows until its focused terminal first renders.
const CONNECTING: &str = "connecting";

/// What ctrl-a U says: whether a newer release is waiting, and how it gets installed.
///
/// Said, not fetched: the interface never touches the network, it only reads the notice
/// `qoro update --check` left beside the sessions. The install itself runs outside, with
/// no session live, which is why the answer names detaching first.
fn update_message() -> String {
    match qoro_runtime::paths::read_update_notice() {
        Some(seen) if qoro_runtime::paths::is_newer_version(&seen, env!("CARGO_PKG_VERSION")) => {
            format!("update {seen} is ready: detach and run `qoro update` with no session running")
        }
        _ => format!(
            "qoro {} is current: `qoro update --check` looks for more",
            env!("CARGO_PKG_VERSION")
        ),
    }
}

/// The shortest gap between two frames: sixty a second.
///
/// A streaming agent renders far faster than a person can read, and every frame is work for
/// the terminal above this one (and for a tmux between them). Renders closer together than
/// this are sent as one frame, the newest.
const FRAME: std::time::Duration = std::time::Duration::from_millis(16);

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
        // Local typing is the person's own hands, never a wait edge.
        &ClientWire::Input {
            bytes,
            from_terminal: None,
            from_token: None,
        },
        seqs.entry(focused).or_default(),
    )?;
    wr.write_all(&frame).await
}

struct State {
    rows: Vec<AgentRow>,
    focused: u32,
    snapshot: Option<SnapshotWire>,
    /// Snapshots of the visible but unfocused tiles, by terminal. The focused
    /// terminal lives in `snapshot` (with scrollback and selection); the
    /// others are viewport-cropped live screens.
    snapshots: std::collections::BTreeMap<u32, SnapshotWire>,
    /// The newest image per terminal, by terminal. Drawn by the frame painter
    /// (overlay or placeholder); keyed like `snapshots` so tiles work the same.
    images: std::collections::BTreeMap<u32, StoredImage>,
    /// What the outer terminal can draw, read once at startup. Decides real
    /// picture versus placeholder per frame.
    graphics: crate::graphics::GraphicsCap,
    /// Image ids currently placed on the outer screen, by terminal. Reconciled
    /// per frame: anything here and not desired comes down.
    shown_images: crate::graphics::Shown,
    /// Outer size the placements were made at. A change reflows cell-anchored
    /// overlays out from under the text, so all come down and go back up.
    img_size: (u16, u16),
    /// Terminals this client receives renders for. Always exactly the visible
    /// set of the space on screen: over-subscribing spends daemon and client
    /// work on output nobody sees.
    subs: std::collections::BTreeSet<u32>,
    /// One tiling tree per space. Spaces without an entry show a single pane.
    layouts: std::collections::BTreeMap<i64, Layout>,
    /// Terminals created as split panes, hidden from the top tab bar.
    /// A split is a sub-terminal inside its tab, never a new tab. Agents
    /// in panes still list in the sidebar. Filled on `TerminalCreated`
    /// with a `split-` id, cleared on close/prune/detach. Client-local like
    /// layouts: a second client or a reconnect sees these terminals as
    /// ordinary tabs until splits are made again.
    panes: std::collections::BTreeSet<u32>,
    /// Where each outstanding split was asked for, by request id. A split
    /// answer carries no workspace, and the space on screen may have moved
    /// since the request: without this the bind lands in the wrong tree or
    /// the placeholder leaks and blocks every later split. Consumed by the
    /// `TerminalCreated`/`Failed` answer.
    split_origin: std::collections::BTreeMap<String, i64>,
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
    /// The adopted child still needs sizing to this pane. Set where the
    /// adoption happens without a write in hand (the index path); drained
    /// with the deferred focus everywhere a write is in hand, including the
    /// ack tick, so an idle daemon still sizes it within seconds.
    adopted_resize: bool,
    /// Set when there is nothing left to be an interface to.
    done: bool,
    /// The open naming box, when a person is being asked what to call something.
    prompt: Option<Prompt>,
    /// The open menu, and which of its items is selected.
    menu: Option<usize>,
    /// Whether the left column is folded away to a strip.
    collapsed: bool,
    /// Whether the focus list covers the pane: the last prompt, the last
    /// turns, one row a tool. Client-local like `collapsed`: the daemon
    /// never knows, it only answers `ListFocusEvents`.
    focus: bool,
    /// The focused terminal's hook events, oldest first, as last listed.
    /// Read-only paint input; the daemon owns the log.
    focus_events: Vec<qoro_runtime::control::FocusEvent>,
    /// Rows of the list hidden below the window. Zero pins the tail, so new
    /// events arrive in view without scrolling.
    focus_offset: usize,
    /// A refetch is due: the index answered while open. Open and
    /// terminal-switch fetch on their own bytes instead: on an idle daemon
    /// nothing else arrives to drain a flag on. Consumed by the loop, never
    /// on a timer.
    focus_fetch: bool,
    /// An overlay showing something read only: the keys, or what this daemon is set to.
    reading: Option<Reading>,
    /// Spaces asked for and not yet answered for.
    ///
    /// Only the naming needs this, and the naming needs it badly: two presses of `ctrl-a w`
    /// before either answer came back both counted the same list and both produced
    /// `space 2`, so the panel showed two rows with one name on them.
    creating: usize,
    /// How the agents panel orders its rows: daemon order, or flat by urgency.
    view: sidebar::View,
    /// The done rows this client has already looked at, as (terminal, generation).
    ///
    /// Client-local, like the focus itself: reading a done agent here does not
    /// mark it read anywhere else. A focused done row counts as read without
    /// an entry, so this only holds rows finished while the keyboard was away.
    seen_done: sidebar::Seen,
    /// Rows that finished while the keyboard was away, as (terminal, generation).
    ///
    /// For one event only: a screen agent going working to idle
    /// while unfocused is the same news as a daemon `done`. Derived here from
    /// the transition, cleared on focus, pruned with the rows. Same sage,
    /// same `new` word, no new theme.
    unseen_finished: sidebar::Unseen,
    /// Unused animation step, kept so the frame shape stays stable.
    ///
    /// The panel is static and the tick never advances it; it stays zero so a
    /// still screen never repaints.
    tick: u64,
    /// How wide the open left column paints, clamped to the sidebar range.
    ///
    /// The default is the constant; a drag changes this, arrivals never do.
    /// The collapsed strip keeps its own four columns regardless.
    sidebar_width: usize,
    /// A divider drag in progress: the panel width when the press landed.
    ///
    /// `None` while the mouse does anything else. The width at the press is
    /// what tells a motionless press from a pull on release.
    sidebar_drag: Option<usize>,
    /// The panel width the focused child was last sized to.
    ///
    /// Drag motions repaint every frame but reflow at most once per frame:
    /// the paint block below sends one resize when these two disagree.
    sidebar_sized: usize,
    /// The last frame's clock: hit testing reads this rather than a fresh
    /// one, so a click maps onto the rows the paint drew by construction.
    /// Zero before the first paint, which lands on the first loop turn.
    paint_now: i64,
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

/// Which tile a screen position lands in: its terminal, the pane-global cell,
/// and the tile-local cell. Divider cells answer `None`.
type TileHit = (u32, (usize, usize), (usize, usize));

impl State {
    fn frame(&self) -> Frame<'_> {
        // The pane area in its own cells, shared by every tile rect.
        let (pane_cols, pane_rows) =
            screen::pane_size_for(self.cols, self.lines, self.collapsed, 0, self.sidebar_width);
        let full = layout::Rect {
            x: 0,
            y: 0,
            cols: pane_cols as usize,
            rows: pane_rows as usize,
        };
        let mut tiles = Vec::new();
        if self.focused != 0
            && let Some(space) = self.space
            && let Some(layout) = self.layouts.get(&space)
        {
            for (terminal, rect) in layout.tile_rects(full) {
                let snapshot = if terminal == self.focused {
                    self.snapshot.as_ref()
                } else {
                    self.snapshots.get(&terminal)
                };
                tiles.push(screen::Tile {
                    terminal,
                    rect,
                    snapshot,
                    focused: terminal == self.focused,
                });
            }
        }
        Frame {
            rows: &self.rows,
            spaces: &self.spaces,
            space: self.space,
            panes: &self.panes,
            focused: self.focused,
            snapshot: self.snapshot.as_ref(),
            cols: self.cols,
            lines: self.lines,
            message: self.message.as_deref(),
            hint: self.hint.as_deref(),
            view: self.view,
            seen: &self.seen_done,
            unseen: &self.unseen_finished,
            tick: self.tick,
            sidebar_width: self.sidebar_width,
            // One clock per frame: tiers, order and hit testing below all
            // read the same second.
            now_unix: sidebar::now_unix(),
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
            tiles,
            // Filled by the paint loop when the focused terminal holds a picture
            // the outer terminal cannot draw. `None` here keeps `frame()` pure:
            // capability is a client property, not daemon state.
            image_notice: None,
        }
    }

    /// The tabs of the space on screen, in the order the daemon reports them.
    /// Split panes are sub-terminals inside their tab, never top tabs, so
    /// they are filtered here. Agents in panes still list in the sidebar.
    fn tabs(&self) -> Vec<AgentRow> {
        screen::in_space(&self.rows, self.space)
            .into_iter()
            .filter(|r| !self.panes.contains(&r.terminal))
            .collect()
    }

    /// What a click at a one based screen position means.
    ///
    /// Asked of the same modules that drew the row, because both of them scroll to keep
    /// the focused entry visible: the row a person clicked is not the entry's position in
    /// the list, and computing it here would be a second copy of that arithmetic that
    /// starts out right and drifts.
    /// Where in the focused pane a screen position is, if it is in it at all.
    ///
    /// Zero based tile-local cells, which is what the snapshot's rows and the
    /// selection both count in. Unfocused tiles never take input directly: a
    /// click on one focuses it first, and the keystroke goes to the second
    /// click. That ordering is what keeps a click aimed at one pane from being
    /// typed into another.
    fn in_pane(&self, row: u16, col: u16) -> Option<(usize, usize)> {
        if self.focused == 0 {
            return None;
        }
        let (terminal, _, local) = self.tile_at_screen(row, col)?;
        (terminal == self.focused).then_some(local)
    }

    /// Which visible tile a one based screen position lands in, with tile-local
    /// coordinates. Divider cells answer `None`.
    fn tile_at_screen(&self, row: u16, col: u16) -> Option<TileHit> {
        let (pane_cols, pane_rows) =
            screen::pane_size_for(self.cols, self.lines, self.collapsed, 0, self.sidebar_width);
        let left = screen::left_for(self.cols, self.collapsed, self.sidebar_width);
        let first_col = left + 1;
        if (col as usize) < first_col || (col as usize) >= first_col + pane_cols as usize {
            return None;
        }
        let within = (row as usize).checked_sub(screen::TABS + 1)?;
        if within >= pane_rows as usize {
            return None;
        }
        let x = col as usize - first_col;
        if self.focused == 0 {
            return None;
        }
        if let Some(space) = self.space
            && let Some(layout) = self.layouts.get(&space)
        {
            let area = layout::Rect {
                x: 0,
                y: 0,
                cols: pane_cols as usize,
                rows: pane_rows as usize,
            };
            let hit = layout.hit(area, x, within)?;
            let rect = layout
                .tile_rects(area)
                .into_iter()
                .find(|(t, _)| *t == hit)
                .map(|(_, r)| r)?;
            return Some((hit, (within, x), (within - rect.y, x - rect.x)));
        }
        Some((self.focused, (within, x), (within, x)))
    }

    /// How many of the pane's rows come from behind the screen.
    ///
    /// Derived rather than stored: what is fixed is the row the top of the pane is pinned
    /// to, and how much of the pane that fills changes by itself as the terminal prints.
    fn scrolled_rows(&self) -> usize {
        let (_, pane_rows) =
            screen::pane_size_for(self.cols, self.lines, self.collapsed, 0, self.sidebar_width);
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

    /// Whether a plain press at a one based screen position arms a divider
    /// drag, and arm it.
    ///
    /// One strict bouncer so tests pin it: the open column actually drawn
    /// (not collapsed, not dropped on a narrow window), no overlay eating
    /// the press, exactly the divider column inside the painted rows. The
    /// caller filters button and shift before asking. Arming consumes the
    /// press whole: no `Clicked` runs, including `Collapse` on the foot row
    /// the divider also crosses. A press without motion stays a no-op.
    fn divider_press(&mut self, col: u16, row: u16) -> bool {
        let col = col as usize;
        let row = row as usize;
        let drawn = crate::screen::left_for(self.cols, self.collapsed, self.sidebar_width) > 0;
        let column_rows = (self.lines as usize).saturating_sub(crate::screen::STATUS);
        if self.collapsed
            || !drawn
            || self.menu.is_some()
            || self.prompt.is_some()
            || self.palette.is_some()
            || col != self.sidebar_width + 1
            || row == 0
            || row > column_rows
        {
            return false;
        }
        self.sidebar_drag = Some(self.sidebar_width);
        true
    }

    /// Move an armed drag to a one based screen column. Equal widths and
    /// shift-flagged motions do nothing at all: the drag stays armed, and
    /// the caller paints and reflows only on `true`.
    fn divider_motion(&mut self, col: u16, shift: bool) -> bool {
        if self.sidebar_drag.is_none() || shift {
            return false;
        }
        let new = (col as usize)
            .saturating_sub(1)
            .clamp(sidebar::SIDEBAR_MIN, sidebar::SIDEBAR_MAX);
        if new == self.sidebar_width {
            return false;
        }
        self.sidebar_width = new;
        true
    }

    /// End an armed drag, anywhere. Answers whether the width moved since
    /// the press: only then does the caller owe the anchored view a
    /// scrollback refetch.
    fn finish_drag(&mut self) -> bool {
        match self.sidebar_drag.take() {
            Some(at_press) => self.sidebar_width != at_press,
            None => false,
        }
    }

    fn clicked(&self, row: u16, col: u16) -> Option<Clicked> {
        // The last painted frame's clock, not a fresh one: tiers move rows,
        // and a click maps onto what the paint drew by construction. A click
        // before the first paint reads zero, where no positive stamp is stale.
        let now_unix = self.paint_now;
        let (pane_cols, _) =
            screen::pane_size_for(self.cols, self.lines, self.collapsed, 0, self.sidebar_width);
        let left = screen::left_for(self.cols, self.collapsed, self.sidebar_width);
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
                &self.unseen_finished,
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
                within,
                self.view,
                &self.seen_done,
                &self.unseen_finished,
                // The last painted frame's clock, not a fresh one: tiers move
                // rows, and a click maps onto what the paint drew.
                now_unix,
                self.sidebar_width,
                // The paint reserves art rows above the foot when a pet is
                // picked, so the click maps onto the same layout it drew.
                sidebar::current_avatar().as_ref(),
            )
        };
        // The row between the panels answers by which half was clicked, so the column has
        // to ask a second question the row itself cannot answer.
        // The spaces title's `+` corner asks a second question too: the row
        // itself is a no-op, only the corner cell makes a space.
        let hit = hit
            .or_else(|| {
                (within == 0 && !self.collapsed)
                    .then(|| sidebar::spaces_title_hit(col as usize, self.sidebar_width))?
            })
            .or_else(|| {
                let rows = sidebar::rows_folded(
                    &self.spaces,
                    &self.rows,
                    self.space.unwrap_or_default(),
                    self.focused,
                    column_rows,
                    // Hit testing does not depend on the animation frame.
                    0,
                    self.view,
                    &self.seen_done,
                    &self.unseen_finished,
                    now_unix,
                    self.sidebar_width,
                    sidebar::current_avatar().as_ref(),
                );
                let is_footer = rows
                    .get(within)
                    .is_some_and(|(line, _)| line.text.contains("menu"));
                (is_footer && !self.collapsed)
                    .then(|| sidebar::footer_hit(col as usize, self.sidebar_width))?
            })
            .or_else(|| {
                let rows = sidebar::rows_folded(
                    &self.spaces,
                    &self.rows,
                    self.space.unwrap_or_default(),
                    self.focused,
                    column_rows,
                    // Hit testing does not depend on the animation frame.
                    0,
                    self.view,
                    &self.seen_done,
                    &self.unseen_finished,
                    now_unix,
                    self.sidebar_width,
                    sidebar::current_avatar().as_ref(),
                );
                let is_agents_title = rows
                    .get(within)
                    .is_some_and(|(line, _)| line.text.starts_with(" agents"));
                (is_agents_title && !self.collapsed).then(|| {
                    sidebar::agents_title_hit(self.view, col as usize, self.sidebar_width)
                })?
            });
        match hit? {
            sidebar::Hit::Space(space) => Some(Clicked::Space(space)),
            sidebar::Hit::Terminal(terminal) => Some(Clicked::Terminal(terminal)),
            sidebar::Hit::NewSpace => Some(Clicked::NewSpace),
            sidebar::Hit::Menu => Some(Clicked::Menu),
            sidebar::Hit::View => Some(Clicked::View),
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
/// Up and down move, as do `k` and `j`; enter chooses, esc closes; a row's own
/// `ctrl-a` key jumps to it, and the first letter jumps to a row with no key.
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
                if letter == b'k' {
                    state.menu = Some(selected.saturating_sub(1));
                    return MenuOutcome::Moved;
                }
                if letter == b'j' {
                    state.menu = Some((selected + 1).min(MENU.len() - 1));
                    return MenuOutcome::Moved;
                }
                // A row's own key first, so `r` means rename tab and never
                // reload; then the first letter of a keyless row.
                if let Some(found) = MENU
                    .iter()
                    .position(|(_, key, _)| key.as_bytes().first() == Some(&letter))
                {
                    state.menu = Some(found);
                    return MenuOutcome::Moved;
                }
                if let Some(found) = MENU.iter().position(|(name, key, _)| {
                    key.is_empty() && name.as_bytes().first() == Some(&letter)
                }) {
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
/// one would be right to stop trusting the rest. The key is the `ctrl-a` letter that does
/// the same thing without the menu; empty when the row has no direct key.
pub const MENU: &[(&str, &str, &str)] = &[
    ("new tab", "c", "name it, then it opens"),
    ("new space", "w", "name it, then it opens"),
    ("rename tab", "r", "rename what is focused"),
    ("rename space", "R", "rename what is on screen"),
    ("close tab", "x", "close what is focused"),
    ("split side by side", "v", "a sub-terminal, not a new tab"),
    ("split stacked", "-", "a sub-terminal, not a new tab"),
    ("zoom pane", "z", "fill the pane with this tile, or go back"),
    ("toggle agents view", "g", "agents grouped or by urgency"),
    ("keybinds", "?", "every key, and what it does"),
    ("settings", "", "what this daemon is set to"),
    (
        "reload manifests",
        "",
        "re-read the agent detection overrides",
    ),
    ("detach", "d", "leave, and keep everything running"),
];

/// What choosing a menu row asks the daemon for, if anything.
enum MenuSend {
    Nothing,
    Detach,
    Control(ControlRequest),
    /// Flip zoom on the focused pane. The tree flips and the follow-up bytes
    /// go out at the call sites, because they own the sequence numbers.
    Zoom,
}

/// Where a new terminal starts: the focused terminal's directory, as last reported.
///
/// A new tab opens where the
/// person already is rather than where the daemon started. The daemon cannot do this
/// itself: focus lives in this client, so the row's last known cwd travels on the
/// request. A stale, relative or vanished directory falls back to `None`, which is the
/// daemon's own directory, rather than failing the create.
fn focused_cwd(state: &State) -> Option<String> {
    let cwd = state
        .rows
        .iter()
        .find(|r| r.terminal == state.focused)?
        .cwd
        .clone()?;
    // A remote terminal's directory is on its own machine, so this one cannot check it;
    // the mux carries it only to that same machine.
    let here = crate::mux::is_remote_terminal(state.focused) || std::path::Path::new(&cwd).is_dir();
    (!cwd.is_empty() && std::path::Path::new(&cwd).is_absolute() && here).then_some(cwd)
}

/// The numbered name a space gets when nobody names it.
///
/// Counted among the spaces of the machine on screen, and on a remote prefixed with its
/// name, which is also what makes the new space over there: `ctrl-a w` in `devbox:api`
/// makes `devbox:space 3`, not a local space.
fn next_space_name(state: &State) -> String {
    let remote = state
        .space
        .filter(|s| crate::mux::is_remote_workspace(*s))
        .and_then(|s| state.spaces.iter().find(|w| w.id == s))
        .and_then(|w| w.name.split_once(':'))
        .map(|(prefix, _)| prefix.to_string());
    let same = |w: &&qoro_runtime::control::WorkspaceRow| match &remote {
        Some(prefix) => {
            crate::mux::is_remote_workspace(w.id)
                && w.name.split_once(':').is_some_and(|(p, _)| p == prefix)
        }
        None => !crate::mux::is_remote_workspace(w.id),
    };
    let n = state.spaces.iter().filter(same).count() + state.creating + 1;
    match remote {
        Some(prefix) => format!("{prefix}:space {n}"),
        None => format!("space {n}"),
    }
}

/// Run one menu row: prompts, messages and state changes happen here, and the
/// caller only sends the returned request. One function for the keyboard and
/// the mouse, so the two cannot disagree about what a row does.
fn apply_menu_choice(state: &mut State, item: usize, request: &mut u64) -> MenuSend {
    match MENU[item].0 {
        "new tab" => {
            state.prompt = Some(Prompt {
                what: Naming::Terminal,
                text: String::new(),
            });
            MenuSend::Nothing
        }
        "new space" => {
            state.prompt = Some(Prompt {
                what: Naming::Space,
                text: String::new(),
            });
            MenuSend::Nothing
        }
        "rename tab" => {
            if state.focused != 0 {
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
            } else {
                state.message = Some("nothing to rename, ctrl-a c starts a terminal".to_string());
            }
            MenuSend::Nothing
        }
        "rename space" => {
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
            } else {
                state.message = Some("nothing to rename yet".to_string());
            }
            MenuSend::Nothing
        }
        "close tab" => {
            if state.focused == 0 {
                state.message = Some("nothing to close".to_string());
                MenuSend::Nothing
            } else {
                *request += 1;
                MenuSend::Control(ControlRequest::CloseTerminal {
                    id: format!("close-{request}"),
                    terminal: state.focused,
                })
            }
        }
        "split side by side" | "split stacked" => {
            let dir = if MENU[item].0 == "split side by side" {
                layout::Dir::V
            } else {
                layout::Dir::H
            };
            let focused = state.focused;
            let space = state.space;
            if focused == 0 {
                state.message = Some("nothing to split, pick new tab instead".to_string());
                MenuSend::Nothing
            } else {
                ensure_layout(state);
                let area = pane_area(state);
                let placed = space
                    .and_then(|s| state.layouts.get_mut(&s))
                    .is_some_and(|layout| layout.split_placeholder(focused, dir, area));
                if !placed {
                    state.message = Some(
                        "cannot split now: too small, zoomed, or one already on its way"
                            .to_string(),
                    );
                    MenuSend::Nothing
                } else {
                    *request += 1;
                    if let Some(s) = space {
                        state.split_origin.insert(format!("split-{request}"), s);
                    }
                    MenuSend::Control(ControlRequest::CreateTerminal {
                        id: format!("split-{request}"),
                        cwd: focused_cwd(state),
                        workspace: space,
                        name: None,
                    })
                }
            }
        }
        "zoom pane" => MenuSend::Zoom,
        "toggle agents view" => {
            state.view = state.view.toggle();
            MenuSend::Nothing
        }
        "keybinds" => {
            state.reading = Some(Reading::Keybinds);
            MenuSend::Nothing
        }
        "settings" => {
            state.reading = Some(Reading::Settings);
            MenuSend::Nothing
        }
        "reload manifests" => {
            *request += 1;
            state.message = Some("reloading agent manifests".to_string());
            MenuSend::Control(ControlRequest::ReloadAgentManifests {
                id: format!("reload-{request}"),
            })
        }
        "detach" => MenuSend::Detach,
        _ => unreachable!("menu row without dispatch"),
    }
}

/// A mouse press while the menu is open: which row it lands on, if any.
///
/// `Some(Some(row))` is a press on a row, `Some(None)` a press anywhere else,
/// `None` no press in these bytes at all. Wheel, motion, releases and other
/// buttons are not a choice, so they read as no press rather than as outside.
fn menu_click(bytes: &[u8], cols: u16, lines: u16) -> Option<Option<usize>> {
    let mut at = 0;
    let mut found: Option<Option<usize>> = None;
    while let Some(start) = bytes[at..].windows(3).position(|w| w == b"\x1b[<") {
        let from = at + start;
        let Some(end) = bytes[from..].iter().position(|b| *b == b'M' || *b == b'm') else {
            break;
        };
        let end = from + end;
        let press = bytes[end] == b'M';
        if let Ok(body) = std::str::from_utf8(&bytes[from + 3..end]) {
            let mut parts = body.split(';');
            if let (Some(button), Some(col), Some(row)) = (
                parts.next().and_then(|s| s.parse::<u16>().ok()),
                parts.next().and_then(|s| s.parse::<u16>().ok()),
                parts.next().and_then(|s| s.parse::<u16>().ok()),
            ) {
                let wheel = button & 64 != 0;
                let motion = button & 32 != 0;
                let plain = button & 0b0000_0011;
                if press && !wheel && !motion && plain == 0 {
                    found = Some(crate::screen::menu_row_at(cols, lines, row, col));
                }
            }
        }
        at = end + 1;
    }
    found
}

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
///
/// Pinned to the default width on purpose: a naming box is a rare,
/// short-lived overlay, and a budget that follows the pulled panel would
/// let a wide drag smuggle a name the default column cannot show.
const NAME_LIMIT: usize = crate::sidebar::WIDTH - 6;

/// What one read of stdin meant while the focus list owns the keyboard.
#[derive(Debug)]
enum FocusOutcome {
    /// The list moved under the wheel or the arrows: repaint.
    Scrolled,
    /// The list closed with nothing sent: repaint.
    Closed,
    /// The list closed and these bytes are ordinary input after all: feed
    /// them to the key decoder as though the list had never been open.
    Passthrough(Vec<u8>),
}

/// Move the focus list, clamped to what it holds.
///
/// The window is bottom-pinned: offset zero shows the tail, so new events
/// arrive in view by themselves, and scrolling up hides rows below.
fn focus_height(state: &State) -> usize {
    let (_, pane_rows) = screen::pane_size_for(
        state.cols,
        state.lines,
        state.collapsed,
        0,
        state.sidebar_width,
    );
    (pane_rows as usize).max(1)
}

fn scroll_focus(state: &mut State, up: bool, step: usize) {
    let max = state.focus_events.len().saturating_sub(focus_height(state));
    if up {
        state.focus_offset = state.focus_offset.saturating_add(step).min(max);
    } else {
        state.focus_offset = state.focus_offset.saturating_sub(step);
    }
}

/// Shut the focus list: open state, scroll position and any unsent fetch.
///
/// One place because the view closes from five directions (its own keys, a
/// click, the toggle, an empty space, a dead terminal) and a stale offset
/// or a stuck fetch flag in any of them is a bug that only shows later.
fn close_focus(state: &mut State) {
    state.focus = false;
    state.focus_offset = 0;
    state.focus_fetch = false;
}

/// Decide what one read of stdin meant while the focus list owns the keyboard.
///
/// One logical key at a time, because a read can hold several: a held arrow
/// repeats into one chunk, a paste coalesces, and matching the whole chunk
/// would make scroll-vs-close depend on read chunking. A scroll key scrolls
/// and the rest is reconsidered with the view still open; anything else
/// closes first, and the bytes from the close keystroke on are ordinary
/// input, so no keystroke is lost to the view.
///
/// The `ctrl-a F` pair that closes is swallowed, not passed on: feeding it
/// to the decoder would toggle the view straight back open onto the same
/// keystrokes. Any other prefix sequence is passed on untouched, so `ctrl-a
/// d` still detaches. A prefix as the last byte of the chunk is passed on
/// undecided: split across reads, the decoder holds it, and a following `F`
/// reopens, which is the one keystroke this swallowing misses.
fn focus_key(state: &mut State, bytes: &[u8]) -> FocusOutcome {
    const PREFIX: u8 = crate::keys::PREFIX;
    let mut scrolled = false;
    let mut at = 0;
    while at < bytes.len() {
        let rest = &bytes[at..];
        // Arrow and SS3 spellings, the way the wheel arm spells them.
        let scroll = if rest.starts_with(b"\x1b[A") || rest.starts_with(b"\x1bOA") {
            Some((true, 3))
        } else if rest.starts_with(b"\x1b[B") || rest.starts_with(b"\x1bOB") {
            Some((false, 3))
        } else if rest[0] == b'k' {
            Some((true, 1))
        } else if rest[0] == b'j' {
            Some((false, 1))
        } else {
            None
        };
        if let Some((up, len)) = scroll {
            scroll_focus(state, up, 1);
            scrolled = true;
            at += len;
            continue;
        }
        // Everything else closes first; what differs is what survives it.
        close_focus(state);
        // The close keystroke itself is dropped; what follows it is typed.
        let from = if rest[0] == PREFIX && rest.get(1) == Some(&b'F') {
            at + 2
        } else if rest[0] == b'\r' || rest[0] == b'\n' || rest[0] == b'q' {
            at + 1
        } else if rest[0] == 0x1b {
            // A lone esc, or an esc opening something that is not an arrow:
            // the esc was the close, and the byte after it is ordinary.
            // A split arrow (`\x1b[` at the end of the chunk) types its tail
            // the way the ordinary path does: the decoder there holds no
            // partial escape either.
            at + 1
        } else {
            at
        };
        if from < bytes.len() {
            return FocusOutcome::Passthrough(bytes[from..].to_vec());
        }
        return if scrolled {
            FocusOutcome::Scrolled
        } else {
            FocusOutcome::Closed
        };
    }
    if scrolled {
        FocusOutcome::Scrolled
    } else {
        // An empty read carries no key: nothing to scroll, nothing to send.
        // The caller never sends one, and this arm is the honest default.
        FocusOutcome::Closed
    }
}

/// What the interface drew where a person clicked.
enum Clicked {
    Menu,
    View,
    Collapse,
    Space(i64),
    Terminal(u32),
    NewTerminal,
    NewSpace,
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
    run_stream(stream, leftover, notice).await
}

/// The interface over a connection that has already done its handshake: a local socket,
/// an `ssh` running `qoro relay`, or the mux that puts several daemons behind one stream.
pub async fn run_stream<S>(stream: S, leftover: BytesMut, notice: Option<String>) -> io::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
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

async fn drive<S>(stream: S, leftover: BytesMut, notice: Option<String>) -> io::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (mut rd, mut wr) = tokio::io::split(stream);
    let (cols, lines) = client::outer_size();
    let mut state = State {
        rows: Vec::new(),
        focused: 0,
        snapshot: None,
        snapshots: std::collections::BTreeMap::new(),
        images: std::collections::BTreeMap::new(),
        // Read once: what this client's outer terminal can draw decides real
        // picture versus placeholder for the life of the session.
        graphics: crate::graphics::detect(),
        shown_images: std::collections::BTreeMap::new(),
        img_size: (0, 0),
        subs: std::collections::BTreeSet::new(),
        layouts: std::collections::BTreeMap::new(),
        panes: std::collections::BTreeSet::new(),
        split_origin: std::collections::BTreeMap::new(),
        cols,
        lines,
        // What the caller wanted said, or that this is connecting. A notice about a
        // missing hook is worth more than the word "connecting", which is gone by the time
        // anybody reads it.
        message: Some(CONNECTING.to_string()),
        hint: notice,
        pending_focus: None,
        spaces: Vec::new(),
        space: None,
        was: std::collections::BTreeMap::new(),
        ring: false,
        prompt: None,
        menu: None,
        collapsed: false,
        focus: false,
        focus_events: Vec::new(),
        focus_offset: 0,
        focus_fetch: false,
        reading: None,
        wants_terminal_in: Vec::new(),
        adopted: false,
        adopted_resize: false,
        creating: 0,
        done: false,

        view: sidebar::View::default(),
        seen_done: sidebar::Seen::new(),
        unseen_finished: sidebar::Unseen::new(),
        tick: 0,
        // Read now, not zero: the first paint lands on the first loop turn,
        // and a click before it must still test a real clock.
        paint_now: sidebar::now_unix(),
        sidebar_width: sidebar::WIDTH,
        sidebar_drag: None,
        sidebar_sized: sidebar::WIDTH,
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
    // Mutating ids are replayed by the daemon's durable ledger (ADR-017),
    // so they must not repeat across interface runs: a counter from zero
    // made a second run's first `close-N` or `new-N` replay an older answer
    // and do nothing. Started from the wall clock in nanoseconds, the way
    // the CLI's `request_id` stays unique.
    let mut request = request_seed();
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
    let mut presenter = present::Presenter::default();
    let mut written = String::new();
    let mut presented = tokio::time::Instant::now();
    presented = presented.checked_sub(FRAME).unwrap_or(presented);

    let outcome: io::Result<()> = async {
        loop {
            // At most one frame per `FRAME`. The first render after a quiet spell goes out
            // at once, so a keystroke echoes with no added wait; renders arriving closer
            // together than that wait for the timer arm below and go out as one frame.
            if dirty && presented.elapsed() >= FRAME {
                // One frame for both paints: a single clock for tiers, order
                // and the hit testing below, and its time remembered for the
                // clicks that land between paints.
                let mut frame = state.frame();
                // A picture the outer terminal cannot draw is named on the pane
                // instead. The notice is computed before the paint; the real
                // picture never enters the painted text at all. Nothing is named
                // while scrolled back: the screen shows history, which holds no
                // pictures, and the reconcile below brings the placement down.
                let notice = match state.images.get(&state.focused) {
                    Some(image)
                        if state.anchor.is_none()
                            && !state.graphics.allows(image.protocol) =>
                    {
                        Some(crate::graphics::placeholder(image))
                    }
                    _ => None,
                };
                frame.image_notice = notice.as_deref();
                // Where a real picture lands: the focused pane's last row, in
                // screen cells, one based. Copied from the cursor arithmetic in
                // `screen::pane_cursor`, which owns these coordinates.
                let tile_place_at: Option<(usize, usize)> = {
                    let left = screen::left_for(state.cols, state.collapsed, state.sidebar_width);
                    if frame.tiles.len() < 2 {
                        let (_, pane_rows) = screen::pane_size_for(
                            state.cols,
                            state.lines,
                            state.collapsed,
                            0,
                            state.sidebar_width,
                        );
                        (pane_rows > 0).then_some((screen::TABS + pane_rows as usize, left + 1))
                    } else {
                        frame.tiles.iter().find(|t| t.focused).map(|t| {
                            (
                                screen::TABS + t.rect.y + t.rect.rows,
                                left + 1 + t.rect.x,
                            )
                        })
                    }
                };
                let mut painted = screen::paint(&frame);
                // The focus list covers the pane the frame just drew, the way the
                // palette does. Never over an overlay: an open palette owns the
                // keyboard first, and the list never covers it.
                if state.focus
                    && state.focused != 0
                    && state.palette.is_none()
                    && state.prompt.is_none()
                    && state.menu.is_none()
                    && state.reading.is_none()
                {
                    screen::paint_focus(
                        &frame,
                        &state.focus_events,
                        state.focus_offset,
                        &mut painted,
                    );
                }
                // The borrow ends here: the clock is copied out before the
                // state is touched again below.
                let paint_now = frame.now_unix;
                drop(frame);
                state.paint_now = paint_now;
                written.clear();
                presenter.present(&painted, state.cols, state.lines, &mut written);
                // Pictures, reconciled per frame. Anything placed on the outer
                // screen and no longer desired comes down; the current picture
                // goes up over the pane whose last row the notice names. The
                // block runs after the text frame in the same flush, with the
                // cursor restored, so the row diff never sees image bytes.
                //
                // A floating overlay suppresses placement the way scrollback
                // does: the picture would show straight through the palette,
                // so it comes down while one is open and goes back up after.
                let suppressed = state.palette.is_some()
                    || state.prompt.is_some()
                    || state.menu.is_some()
                    || state.reading.is_some()
                    || state.focus;
                let resized = state.img_size != (state.cols, state.lines);
                let anchored = state.anchor.is_some() || suppressed;
                let (image_ops, shown) = crate::graphics::plan(
                    state.focused,
                    state.images.get(&state.focused),
                    &state.shown_images,
                    state.graphics,
                    anchored,
                    resized,
                    tile_place_at,
                );
                state.shown_images = shown;
                if resized {
                    state.img_size = (state.cols, state.lines);
                }
                let image_block = crate::graphics::block(
                    &image_ops,
                    presenter.cursor().unwrap_or(((1, 1), true)),
                );
                if state.ring {
                    // After the frame, so the terminal has the new colour on screen when
                    // the person looks up.
                    written.push('\x07');
                    state.ring = false;
                }
                // A frame that changed nothing on screen is not a write at all.
                if !written.is_empty() {
                    out.write_all(written.as_bytes())?;
                }
                if !image_block.is_empty() {
                    out.write_all(&image_block)?;
                }
                if !written.is_empty() || !image_block.is_empty() {
                    out.flush()?;
                }
                dirty = false;
                presented = tokio::time::Instant::now();
                // A pulled panel reflows at most once per painted frame, no
                // matter how many motions the frame coalesced: the width
                // never changes the row count, so no intermediate width is
                // worth a reflow of its own.
                if state.sidebar_width != state.sidebar_sized {
                    if let Some(resize) = resize_focused_to_tile(&state, &mut seqs)? {
                        wr.write_all(&resize).await?;
                    }
                    state.sidebar_sized = state.sidebar_width;
                }
            }

            // The panel's next tier boundary, if any idle row has one coming.
            // One shot and not an interval: the arm below sleeps until it and
            // then goes back to never. Folded away, the strip shows no tiers,
            // so nothing is armed. See `sidebar::wants_tick`.
            let tier_at = if state.collapsed {
                None
            } else {
                let now_wall = sidebar::now_unix();
                sidebar::wants_tick(&state.rows, now_wall).and_then(|at| {
                    let wait = (at - now_wall).max(0) as u64;
                    tokio::time::Instant::now()
                        .checked_add(std::time::Duration::from_secs(wait))
                })
            };

            tokio::select! {
                biased;
                _ = ack_tick.tick() => {
                    if let Some(ack) = connection.renders().due_ack(std::time::Instant::now()) {
                        wr.write_all(&ack).await?;
                    }
                    drain_pending_focus(&mut state, &mut control_seq, &mut seqs, &mut wr, &mut dirty).await?;
                }
                _ = sigterm.recv() => return Ok(()),
                _ = sighup.recv() => return Ok(()),
                _ = sigwinch.recv() => {
                    let (cols, lines) = client::outer_size();
                    state.cols = cols;
                    state.lines = lines;
                    // The window moved: refresh what is shown before
                    // sizing, or the child reflows against the width the pane no longer
                    // has.
                    // The focused tile only. A resize is a claim about the window the
                    // person is looking at, and sending it to every terminal would reflow
                    // the ones they are not.
                    if let Some(resize) = resize_focused_to_tile(&state, &mut seqs)? {
                        wr.write_all(&resize).await?;
                    }
                    // A taller window reads further back, and what was fetched for the
                    // shorter one stops at the row the shorter one needed: without asking
                    // again, growing the window while scrolled back fills the new rows at
                    // the top with blanks until the person turns the wheel.
                    if state.anchor.is_some() && state.focused != 0 {
                        let want = behind_request(
                            state.focused,
                            state.rows_back() as usize,
                            focused_rect(&state).rows,
                        );
                        wr.write_all(&client::encode_control_seq(&want, &mut control_seq)?)
                            .await?;
                    }
                    dirty = true;
                }
                typed = stdin_rx.recv() => {
                    let Some(bytes) = typed else { return Ok(()) };
                    // Ahead of the keystroke itself: a resize behind the input
                    // loses the race with the shell reading it.
                    drain_adopt_resize(&mut state, &mut control_seq, &mut seqs, &mut wr, &mut dirty).await?;
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
                        // A click chooses a row, or closes on the box around it. Checked
                        // before the keyboard owns the bytes, or the press reads as an
                        // escape and the menu shuts without choosing anything.
                        if let Some(hit) = menu_click(&bytes, state.cols, state.lines) {
                            match hit {
                                Some(item) => {
                                    state.menu = None;
                                    match apply_menu_choice(&mut state, item, &mut request) {
                                        MenuSend::Nothing => {}
                                        MenuSend::Detach => return Ok(()),
                                        MenuSend::Control(ask) => {
                                            wr.write_all(&client::encode_control_seq(
                                                &ask,
                                                &mut control_seq,
                                            )?)
                                            .await?;
                                        }
                                        MenuSend::Zoom => {
                                            let (changed, out) = zoom_flip(
                                                &mut state,
                                                &mut control_seq,
                                                &mut seqs,
                                            )?;
                                            if changed && let Some(out) = out {
                                                wr.write_all(&out).await?;
                                            }
                                        }
                                    }
                                }
                                None => {
                                    state.menu = None;
                                }
                            }
                            dirty = true;
                            // The release of the same click arrives next: with no press
                            // in it this branch ignores it, and the closed menu lets
                            // it fall through to the pane below.
                        } else {
                            // The menu owns the keyboard while it is open, the way the
                            // palette and the naming box do, and hands a prefix back
                            // for the same reason.
                            match menu_key(&mut state, selected, &bytes) {
                                MenuOutcome::Moved | MenuOutcome::Closed => dirty = true,
                                MenuOutcome::Chose(item) => {
                                    match apply_menu_choice(&mut state, item, &mut request) {
                                        MenuSend::Nothing => {}
                                        MenuSend::Detach => return Ok(()),
                                        MenuSend::Control(ask) => {
                                            wr.write_all(&client::encode_control_seq(
                                                &ask,
                                                &mut control_seq,
                                            )?)
                                            .await?;
                                        }
                                        MenuSend::Zoom => {
                                            let (changed, out) = zoom_flip(
                                                &mut state,
                                                &mut control_seq,
                                                &mut seqs,
                                            )?;
                                            if changed && let Some(out) = out {
                                                wr.write_all(&out).await?;
                                            }
                                        }
                                    }
                                    dirty = true;
                                }
                                MenuOutcome::Passthrough(rest) => feed = Some(rest),
                            }
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
                                        // The new tab follows the
                                        // focused terminal's directory.
                                        cwd: focused_cwd(&state),
                                        // The space the person is looking at.
                                        workspace: state.space,
                                        name,
                                    },
                                    Naming::Space => ControlRequest::CreateWorkspace {
                                        id: format!("space-{request}"),
                                        // Empty means "you name it", which is the numbered
                                        // name this counted before anybody was asked.
                                        name: name.unwrap_or_else(|| next_space_name(&state)),
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
                    } else if state.focus {
                        // The focus list owns the keyboard while it is open, the way the
                        // palette and the naming box do, and hands the rest back for the
                        // same reason. Checked after the overlays: an open palette owns
                        // the keyboard first, and the list never covers it.
                        match focus_key(&mut state, &bytes) {
                            FocusOutcome::Scrolled | FocusOutcome::Closed => dirty = true,
                            FocusOutcome::Passthrough(rest) => {
                                // The close repaints with the list gone; the bytes
                                // themselves run the ordinary path below.
                                dirty = true;
                                feed = Some(rest);
                            }
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
                                //
                                // Typing also drops a selection: the text is already on
                                // the clipboard from the button release, so the invert
                                // staying up only reads as the program's own colours
                                // going wrong.
                                if state.selection.take().is_some() {
                                    dirty = true;
                                }
                                let focused = state.focused;
                                if focused != 0 {
                                    // Large pastes travel as several small messages
                                    // rather than one huge one: one message
                                    // inflates on the wire and trips the
                                    // reassembly bound, dropping the whole paste
                                    // where a streamed one flows.
                                    for piece in client::split_input(&bytes) {
                                        wr.write_all(&client::encode_command_to(
                                            focused,
                                            // A paste is still the person's own
                                            // hands: no wait edge, like typing.
                                            &ClientWire::Input {
                                                bytes: piece.to_vec(),
                                                from_terminal: None,
                                                from_token: None,
                                            },
                                            seqs.entry(focused).or_default(),
                                        )?)
                                        .await?;
                                    }
                                }
                            }
                            Action::Detach => return Ok(()),
                            Action::Update => {
                                state.message = Some(update_message());
                                dirty = true;
                            }
                            Action::Click { row, col, shift } => {
                                // A click lands on the live pane, never on the
                                // list: the list closes under it and the press
                                // itself runs the ordinary path below.
                                if state.focus {
                                    close_focus(&mut state);
                                    dirty = true;
                                }
                                // The divider first: a press there arms a drag
                                // and consumes the press whole, so it never
                                // reaches a tile, the column, or the fold mark
                                // the divider also crosses.
                                if !shift && state.divider_press(col, row) {
                                    // A press anywhere else puts down whatever
                                    // was selected, divider presses included.
                                    if state.selection.take().is_some() {
                                        dirty = true;
                                    }
                                    continue;
                                }
                                // A tiled pane first. A click on an unfocused tile only
                                // focuses it: the click itself goes nowhere, so coordinates
                                // aimed at one pane are never typed into another. The
                                // keystroke goes to the second click.
                                if let Some((terminal, _, (tile_row, tile_col))) =
                                    state.tile_at_screen(row, col)
                                {
                                    if terminal != state.focused {
                                        if terminal != 0
                                            && let Some(bytes) = focus(
                                                &mut state,
                                                terminal,
                                                &mut control_seq,
                                                &mut seqs,
                                            )?
                                        {
                                            wr.write_all(&bytes).await?;
                                            dirty = true;
                                        }
                                        continue;
                                    }
                                    // In the focused pane, and the program in it has not
                                    // taken the mouse, or the person said this one is
                                    // theirs: a press starts a selection rather than
                                    // reaching anything.
                                    if state.pane_wants_mouse() && !shift {
                                        if let Some(bytes) =
                                            mouse_report(0, tile_row, tile_col, true)
                                        {
                                            send_to_pane(&mut state, &mut seqs, &mut wr, bytes)
                                                .await?;
                                        }
                                    } else {
                                        state.selection =
                                            Some(crate::selection::Selection::start(
                                                tile_row, tile_col,
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
                                    Some(Clicked::NewSpace) => {
                                        state.prompt = Some(Prompt {
                                            what: Naming::Space,
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
                                    Some(Clicked::Menu) => {
                                        state.menu = Some(0);
                                        dirty = true;
                                    }
                                    Some(Clicked::View) => {
                                        state.view = state.view.toggle();
                                        dirty = true;
                                    }
                                    Some(Clicked::Collapse) => {
                                        state.collapsed = !state.collapsed;
                                        // The pane's width changed with the column, and a
                                        // child that is not told reflows its output against
                                        // a width it no longer has.
                                        if let Some(resize) =
                                            resize_focused_to_tile(&state, &mut seqs)?
                                        {
                                            wr.write_all(&resize).await?;
                                        }
                                        dirty = true;
                                    }
                                    None => {}
                                }
                            }
                            Action::Drag { row, col, shift } => {
                                // Like a click: the drag starts on the live pane,
                                // never on the list under the overlay.
                                if state.focus {
                                    close_focus(&mut state);
                                    dirty = true;
                                }
                                // An armed divider drag owns every motion until
                                // release, wherever the pointer went: latching
                                // here keeps the pane, selection and forward
                                // paths below from seeing it.
                                if state.sidebar_drag.is_some() {
                                    if state.divider_motion(col, shift) {
                                        dirty = true;
                                    }
                                    continue;
                                }
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
                                // Like a click: the release belongs to the live
                                // pane, never to the list under the overlay.
                                if state.focus {
                                    close_focus(&mut state);
                                    dirty = true;
                                }
                                // A divider drag ends anywhere and reaches
                                // neither selection-copy nor mouse-forward.
                                // Scrollback refetch only when the pull moved
                                // the panel under an anchored view, in
                                // sigwinch order: resize first, refetch with
                                // post-resize rows. The resize is flushed
                                // here rather than left to the paint block,
                                // or the refetch would ask with stale rows.
                                if state.sidebar_drag.is_some() {
                                    if state.finish_drag()
                                        && state.anchor.is_some()
                                        && state.focused != 0
                                    {
                                        if state.sidebar_width != state.sidebar_sized {
                                            if let Some(resize) =
                                                resize_focused_to_tile(&state, &mut seqs)?
                                            {
                                                wr.write_all(&resize).await?;
                                            }
                                            state.sidebar_sized = state.sidebar_width;
                                        }
                                        let want = behind_request(
                                            state.focused,
                                            state.rows_back() as usize,
                                            focused_rect(&state).rows,
                                        );
                                        wr.write_all(&client::encode_control_seq(
                                            &want, &mut control_seq,
                                        )?)
                                        .await?;
                                    }
                                    continue;
                                }
                                let inside = state.in_pane(row, col);
                                if let Some(selection) = state.selection {
                                    // Copied when the button comes up, with no key to
                                    // press: a selection that has to be confirmed is a
                                    // selection a person makes twice.
                                    // Read from the window as it is drawn, not from the
                                    // live screen: scrolled back, the rows under the
                                    // pointer are rows that went past, and copying the
                                    // live screen instead would hand the person text they
                                    // never saw. Sized to the focused tile, like the
                                    // selection itself.
                                    let tile = focused_rect(&state);
                                    let window: Vec<String> = (0..tile.rows)
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
                                    let text = selection.text(&window, tile.cols);
                                    if !text.is_empty() {
                                        let mut out = std::io::stdout();
                                        match crate::selection::copy(&text, &mut out)? {
                                            Some(via) => {
                                                state.message = Some(copied(&text, via));
                                            }
                                            // Empty text never reaches here (guarded
                                            // above); a `None` without text is a click
                                            // that released outside any cell.
                                            None => {
                                                state.selection = None;
                                            }
                                        }
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
                                if state.focus {
                                    // The list scrolls under the wheel; the program
                                    // underneath never sees it.
                                    scroll_focus(&mut state, up, WHEEL_ROWS);
                                    dirty = true;
                                    continue;
                                }
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
                                    // scrolled past instead. Sized to the focused tile.
                                    let tile_rows = focused_rect(&state).rows;
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
                                            tile_rows,
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
                            Action::CloseSpace => {
                                let Some(space) = state.space else {
                                    state.message = Some("no space to close".to_string());
                                    dirty = true;
                                    continue;
                                };
                                // Counted here, refused there too: the index may lag a
                                // close that is still settling, and the daemon is the
                                // one that knows. Either refusal reads the same way.
                                let held = state
                                    .rows
                                    .iter()
                                    .filter(|r| r.workspace == space)
                                    .count();
                                if held > 0 {
                                    state.message = Some(format!(
                                        "space still holds {held} terminal{}, close them first (ctrl-a x)",
                                        if held == 1 { "" } else { "s" }
                                    ));
                                    dirty = true;
                                    continue;
                                }
                                request += 1;
                                wr.write_all(&client::encode_control_seq(
                                    &ControlRequest::CloseWorkspace {
                                        id: format!("close-space-{request}"),
                                        workspace: space,
                                    },
                                    &mut control_seq,
                                )?).await?;
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
                            Action::Keybinds => {
                                state.reading = Some(Reading::Keybinds);
                                dirty = true;
                            }
                            Action::Collapse => {
                                state.collapsed = !state.collapsed;
                                // The pane's width changed with the column, and a child
                                // that is not told reflows its output against a width it no
                                // longer has. Only the focused tile is told: the others
                                // are viewport-cropped.
                                if let Some(resize) =
                                    resize_focused_to_tile(&state, &mut seqs)?
                                {
                                    wr.write_all(&resize).await?;
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
                            Action::ToggleView => {
                                state.view = state.view.toggle();
                                dirty = true;
                            }
                            Action::Focus => {
                                if state.focus {
                                    close_focus(&mut state);
                                    dirty = true;
                                } else if state.focused == 0 {
                                    state.message = Some(
                                        "nothing to focus, ctrl-a c starts a terminal"
                                            .to_string(),
                                    );
                                    dirty = true;
                                } else {
                                    state.focus = true;
                                    state.focus_offset = 0;
                                    state.focus_events.clear();
                                    // Asked for at once, not on the fetch flag: an idle
                                    // daemon sends nothing, and the flag below is only
                                    // drained on an arrival.
                                    request += 1;
                                    let (_, pane_rows) = screen::pane_size_for(
                                        state.cols,
                                        state.lines,
                                        state.collapsed,
                                        0,
                                        state.sidebar_width,
                                    );
                                    wr.write_all(&client::encode_control_seq(
                                        &ControlRequest::ListFocusEvents {
                                            id: format!("focus-{request}"),
                                            terminal: state.focused,
                                            last_n: pane_rows as usize,
                                        },
                                        &mut control_seq,
                                    )?)
                                    .await?;
                                    dirty = true;
                                }
                            }
                            Action::SplitV | Action::SplitH => {
                                let dir = if matches!(action, Action::SplitV) {
                                    layout::Dir::V
                                } else {
                                    layout::Dir::H
                                };
                                let focused = state.focused;
                                let space = state.space;
                                if focused == 0 {
                                    state.message = Some(
                                        "nothing to split, ctrl-a c starts a terminal"
                                            .to_string(),
                                    );
                                    dirty = true;
                                } else {
                                    ensure_layout(&mut state);
                                    let area = pane_area(&state);
                                    let placed = space
                                        .and_then(|s| state.layouts.get_mut(&s))
                                        .is_some_and(|layout| {
                                            layout.split_placeholder(focused, dir, area)
                                        });
                                    if !placed {
                                        state.message = Some(
                                            "cannot split now: too small, zoomed, or one already on its way"
                                                .to_string(),
                                        );
                                        dirty = true;
                                    } else {
                                        // The tree entry goes in first and the terminal
                                        // follows: binding happens when `TerminalCreated`
                                        // answers, rollback when it fails.
                                        request += 1;
                                        if let Some(s) = space {
                                            state.split_origin.insert(format!("split-{request}"), s);
                                        }
                                        wr.write_all(&client::encode_control_seq(
                                            &ControlRequest::CreateTerminal {
                                                id: format!("split-{request}"),
                                                // The split follows
                                                // the focused pane's directory.
                                                cwd: focused_cwd(&state),
                                                workspace: space,
                                                name: None,
                                            },
                                            &mut control_seq,
                                        )?).await?;
                                    }
                                }
                            }
                            Action::PaneMove(mv) => {
                                let target = state.space.and_then(|space| {
                                    state.layouts.get(&space).and_then(|layout| {
                                        layout::neighbour_in_dir(
                                            &layout.tile_rects(pane_area(&state)),
                                            state.focused,
                                            mv,
                                        )
                                    })
                                });
                                if let Some(next) = target
                                    && let Some(bytes) =
                                        focus(&mut state, next, &mut control_seq, &mut seqs)?
                                {
                                    wr.write_all(&bytes).await?;
                                    dirty = true;
                                }
                            }
                            Action::PaneNext => {
                                let target = state.space.and_then(|space| {
                                    state.layouts.get(&space).and_then(|layout| {
                                        let visible = layout.visible();
                                        if visible.len() < 2 {
                                            return None;
                                        }
                                        let at = visible
                                            .iter()
                                            .position(|t| *t == state.focused)
                                            .unwrap_or(0);
                                        Some(visible[(at + 1) % visible.len()])
                                    })
                                });
                                if let Some(next) = target
                                    && let Some(bytes) =
                                        focus(&mut state, next, &mut control_seq, &mut seqs)?
                                {
                                    wr.write_all(&bytes).await?;
                                    dirty = true;
                                }
                            }
                            Action::Zoom => {
                                let (changed, out) =
                                    zoom_flip(&mut state, &mut control_seq, &mut seqs)?;
                                if changed {
                                    // Hidden tiles are unsubscribed with the sync below:
                                    // their output costs nothing while zoomed, at the
                                    // price of one `Refresh` each on the way back.
                                    if let Some(out) = out {
                                        wr.write_all(&out).await?;
                                    }
                                    dirty = true;
                                }
                            }
                            Action::DetachPane => {
                                let focused = state.focused;
                                let space = state.space;
                                // Detaching the last pane would leave a tree with no
                                // leaves over a space that still has a terminal: refuse
                                // instead, `ctrl-a x` is the way to close it.
                                let next = match space
                                    .and_then(|s| state.layouts.get_mut(&s))
                                {
                                    Some(layout) if layout.leaf_count() > 1 => {
                                        layout.remove(focused);
                                        // Detached panes become real tabs again.
                                        state.panes.remove(&focused);
                                        Some(layout.focus)
                                    }
                                    _ => None,
                                };
                                match next {
                                    Some(next) => {
                                        if let Some(bytes) = focus(
                                            &mut state,
                                            next,
                                            &mut control_seq,
                                            &mut seqs,
                                        )? {
                                            wr.write_all(&bytes).await?;
                                            dirty = true;
                                        }
                                    }
                                    None => {
                                        state.message = Some(
                                            "only one pane, ctrl-a x closes its terminal"
                                                .to_string(),
                                        );
                                        dirty = true;
                                    }
                                }
                            }
                        }
                    }
                    // A deferred focus (a recovery after a close)
                    // applies on this keystroke, not on the
                    // daemon's next word: an idle daemon says nothing for
                    // minutes, and until then the keyboard sits in the wrong
                    // terminal while the screen already shows the new one.
                    drain_pending_focus(&mut state, &mut control_seq, &mut seqs, &mut wr, &mut dirty).await?;
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
                                if terminal == state.focused {
                                    state.snapshot = Some(snapshot);
                                    state.message = None;
                                    dirty = true;
                                } else if state.subs.contains(&terminal)
                                    && (state
                                        .rows
                                        .iter()
                                        .any(|r| r.terminal == terminal && Some(r.workspace) == state.space))
                                {
                                    // A visible onlooker in this space: kept for its tile,
                                    // never promoted, never scrolled. Anything else is a
                                    // render from a dropped subscription or another space
                                    // and is ignored.
                                    state.snapshots.insert(terminal, snapshot);
                                    dirty = true;
                                } else if state.focused == 0 && !state.adopted {
                                    // Only what is on screen: adopting another space's
                                    // first render paints it under this space's tab bar
                                    // with the keyboard in it, and the person types
                                    // somewhere they cannot see. Unknown rows (nothing
                                    // listed yet) still adopt; the index below pulls
                                    // the keyboard into its space when it lands.
                                    if adopt_render(&state, terminal) {
                                        state.adopted = true;
                                        state.focused = terminal;
                                        state.snapshot = Some(snapshot);
                                        state.message = None;
                                        // Bootstrap the tiling state to match: one pane, one
                                        // subscription.
                                        if let Some(space) = state.space {
                                            state.layouts.insert(space, Layout::single(terminal));
                                            state.subs.insert(terminal);
                                        }
                                        // And the size to match: the child was sized for
                                        // whoever had it before (a restore spawns at the
                                        // default), so without this the first paint shows
                                        // a small screen until anything moves the focus.
                                        if let Some(resize) =
                                            resize_focused_to_tile(&state, &mut seqs)?
                                        {
                                            wr.write_all(&resize).await?;
                                        }
                                        dirty = true;
                                    }
                                }
                            }
                            Incoming::Image { terminal, image } => {
                                // Stored for the frame painter (overlay or
                                // placeholder in Faz 3). Newest wins per
                                // terminal, like snapshots; drawn by image id.
                                state.images.insert(terminal, image);
                                dirty = true;
                            }
                            Incoming::Event(EventMessage::AgentChanged { .. })
                            | Incoming::Event(EventMessage::Resync) => {
                                // What changed is not in the message on purpose, so the
                                // answer comes from the one place that has it. A resync
                                // is the same refresh: the daemon missed nothing, this
                                // client did, and re-reading the index converges it.
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
                                        | ControlResponse::WorkspaceClosed { .. }
                                        | ControlResponse::Renamed { .. }
                                );
                                refresh_spaces |= matches!(
                                    response,
                                    ControlResponse::TerminalCreated { .. }
                                        | ControlResponse::TerminalClosed { .. }
                                        | ControlResponse::WorkspaceCreated { .. }
                                        | ControlResponse::WorkspaceClosed { .. }
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
                    // A new space's first terminal follows the
                    // focused terminal's directory, which still names the space
                    // the person came from at this point.
                    let cwd = focused_cwd(&state);
                    for space in std::mem::take(&mut state.wants_terminal_in) {
                        request += 1;
                        wr.write_all(&client::encode_control_seq(
                            &ControlRequest::CreateTerminal {
                                id: format!("new-{request}"),
                                cwd: cwd.clone(),
                                workspace: Some(space),
                                // The terminal a new space comes up with, which nobody
                                // was asked about: they named the space.
                                name: None,
                            },
                            &mut control_seq,
                        )?).await?;
                    }
                    drain_pending_focus(&mut state, &mut control_seq, &mut seqs, &mut wr, &mut dirty).await?;
                    // The focus list refetches on its own flag, never on a timer:
                    // opened, moved to another terminal, or told the index
                    // changed while open. A tool event arrives with the state
                    // still `working`, so a state-change trigger would miss it.
                    if state.focus_fetch && state.focus && state.focused != 0 {
                        state.focus_fetch = false;
                        request += 1;
                        let (_, pane_rows) = screen::pane_size_for(
                            state.cols,
                            state.lines,
                            state.collapsed,
                            0,
                            state.sidebar_width,
                        );
                        wr.write_all(&client::encode_control_seq(
                            &ControlRequest::ListFocusEvents {
                                id: format!("focus-{request}"),
                                terminal: state.focused,
                                last_n: pane_rows as usize,
                            },
                            &mut control_seq,
                        )?).await?;
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
                // Last, so it never goes ahead of input: it only wakes the loop to send
                // the frame that the renders above held back.
                _ = tokio::time::sleep_until(presented + FRAME), if dirty => {}
                // Last beside it, for the same reason: a tier boundary arrived
                // with nothing else happening only marks the frame dirty, and
                // the FRAME arm above still decides when it goes out. Minute
                // granularity, so the row dims and sinks within a minute of
                // its boundary. Any other arm firing first repaints anyway
                // and re-arms this one.
                _ = async {
                    match tier_at {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => {
                    dirty = true;
                }
            }
        }
    }
    .await;

    // Off in the reverse order, and before leaving the alternate screen: a terminal left
    // in mouse reporting mode sends escape sequences into whatever the person runs next.
    // Kitty placements outlive the client the same way: the grid goes with the
    // alternate screen below, but the overlay does not. Best effort on the way out.
    {
        let deletes: Vec<crate::graphics::ImageOp> = state
            .shown_images
            .values()
            .filter(|(_, protocol)| *protocol == qoro_runtime::daemon::ImageProtocol::Kitty)
            .map(|(id, _)| crate::graphics::ImageOp::Delete { id: *id })
            .collect();
        let cleanup =
            crate::graphics::block(&deletes, presenter.cursor().unwrap_or(((1, 1), true)));
        if !cleanup.is_empty() {
            let _ = out.write_all(&cleanup);
        }
    }
    let _ = out.write_all(b"\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?1049l");
    let _ = out.flush();
    outcome
}

/// Whether a fresh client's first render may adopt its terminal: only what
/// is on screen. The daemon subscribes a fresh client to whatever is already
/// open, which may be another space's terminal; adopting that paints it under
/// this space's tab bar with the keyboard in it. With the space still unknown
/// the first workspace wins, which is where the workspace list will park the
/// bar: adopting anywhere else desyncs on arrival. Nothing listed yet still
/// adopts; the index below pulls the space under the keyboard when it lands.
fn adopt_render(state: &State, terminal: u32) -> bool {
    match state.rows.iter().find(|r| r.terminal == terminal) {
        Some(row) => match state.space {
            Some(space) => row.workspace == space,
            None => {
                let first = state.rows.iter().map(|r| r.workspace).min();
                Some(row.workspace) == first
            }
        },
        None => state.rows.is_empty(),
    }
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
                        let count = palette
                            .items(
                                &state.rows,
                                &state.spaces,
                                state.focused,
                                &state.seen_done,
                                &state.unseen_finished,
                            )
                            .len();
                        palette.move_by(-1, count);
                        i += 3;
                    }
                    Some(b'B') => {
                        let count = palette
                            .items(
                                &state.rows,
                                &state.spaces,
                                state.focused,
                                &state.seen_done,
                                &state.unseen_finished,
                            )
                            .len();
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
                if let Some(item) = palette.picked(
                    &state.rows,
                    &state.spaces,
                    state.focused,
                    &state.seen_done,
                    &state.unseen_finished,
                ) {
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
///
/// The pane area in its own cells, shared by every tile rect.
fn pane_area(state: &State) -> layout::Rect {
    let (cols, rows) = screen::pane_size_for(
        state.cols,
        state.lines,
        state.collapsed,
        0,
        state.sidebar_width,
    );
    layout::Rect {
        x: 0,
        y: 0,
        cols: cols as usize,
        rows: rows as usize,
    }
}

/// The rectangle of the focused terminal, or the whole pane when there is no
/// tiling tree for this space.
fn focused_rect(state: &State) -> layout::Rect {
    let area = pane_area(state);
    if state.focused == 0 {
        return area;
    }
    let Some(space) = state.space else {
        return area;
    };
    state
        .layouts
        .get(&space)
        .and_then(|layout| {
            layout
                .tile_rects(area)
                .into_iter()
                .find(|(t, _)| *t == state.focused)
                .map(|(_, r)| r)
        })
        .unwrap_or(area)
}

/// Make sure the space on screen has a tiling tree, seeding a single pane
/// from the focused terminal (or the first tab) when it has none.
fn ensure_layout(state: &mut State) {
    let Some(space) = state.space else {
        return;
    };
    if state.layouts.contains_key(&space) {
        return;
    }
    let tabs = state.tabs();
    let seed = tabs
        .iter()
        .find(|r| r.terminal == state.focused)
        .or_else(|| tabs.first())
        .map(|r| r.terminal)
        .unwrap_or(0);
    if seed == 0 {
        return;
    }
    state.layouts.insert(space, Layout::single(seed));
}

/// The terminals this client receives renders for: the visible set of the
/// space on screen, or the focused one when there is no tree yet.
fn visible_set(state: &State) -> Vec<u32> {
    if let Some(space) = state.space
        && let Some(layout) = state.layouts.get(&space)
    {
        layout.visible()
    } else if state.focused != 0 {
        vec![state.focused]
    } else {
        Vec::new()
    }
}

/// Apply a deferred focus through the subscribing path: subscribe, move the
/// keyboard, resize, all in one write. Both wake arms share it, so a focus
/// deferred on an idle daemon applies on the next keystroke as well as on the
/// next daemon message: until then the keyboard sits in the wrong terminal
/// while the screen already shows the new one.
async fn drain_pending_focus(
    state: &mut State,
    control_seq: &mut u32,
    seqs: &mut std::collections::BTreeMap<u32, u32>,
    wr: &mut (impl tokio::io::AsyncWriteExt + Unpin),
    dirty: &mut bool,
) -> std::io::Result<()> {
    if let Some(next) = state.pending_focus.take()
        && let Some(bytes) = focus(state, next, control_seq, seqs)?
    {
        wr.write_all(&bytes).await?;
        *dirty = true;
    }
    drain_adopt_resize(state, control_seq, seqs, wr, dirty).await
}

/// Size an adopted child to this pane, once.
///
/// An adoption without a write in hand cannot size inline, and the child was
/// sized for whoever had it before. Runs everywhere a write is in hand, and
/// ahead of typed input: a resize behind the keystroke loses the race with
/// the shell reading it.
///
/// The adopted terminal is subscribed here too, rather than left to the daemon's habit of
/// subscribing a new connection to its lowest terminal: behind the mux the adopted one can
/// be on another machine, where nothing subscribes it, and locally subscribing twice is
/// one set insert.
async fn drain_adopt_resize(
    state: &mut State,
    control_seq: &mut u32,
    seqs: &mut std::collections::BTreeMap<u32, u32>,
    wr: &mut (impl tokio::io::AsyncWriteExt + Unpin),
    dirty: &mut bool,
) -> std::io::Result<()> {
    if state.adopted_resize {
        state.adopted_resize = false;
        if let Some(subscribe) = sync_subs(state, control_seq)? {
            wr.write_all(&subscribe).await?;
        }
        if let Some(resize) = resize_focused_to_tile(state, seqs)? {
            wr.write_all(&resize).await?;
            *dirty = true;
        }
    }
    Ok(())
}

/// Bring subscriptions to exactly the visible set, in one write. `None` when
/// already there, so moving the focus inside the visible set costs no control
/// traffic and no `Refresh` storm.
/// Flip zoom on the focused pane and build the follow-up bytes, if any.
/// Hidden tiles unsubscribe on the way in and resubscribe on the way out,
/// and the focused terminal relearns its tile. One function for the zoom key
/// and the zoom menu row, so the two never drift. True when the tree
/// changed; a no-op with a single leaf answers false and sends nothing.
fn zoom_flip(
    state: &mut State,
    control_seq: &mut u32,
    seqs: &mut std::collections::BTreeMap<u32, u32>,
) -> io::Result<(bool, Option<BytesMut>)> {
    let space = state.space;
    let changed = match space.and_then(|s| state.layouts.get_mut(&s)) {
        Some(layout) => layout.toggle_zoom(),
        None => false,
    };
    if !changed {
        return Ok((false, None));
    }
    let mut out = BytesMut::new();
    if let Some(sync) = sync_subs(state, control_seq)? {
        out.extend_from_slice(&sync);
    }
    if let Some(resize) = resize_focused_to_tile(state, seqs)? {
        out.extend_from_slice(&resize);
    }
    Ok((true, if out.is_empty() { None } else { Some(out) }))
}

fn sync_subs(state: &mut State, control_seq: &mut u32) -> io::Result<Option<BytesMut>> {
    let want: std::collections::BTreeSet<u32> = visible_set(state).into_iter().collect();
    if want == state.subs {
        return Ok(None);
    }
    let drop: Vec<u32> = state.subs.difference(&want).copied().collect();
    let add: Vec<u32> = want.difference(&state.subs).copied().collect();
    state.subs = want;
    let mut out = BytesMut::new();
    if !drop.is_empty() {
        out.extend_from_slice(&client::encode_control_seq(
            &ControlRequest::Unsubscribe {
                id: "hide-tiles".to_string(),
                terminals: drop,
            },
            control_seq,
        )?);
    }
    if !add.is_empty() {
        out.extend_from_slice(&client::encode_control_seq(
            &ControlRequest::Subscribe {
                id: "show-tiles".to_string(),
                terminals: add,
            },
            control_seq,
        )?);
    }
    Ok(Some(out))
}

/// Resize the focused terminal to its own tile. The only resize this client
/// sends for tiling: unfocused tiles are viewport-cropped and never resized,
/// which is what keeps the single-owner rule of ADR-007 intact.
fn resize_focused_to_tile(
    state: &State,
    seqs: &mut std::collections::BTreeMap<u32, u32>,
) -> io::Result<Option<BytesMut>> {
    if state.focused == 0 {
        return Ok(None);
    }
    let rect = focused_rect(state);
    Ok(Some(client::encode_command_to(
        state.focused,
        &ClientWire::Resize {
            cols: rect.cols as u16,
            rows: rect.rows as u16,
        },
        seqs.entry(state.focused).or_default(),
    )?))
}

fn focus(
    state: &mut State,
    to: u32,
    control_seq: &mut u32,
    seqs: &mut std::collections::BTreeMap<u32, u32>,
) -> io::Result<Option<BytesMut>> {
    if to == 0 || to == state.focused {
        return Ok(None);
    }
    let from = state.focused;
    // The space follows the terminal, first of all: a click in the agents
    // panel can land on an agent in another space, which is the point of that
    // panel crossing spaces, and every layout and subscription below is per
    // space. Leaving the bar on the old space would show tabs that do not
    // contain the focused one.
    state.space = state
        .rows
        .iter()
        .find(|r| r.terminal == to)
        .map(|r| r.workspace)
        .or(state.space);
    // A tab outside the tiling tree drops the tree back to a single pane:
    // tabs name every terminal in the space, tiles only the visible ones.
    if let Some(space) = state.space {
        ensure_layout(state);
        if let Some(layout) = state.layouts.get_mut(&space) {
            if layout.contains(to) {
                layout.focus_terminal(to);
            } else {
                state.layouts.insert(space, Layout::single(to));
            }
        }
    }
    // Promote the already-visible snapshot instead of waiting for the next
    // render: the subscription never dropped, so the picture is fresh.
    if let Some(snap) = state.snapshots.remove(&to) {
        if let Some(old) = state.snapshot.take() {
            state.snapshots.insert(from, old);
        }
        state.snapshot = Some(snap);
    } else {
        // Another terminal's screen, and another terminal's history. Both are
        // dropped rather than carried, or the new tab opens scrolled into rows
        // that are not its own.
        if let Some(old) = state.snapshot.take() {
            state.snapshots.insert(from, old);
        }
        state.snapshot = None;
    }
    state.anchor = None;
    state.behind = Behind::default();
    state.selection = None;
    state.focused = to;
    // The list belongs to its terminal: a new one starts unlisted, and the
    // fetch at the end of this function fills it rather than showing the
    // old terminal's rows.
    if state.focus {
        state.focus_events.clear();
        state.focus_offset = 0;
    }
    // Standing on a finished row is reading it: it stops saying `new` the
    // moment the keyboard arrives, with no extra keystroke asked.
    if let Some(row) = state.rows.iter().find(|r| r.terminal == to)
        && matches!(row.state.as_str(), "idle" | "done")
    {
        state.unseen_finished.remove(&(to, sidebar::gen_of(row)));
        if row.state == "done" {
            state.seen_done.insert((to, sidebar::gen_of(row)));
        }
    }
    // Dropped from the onlookers: a snapshot kept past its unsubscription is
    // a screen that can never update again.
    let visible: std::collections::BTreeSet<u32> = visible_set(state).into_iter().collect();
    state.snapshots.retain(|t, _| visible.contains(t));
    let mut out = BytesMut::new();
    if let Some(sync) = sync_subs(state, control_seq)? {
        out.extend_from_slice(&sync);
    }
    if let Some(resize) = resize_focused_to_tile(state, seqs)? {
        out.extend_from_slice(&resize);
    }
    // The list refetches on the same bytes as the subscribe above: on an
    // idle daemon nothing else arrives to drain a flag on.
    if state.focus {
        let (_, pane_rows) = screen::pane_size_for(
            state.cols,
            state.lines,
            state.collapsed,
            0,
            state.sidebar_width,
        );
        // The id only has to name the family for the refusal arm; the answer
        // is matched on its terminal, never on this string.
        let id = format!("focus-term-{to}-{}", *control_seq);
        out.extend_from_slice(&client::encode_control_seq(
            &ControlRequest::ListFocusEvents {
                id,
                terminal: to,
                last_n: pane_rows as usize,
            },
            control_seq,
        )?);
    }
    state.message = None;
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
    ensure_layout(state);
    // The layout's visible terminals first, so a tiled space comes back as it
    // was left; the tab list only when the tree names nothing alive. Panes
    // never restore the keyboard: `tabs()` hides them, so re-entering lands
    // on the first tab while the tile border keeps the pane focus inside.
    let first = state
        .layouts
        .get(&space)
        .and_then(|layout| {
            layout
                .visible()
                .into_iter()
                .find(|t| state.tabs().iter().any(|r| r.terminal == *t))
        })
        .or_else(|| state.tabs().first().map(|r| r.terminal));
    match first {
        // Already there: the subscriptions still name the old space, so sync
        // and resize even though the focus does not move.
        Some(first) if first == state.focused => {
            let mut out = BytesMut::new();
            if let Some(sync) = sync_subs(state, control_seq)? {
                out.extend_from_slice(&sync);
            }
            if let Some(resize) = resize_focused_to_tile(state, seqs)? {
                out.extend_from_slice(&resize);
            }
            Ok(Some(out))
        }
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
    state.focused = 0;
    state.snapshot = None;
    state.message = Some(why.to_string());
    // No terminal, no list: the overlay hides, and so does the keyboard
    // ownership that would otherwise intercept keys for nobody.
    close_focus(state);
    // Every subscription goes, not just the focused one: with tiling the
    // client can watch several terminals, and leaving any of them subscribed
    // paints output from a space the person has left.
    let drop: Vec<u32> = std::mem::take(&mut state.subs).into_iter().collect();
    if drop.is_empty() {
        return Ok(None);
    }
    Ok(Some(client::encode_control_seq(
        &ControlRequest::Unsubscribe {
            id: "hide-tiles".to_string(),
            terminals: drop,
        },
        control_seq,
    )?))
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
                // The bar just parked somewhere: a keyboard adopted from
                // another space comes home to its first tab now, through the
                // subscribing path. An idle session sends no further index,
                // so waiting for one would strand the keyboard unseen.
                if state.pending_focus.is_none()
                    && let Some(space) = state.space
                    && state.focused != 0
                {
                    let here = state
                        .rows
                        .iter()
                        .any(|r| r.terminal == state.focused && r.workspace == space);
                    if !here {
                        state.pending_focus = state.tabs().first().map(|r| r.terminal);
                    }
                }
            }
            state.spaces = workspaces;
            *dirty = true;
        }
        ControlResponse::WorkspaceCreated { workspace, .. } => {
            state.creating = state.creating.saturating_sub(1);
            state.space = Some(workspace);
            state.snapshot = None;
            // A new space starts bare: no tree, and forget the old subs. The
            // unsubscribe itself cannot go out from here (`apply` cannot
            // write), so it queues behind the terminal request the new space
            // always triggers: `wants_terminal_in` drains with a write in
            // hand, and the render filter below drops the old space's renders
            // by space in the meantime.
            state.layouts.remove(&workspace);
            state.subs.clear();
            state.snapshots.clear();
            // Asked for here rather than by the caller, because a space with no terminal in
            // it shows an empty pane and a bar with only a `+` on it, and the first thing
            // anybody does in a new space is start a terminal.
            state.wants_terminal_in.push(workspace);
            *dirty = true;
        }
        ControlResponse::WorkspaceClosed { workspace, .. } => {
            // Its tree goes with it: a layout for a space that no longer exists would
            // reseed nothing and leak the entry. The space itself moves to whatever the
            // next list answers: `None` reads as "pick the first" on that path.
            state.layouts.remove(&workspace);
            if state.space == Some(workspace) {
                state.space = None;
                state.snapshot = None;
            }
            *dirty = true;
        }
        ControlResponse::Agents { agents, .. } => {
            if state.focused == 0 && !state.adopted {
                // Only before this client has ever had a terminal. The daemon subscribes a
                // fresh client to what is already open, so this is the bootstrap and no
                // subscription of our own is needed. Later, nothing focused is a space with
                // nothing in it, and reaching into another space's terminal here is how a
                // person ends up typing somewhere they cannot see. Without a space yet
                // the first workspace wins, the way the workspace list and the first
                // render both pick it.
                let first = match state.space {
                    Some(s) => agents.iter().find(|r| r.workspace == s),
                    None => {
                        let min = agents.iter().map(|r| r.workspace).min();
                        agents.iter().find(|r| Some(r.workspace) == min)
                    }
                };
                if let Some(first) = first {
                    state.adopted = true;
                    state.focused = first.terminal;
                    // No write in hand here; the resize goes out with the
                    // deferred focus below, which also runs on the ack tick.
                    state.adopted_resize = true;
                    // The space follows the adoption: leaving it for the first
                    // workspace list parks the bar in another space than the
                    // keyboard, and the person types where they cannot see.
                    if state.space.is_none() {
                        state.space = Some(first.workspace);
                    }
                }
            }
            // A terminal can go away without this client having asked: its shell exited, or
            // somebody closed it from another client. The daemon says so by changing what
            // the index holds and in no other way, so this is the only place that can catch
            // the keyboard up with it. Measured before this: typing `exit` took the tab off
            // the bar and left the focus on a terminal that no longer existed, with the dead
            // screen still painted and every keystroke going nowhere.
            //
            // Tiling trees are pruned here too, in every space: a leaf nobody can focus
            // is a zombie pane. Layouts left with no leaves and no pending split go
            // away entirely, so the next focus reseeds a single pane.
            for layout in state.layouts.values_mut() {
                let dead: Vec<u32> = layout
                    .leaves()
                    .into_iter()
                    .filter(|t| !agents.iter().any(|r| r.terminal == *t))
                    .collect();
                for terminal in dead {
                    layout.remove(terminal);
                }
            }
            state
                .layouts
                .retain(|_, layout| !layout.leaves().is_empty() || layout.has_pending());
            // TerminalClosed and Agents prune the local set silently: the
            // daemon ended the terminal, which ends its subscription
            // server-side too, so there is nothing to unsubscribe from. The
            // assumption is one-directional on purpose: pruning an id the
            // daemon still streams would resurface as a render for an unknown
            // terminal, which the filter above drops.
            let live: std::collections::BTreeSet<u32> = agents.iter().map(|r| r.terminal).collect();
            state.snapshots.retain(|t, _| live.contains(t));
            state.subs.retain(|t| live.contains(t));
            state.panes.retain(|t| live.contains(t));
            if state.focused != 0 && !agents.iter().any(|r| r.terminal == state.focused) {
                state.focused = 0;
                state.snapshot = None;
                // The tree already picked the neighbour; fall back to the tab list
                // only when the tree names nothing alive.
                state.pending_focus = state
                    .space
                    .and_then(|space| state.layouts.get(&space))
                    .and_then(|layout| layout.visible().into_iter().find(|t| live.contains(t)))
                    .or_else(|| {
                        agents
                            .iter()
                            .find(|r| state.space.is_none_or(|s| r.workspace == s))
                            .map(|r| r.terminal)
                    });
                if state.pending_focus.is_none() {
                    state.message =
                        Some("this space is empty, ctrl-a c starts a terminal".to_string());
                }
            }
            // Nothing left anywhere. The interface is a way into terminals, so with none of
            // them it is a screen a person has to know a key to escape from. Ending is what
            // every multiplexer does when its last window closes, and the daemon stays: it
            // is the client that goes.
            // Nothing focused, nothing on its way, and the space on screen has a tab: that
            // tab, through the subscribing path. This is what brings the keyboard back to a
            // remote space when its host reconnects, and a tab that appears in the empty
            // space a person is looking at is theirs to see. A sleeping row has no process
            // behind it to type into.
            if state.adopted
                && state.focused == 0
                && state.pending_focus.is_none()
                && let Some(space) = state.space
            {
                state.pending_focus = agents
                    .iter()
                    .find(|r| {
                        r.workspace == space
                            && !state.panes.contains(&r.terminal)
                            && r.state != "sleeping"
                    })
                    .map(|r| r.terminal);
            }
            // A remote that is connecting or offline is still somewhere to be: its space is
            // listed, and it comes back on its own.
            state.done = state.adopted
                && agents.is_empty()
                && !state
                    .spaces
                    .iter()
                    .any(|w| crate::mux::is_remote_workspace(w.id));

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
                // A screen agent that worked and now sits idle while the
                // keyboard is elsewhere finished without ever reporting done.
                // Same news as a daemon done, same bell, same `new` row.
                let was_working =
                    before.is_some_and(|b| matches!(b.as_str(), "working" | "starting"));
                if was_working
                    && matches!(row.state.as_str(), "idle" | "done")
                    && row.terminal != state.focused
                {
                    state
                        .unseen_finished
                        .insert((row.terminal, sidebar::gen_of(row)));
                    // Idle has no daemon done word to carry the news, so ring
                    // here the way done does above.
                    if row.state == "idle" {
                        state.ring = true;
                        state.message = Some(format!("terminal {} finished", row.terminal));
                    }
                }
                // Back to work, or under the keyboard: no longer news.
                if matches!(
                    row.state.as_str(),
                    "working" | "starting" | "blocked" | "failed"
                ) || row.terminal == state.focused
                {
                    state
                        .unseen_finished
                        .remove(&(row.terminal, sidebar::gen_of(row)));
                }
            }
            state.was = agents
                .iter()
                .map(|r| (r.terminal, r.state.clone()))
                .collect();
            state.rows = agents;
            // A slept tab never renders, so the first render that clears
            // "connecting" never comes for it: drop the word and let the
            // notice row name the tomb and its way out.
            if state.message.as_deref() == Some(CONNECTING)
                && state
                    .rows
                    .iter()
                    .any(|r| r.terminal == state.focused && r.state == "sleeping")
            {
                state.message = None;
            }
            // The keyboard lives in the space on screen. A focused terminal
            // elsewhere is typing somewhere the person cannot see; bootstrap
            // races (a render adopted before the index named its space) land
            // here, never in front of the person. An in-flight deferred focus
            // is left alone.
            if state.pending_focus.is_none()
                && let Some(space) = state.space
                && state.focused != 0
            {
                let here = state
                    .rows
                    .iter()
                    .any(|r| r.terminal == state.focused && r.workspace == space);
                if !here {
                    state.pending_focus = state.tabs().first().map(|r| r.terminal);
                }
            }
            // The index changed while the list is open: fetch again. A state
            // word comparison cannot trigger this, because a tool event
            // arrives with the state still `working`.
            if state.focus {
                state.focus_fetch = true;
            }
            // The read receipts follow the rows: entries for terminals that are
            // gone or no longer finished leave, and a finished row under the
            // keyboard counts as read without waiting for a focus change.
            state.seen_done.retain(|(terminal, generation)| {
                state.rows.iter().any(|r| {
                    r.terminal == *terminal
                        && sidebar::gen_of(r) == *generation
                        && r.state == "done"
                })
            });
            state.unseen_finished.retain(|(terminal, generation)| {
                state.rows.iter().any(|r| {
                    r.terminal == *terminal
                        && sidebar::gen_of(r) == *generation
                        && matches!(r.state.as_str(), "idle" | "done")
                        && *terminal != state.focused
                })
            });
            if let Some(row) = state
                .rows
                .iter()
                .find(|r| r.terminal == state.focused && r.state == "done")
            {
                state.seen_done.insert((row.terminal, sidebar::gen_of(row)));
            }
            if let Some(row) = state.rows.iter().find(|r| {
                r.terminal == state.focused && matches!(r.state.as_str(), "idle" | "done")
            }) {
                state
                    .unseen_finished
                    .remove(&(row.terminal, sidebar::gen_of(row)));
            }
            // Heal the bootstrap gap: the very first answers can arrive in any
            // order, and a focus without a tree paints the legacy single pane
            // while watching nothing the sync below can see. Trees are only
            // seeded here, never disturbed: an existing layout is left alone.
            // Subscriptions are NOT filled here: filling `subs` without a
            // `Subscribe` on the wire makes the next `sync_subs` believe the
            // daemon is already sending, and the pane stays dark. The first
            // real focus or space entry sends the subscribe.
            if state.focused != 0 {
                ensure_layout(state);
            }
            *dirty = true;
        }
        ControlResponse::TerminalCreated { id, terminal, .. } => {
            // Split panes bind by id correlation: only a `split-` answer may
            // claim a placeholder, and it claims the one in the space the
            // request was made in. A racing `new-` tab must not steal it, and
            // a split answer landing after a space switch binds home rather
            // than stranding a foreign terminal in the tree on screen.
            let is_split = id.starts_with("split-");
            let origin = if is_split {
                state.split_origin.remove(&id).or(state.space)
            } else {
                None
            };
            let bound = is_split
                && origin
                    .and_then(|space| state.layouts.get_mut(&space))
                    .is_some_and(|layout| layout.bind_placeholder(terminal));
            if bound {
                state.panes.insert(terminal);
            } else if !is_split {
                // The caller subscribes: a new terminal that the client is not
                // watching draws the terminal it came from, which is what the
                // first version did.
                state.snapshot = None;
            }
            // An unbound split stays where the keyboard is: yanking the focus
            // to a terminal in another space would collapse that space's tree
            // onto one pane.
            if bound && origin == state.space || !is_split {
                state.pending_focus = Some(terminal);
            }
            *dirty = true;
        }
        ControlResponse::TerminalClosed { terminal, .. } => {
            // Out of every tree, so no space keeps a pane nobody can focus.
            // Trees left with no leaves and no pending split go away, and the
            // unsubscription of a tile nobody watches piggybacks on the next
            // sync: the daemon already ended the terminal itself.
            state.panes.remove(&terminal);
            for layout in state.layouts.values_mut() {
                layout.remove(terminal);
            }
            state
                .layouts
                .retain(|_, layout| !layout.leaves().is_empty() || layout.has_pending());
            state.snapshots.remove(&terminal);
            // The picture with it. A Kitty placement still on the outer screen
            // comes down on the next painted frame, through the per-frame
            // reconcile. An iTerm2 one cannot be removed remotely (the protocol
            // has no delete), so it stays until the outer terminal clears it;
            // tracking is still dropped here so later frames cannot retry what
            // cannot work.
            state.images.remove(&terminal);
            // The daemon ended the terminal, which ends its subscription
            // server-side too: pruning the local set without a goodbye write.
            state.subs.remove(&terminal);
            if terminal == state.focused {
                state.snapshot = None;
                // From this space, and through the subscription path, which is why it is a
                // pending focus rather than an assignment. Picking any terminal from
                // `rows` put the keyboard in another space without moving the tab bar
                // there, and assigning it directly left the client unsubscribed, so the
                // pane stayed empty until something else happened to move the focus.
                // The tree already picked the neighbour; the tab list is the fallback.
                state.focused = 0;
                state.pending_focus = state
                    .space
                    .and_then(|space| state.layouts.get(&space))
                    .and_then(|layout| {
                        layout
                            .visible()
                            .into_iter()
                            .find(|t| state.rows.iter().any(|r| r.terminal == *t))
                    })
                    .or_else(|| {
                        state
                            .rows
                            .iter()
                            .find(|r| {
                                r.terminal != terminal
                                    && state.space.is_none_or(|s| r.workspace == s)
                            })
                            .map(|r| r.terminal)
                    });
                if state.pending_focus.is_none() {
                    state.message =
                        Some("this space is empty, ctrl-a c starts a terminal".to_string());
                }
            }
            *dirty = true;
        }
        ControlResponse::Failed { id, reason, .. } => {
            // A space that was refused is not coming: it no longer counts toward the next
            // numbered name.
            if id.starts_with("space-") {
                state.creating = state.creating.saturating_sub(1);
            }
            // A split whose spawn failed rolls its tree entry back where it
            // was asked for: one failure must not clear another space's
            // split still on its way, and a rollback on screen only would
            // leak the origin's placeholder forever.
            if id.starts_with("split-")
                && let Some(space) = state.split_origin.remove(&id).or(state.space)
                && let Some(layout) = state.layouts.get_mut(&space)
            {
                layout.drop_placeholder();
            }
            // A refused focus fetch is silence, not news: an older daemon
            // answers every one, and shouting on each would shout on every
            // index while open. The list keeps what it has.
            if !id.starts_with("focus-") {
                // Shown rather than swallowed. A refusal a person cannot see is a client that
                // looks broken instead of a daemon that said no.
                state.message = Some(reason);
            }
            *dirty = true;
        }
        ControlResponse::Subscribed { .. } => {}

        ControlResponse::FocusEvents {
            terminal, events, ..
        } if state.focus && terminal == state.focused => {
            // A listing for the terminal on screen, while the list is open. A
            // listing for anywhere else is a race with a focus change, and one
            // with the list closed is a race with its toggle: both are
            // dropped rather than painted under the wrong tab.
            state.focus_events = events;
            // A background refetch must not yank a scrolled-up reader to the
            // tail: the offset stays, clamped to what the new list holds.
            state.focus_offset = state
                .focus_offset
                .min(state.focus_events.len().saturating_sub(focus_height(state)));
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
            readiness: "ready".to_string(),
            source: None,
            rule: None,
            manifest: None,
            agent_session: None,
            agent_title: None,
            last_active_at_unix: None,
            cwd: None,
            slept_at_unix: None,
        }
    }

    /// A divider drag arms only on the divider: the open column actually
    /// drawn, no overlay, exactly the rule column inside the painted rows.
    /// The foot row arms too, so the press never falls through to the fold
    /// mark the divider also crosses.
    #[test]
    fn a_divider_drag_arms_only_on_the_divider() {
        let mut s = state();
        s.palette = None;
        // Divider at width + 1, one based, rows 1 through lines - STATUS.
        assert!(s.divider_press(31, 5));
        assert!(s.divider_press(31, 22));
        assert_eq!(s.sidebar_drag, Some(sidebar::WIDTH));

        // Pane, titles, and outside the painted rows: nothing arms.
        let mut s = state();
        assert!(!s.divider_press(32, 5));
        assert!(!s.divider_press(30, 5));
        assert!(!s.divider_press(31, 0));
        assert!(!s.divider_press(31, 23));
        assert_eq!(s.sidebar_drag, None);

        // Folded, overlaid, or too narrow to draw the column: nothing arms.
        let mut s = state();
        s.collapsed = true;
        assert!(!s.divider_press(5, 5));
        let mut s = state();
        s.menu = Some(0);
        assert!(!s.divider_press(31, 5));
        let mut s = state();
        s.cols = 10;
        assert!(!s.divider_press(31, 5));
    }

    /// Motions clamp to the sidebar range, latch past the pane, ignore
    /// shift, and report whether anything moved: equal widths repaint and
    /// reflow nothing.
    #[test]
    fn divider_motions_clamp_latch_and_report_movement() {
        let mut s = state();
        s.palette = None;
        assert!(s.divider_press(31, 5));
        // Pull left past the end: clamps, and says so.
        assert!(s.divider_motion(5, false));
        assert_eq!(s.sidebar_width, sidebar::SIDEBAR_MIN);
        // Stuck at the clamp: same width reports nothing.
        assert!(!s.divider_motion(1, false));
        assert_eq!(s.sidebar_width, sidebar::SIDEBAR_MIN);
        // Past the pane edge the drag stays latched.
        assert!(s.divider_motion(70, false));
        assert_eq!(s.sidebar_width, sidebar::SIDEBAR_MAX);
        // Shift motions are ignored while the drag stays armed.
        assert!(!s.divider_motion(20, true));
        assert_eq!(s.sidebar_width, sidebar::SIDEBAR_MAX);
        assert!(s.sidebar_drag.is_some());
        // Back inside: moves again.
        assert!(s.divider_motion(25, false));
        assert_eq!(s.sidebar_width, 24);
    }

    /// Release ends the drag anywhere and answers whether the pull moved
    /// the panel: only then is a refetch owed. Without an arm there is
    /// nothing to finish.
    #[test]
    fn divider_release_reports_movement_only() {
        let mut s = state();
        s.palette = None;
        assert!(!s.finish_drag());
        assert!(s.divider_press(31, 5));
        assert!(!s.finish_drag(), "a motionless press moved nothing");
        assert!(s.divider_press(31, 5));
        assert!(s.divider_motion(40, false));
        assert!(s.finish_drag());
        assert!(s.sidebar_drag.is_none());
    }

    /// The panel wakes the loop only for a tier boundary, never to animate:
    /// rows without a stamp and rows past every boundary ask for nothing.
    #[test]
    fn only_a_future_tier_boundary_asks_for_a_tick() {
        // No stamp anywhere: nothing to cross, no deadline.
        for state in ["working", "starting", "idle", "blocked", "failed", "done"] {
            assert_eq!(
                sidebar::wants_tick(&[agent(1, 1, "claude", state)], 1_750_000_000),
                None,
                "{state} without a stamp must not arm the loop"
            );
        }
        assert_eq!(sidebar::wants_tick(&[], 1_750_000_000), None);
        let mut shell = agent(2, 1, "sh", "working");
        shell.agent = false;
        assert_eq!(
            sidebar::wants_tick(&[shell], 1_750_000_000),
            None,
            "a non-agent row must not arm the loop"
        );
        // Stamped long ago, every boundary past: nothing left to cross.
        let mut old = agent(3, 1, "claude", "idle");
        old.last_active_at_unix = Some(1_000_000);
        assert_eq!(
            sidebar::wants_tick(&[old], 1_750_000_000),
            None,
            "a row past every boundary must not arm the loop"
        );
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
            focus: false,
            focus_events: Vec::new(),
            focus_offset: 0,
            focus_fetch: false,
            reading: None,
            wants_terminal_in: Vec::new(),
            adopted: true,
            adopted_resize: false,
            creating: 0,
            done: false,

            view: sidebar::View::default(),
            seen_done: sidebar::Seen::new(),
            unseen_finished: sidebar::Unseen::new(),
            tick: 0,
            paint_now: 0,
            sidebar_width: sidebar::WIDTH,
            sidebar_drag: None,
            sidebar_sized: sidebar::WIDTH,
            palette: Some(Palette::open()),
            palette_pending: Vec::new(),
            selection: None,
            anchor: None,
            behind: Behind::default(),
            snapshots: std::collections::BTreeMap::new(),
            images: std::collections::BTreeMap::new(),
            graphics: crate::graphics::GraphicsCap::off(),
            shown_images: std::collections::BTreeMap::new(),
            img_size: (0, 0),
            subs: std::collections::BTreeSet::new(),
            layouts: std::collections::BTreeMap::new(),
            panes: std::collections::BTreeSet::new(),
            split_origin: std::collections::BTreeMap::new(),
        }
    }

    /// A new tab follows the focused terminal's directory,
    /// so it opens where the person is rather than where the daemon started.
    #[test]
    fn a_new_terminal_follows_the_focused_terminal_directory() {
        let mut s = state();
        let dir = std::env::temp_dir().display().to_string();
        s.rows
            .iter_mut()
            .find(|r| r.terminal == s.focused)
            .expect("focused row")
            .cwd = Some(dir.clone());
        assert_eq!(focused_cwd(&s), Some(dir));
    }

    #[test]
    fn a_new_terminal_without_a_focused_directory_falls_back_to_the_daemon() {
        // Nothing focused: no row to follow.
        let mut s = state();
        s.focused = 0;
        assert_eq!(focused_cwd(&s), None);
        // Focused but unknown: the index has not reported a directory yet.
        let s = state();
        assert_eq!(focused_cwd(&s), None);
    }

    #[test]
    fn a_new_terminal_ignores_a_stale_or_relative_directory() {
        // Vanished: sending it would fail the create instead of opening a tab.
        let mut s = state();
        s.rows
            .iter_mut()
            .find(|r| r.terminal == s.focused)
            .expect("focused row")
            .cwd = Some(
            std::env::temp_dir()
                .join("qoro-no-such-dir")
                .display()
                .to_string(),
        );
        assert_eq!(focused_cwd(&s), None);
        // Relative: it would resolve against the daemon's directory, which is
        // where the fallback already starts.
        let mut s = state();
        s.rows
            .iter_mut()
            .find(|r| r.terminal == s.focused)
            .expect("focused row")
            .cwd = Some("relative/path".to_string());
        assert_eq!(focused_cwd(&s), None);
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
        let (_, pane_rows) =
            crate::screen::pane_size_for(state.cols, state.lines, false, 0, state.sidebar_width);
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
            snapshots: std::collections::BTreeMap::new(),
            images: std::collections::BTreeMap::new(),
            graphics: crate::graphics::GraphicsCap::off(),
            shown_images: std::collections::BTreeMap::new(),
            img_size: (0, 0),
            subs: std::collections::BTreeSet::new(),
            layouts: std::collections::BTreeMap::new(),
            panes: std::collections::BTreeSet::new(),
            split_origin: std::collections::BTreeMap::new(),
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
            adopted_resize: false,
            done: false,
            prompt: None,
            menu: None,
            collapsed: false,
            focus: false,
            focus_events: Vec::new(),
            focus_offset: 0,
            focus_fetch: false,
            reading: None,
            creating: 0,

            view: sidebar::View::default(),
            seen_done: sidebar::Seen::new(),
            unseen_finished: sidebar::Unseen::new(),
            tick: 0,
            paint_now: 0,
            sidebar_width: sidebar::WIDTH,
            sidebar_drag: None,
            sidebar_sized: sidebar::WIDTH,
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

    fn tiled_state() -> State {
        // Space 1 with two terminals side by side, 1 focused, both watched.
        let mut state = State {
            rows: vec![agent(1, 1, "zsh", "idle"), agent(2, 1, "claude", "working")],
            focused: 1,
            snapshot: Some(snapshot_with_behind(0)),
            snapshots: std::collections::BTreeMap::from([(2, snapshot_with_behind(0))]),
            images: std::collections::BTreeMap::new(),
            graphics: crate::graphics::GraphicsCap::off(),
            shown_images: std::collections::BTreeMap::new(),
            img_size: (0, 0),
            subs: std::collections::BTreeSet::from([1, 2]),
            layouts: std::collections::BTreeMap::new(),
            panes: std::collections::BTreeSet::new(),
            split_origin: std::collections::BTreeMap::new(),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            pending_focus: None,
            spaces: vec![space(1, "app")],
            space: Some(1),
            was: Default::default(),
            ring: false,
            wants_terminal_in: Vec::new(),
            adopted: true,
            adopted_resize: false,
            creating: 0,
            done: false,
            prompt: None,
            menu: None,
            collapsed: false,
            focus: false,
            focus_events: Vec::new(),
            focus_offset: 0,
            focus_fetch: false,
            reading: None,

            view: sidebar::View::default(),
            seen_done: sidebar::Seen::new(),
            unseen_finished: sidebar::Unseen::new(),
            tick: 0,
            paint_now: 0,
            sidebar_width: sidebar::WIDTH,
            sidebar_drag: None,
            sidebar_sized: sidebar::WIDTH,
            palette: None,
            palette_pending: Vec::new(),
            selection: None,
            anchor: None,
            behind: Behind::default(),
        };
        let area = pane_area(&state);
        let mut layout = Layout::single(1);
        assert!(layout.split_placeholder(1, layout::Dir::V, area));
        assert!(layout.bind_placeholder(2));
        layout.focus_terminal(1);
        state.layouts.insert(1, layout);
        state
    }

    #[test]
    fn moving_focus_inside_the_visible_set_resubscribes_nothing() {
        let mut state = tiled_state();
        let mut control_seq = 0;
        // Already watching exactly the visible set.
        assert!(sync_subs(&mut state, &mut control_seq).unwrap().is_none());
        let mut seqs = std::collections::BTreeMap::new();
        let before = control_seq;
        let out = focus(&mut state, 2, &mut control_seq, &mut seqs)
            .unwrap()
            .expect("focus moves");
        // No control traffic: only the resize to the newly focused tile.
        assert_eq!(control_seq, before, "a subscribe slipped in: {out:?}");
        assert!(!out.is_empty(), "the focused tile was never resized");
        assert_eq!(state.focused, 2);
    }

    #[test]
    fn focusing_a_visible_pane_promotes_its_snapshot() {
        let mut state = tiled_state();
        let mut seqs = std::collections::BTreeMap::new();
        let mut control_seq = 0;
        focus(&mut state, 2, &mut control_seq, &mut seqs).unwrap();
        // Terminal 2's picture was already here and is now the screen; terminal
        // 1's waits in the map instead of being thrown away.
        assert_eq!(state.snapshot.as_ref().map(|s| s.behind), Some(0));
        assert!(state.snapshots.contains_key(&1));
        assert!(!state.snapshots.contains_key(&2));
    }

    #[test]
    fn focusing_a_tab_outside_the_tree_drops_back_to_a_single_pane() {
        let mut state = tiled_state();
        state.rows.push(agent(3, 1, "sh", "idle"));
        let mut seqs = std::collections::BTreeMap::new();
        let mut control_seq = 0;
        focus(&mut state, 3, &mut control_seq, &mut seqs).unwrap();
        assert_eq!(state.layouts.get(&1).map(|l| l.leaves()), Some(vec![3]));
        assert_eq!(state.subs, std::collections::BTreeSet::from([3]));
    }

    #[test]
    fn a_closed_terminal_leaves_no_zombie_pane() {
        let mut state = tiled_state();
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::TerminalClosed {
                id: "x".to_string(),
                terminal: 2,
            },
            &mut dirty,
        );
        assert_eq!(state.layouts.get(&1).map(|l| l.leaves()), Some(vec![1]));
        assert!(!state.snapshots.contains_key(&2));
        assert!(!state.subs.contains(&2));
    }

    #[test]
    fn a_failed_split_rolls_its_tree_entry_back() {
        let mut state = tiled_state();
        let area = pane_area(&state);
        // The tree holds two bound leaves, so this placeholder lands on
        // terminal 1's tile.
        assert!(
            state
                .layouts
                .get_mut(&1)
                .unwrap()
                .split_placeholder(1, layout::Dir::H, area)
        );
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::Failed {
                id: "split-9".to_string(),
                reason: "no".to_string(),
            },
            &mut dirty,
        );
        assert!(!state.layouts.get(&1).unwrap().has_pending());
        assert_eq!(state.message.as_deref(), Some("no"));
    }

    #[test]
    fn clicks_find_their_tile_and_dividers_find_none() {
        let state = tiled_state();
        let left = screen::left_for(80, false, sidebar::WIDTH);
        let first_col = left + 1;
        // Top-left cell of the pane is terminal 1's tile.
        let hit = state.tile_at_screen((screen::TABS + 1) as u16, first_col as u16);
        assert_eq!(hit.map(|h| h.0), Some(1));
        // The divider column between the two tiles belongs to no pane.
        let rects = state.layouts.get(&1).unwrap().tile_rects(pane_area(&state));
        let left_rect = rects.iter().find(|(t, _)| *t == 1).unwrap().1;
        let divider_col = (first_col + left_rect.cols) as u16;
        assert_eq!(
            state.tile_at_screen((screen::TABS + 1) as u16, divider_col),
            None
        );
        // And the focused tile answers tile-local coordinates.
        assert_eq!(
            state.in_pane((screen::TABS + 1) as u16, first_col as u16),
            Some((0, 0))
        );
    }

    #[test]
    fn an_empty_pane_names_no_tile() {
        let mut state = tiled_state();
        state.focused = 0;
        let left = screen::left_for(80, false, sidebar::WIDTH);
        assert_eq!(
            state.tile_at_screen((screen::TABS + 1) as u16, (left + 1) as u16),
            None
        );
        assert_eq!(
            state.in_pane((screen::TABS + 1) as u16, (left + 1) as u16),
            None
        );
    }

    #[test]
    fn a_new_space_starts_bare_and_forgets_the_old_subscriptions() {
        let mut state = tiled_state();
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::WorkspaceCreated {
                id: "w".to_string(),
                workspace: 2,
            },
            &mut dirty,
        );
        assert!(!state.layouts.contains_key(&2));
        assert!(state.subs.is_empty());
        assert!(state.snapshots.is_empty());
        assert_eq!(state.wants_terminal_in, vec![2]);
    }

    #[test]
    fn binds_only_happen_in_the_space_on_screen() {
        let mut state = tiled_state();
        state.space = Some(1);
        state.layouts.insert(2, Layout::single(9));
        let area = pane_area(&state);
        state
            .layouts
            .get_mut(&1)
            .unwrap()
            .split_placeholder(1, layout::Dir::H, area);
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::TerminalCreated {
                id: "split-1".to_string(),
                terminal: 3,
            },
            &mut dirty,
        );
        assert!(state.layouts.get(&1).unwrap().contains(3));
        assert!(!state.layouts.get(&2).unwrap().contains(3));
        assert!(
            state.panes.contains(&3),
            "bound splits hide from the tab bar"
        );
        state.rows.push(agent(3, 1, "zsh", "idle"));
        assert!(!state.tabs().iter().any(|r| r.terminal == 3));
        assert!(state.rows.iter().any(|r| r.terminal == 3));
    }

    #[test]
    fn a_racing_new_tab_does_not_steal_a_split_placeholder() {
        let mut state = tiled_state();
        state.space = Some(1);
        let area = pane_area(&state);
        state
            .layouts
            .get_mut(&1)
            .unwrap()
            .split_placeholder(1, layout::Dir::H, area);
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::TerminalCreated {
                id: "new-1".to_string(),
                terminal: 3,
            },
            &mut dirty,
        );
        assert!(!state.layouts.get(&1).unwrap().contains(3));
        assert!(!state.panes.contains(&3));
        assert!(state.layouts.get(&1).unwrap().has_pending());
    }

    #[test]
    fn a_split_answer_after_a_space_switch_binds_home_without_yanking_focus() {
        let mut state = tiled_state();
        state.space = Some(1);
        let area = pane_area(&state);
        state
            .layouts
            .get_mut(&1)
            .unwrap()
            .split_placeholder(1, layout::Dir::H, area);
        state.split_origin.insert("split-9".to_string(), 1);
        // Switched away while the spawn was on its way.
        state.space = Some(2);
        state.layouts.insert(2, Layout::single(9));
        state.focused = 9;
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::TerminalCreated {
                id: "split-9".to_string(),
                terminal: 3,
            },
            &mut dirty,
        );
        assert!(state.layouts.get(&1).unwrap().contains(3));
        assert!(!state.layouts.get(&2).unwrap().contains(3));
        assert!(state.panes.contains(&3));
        assert_eq!(state.focused, 9, "focus stays where the keyboard is");
        assert_eq!(state.pending_focus, None);
        assert!(!state.split_origin.contains_key("split-9"));
    }

    #[test]
    fn a_failed_split_after_a_space_switch_rolls_back_home() {
        let mut state = tiled_state();
        state.space = Some(1);
        let area = pane_area(&state);
        state
            .layouts
            .get_mut(&1)
            .unwrap()
            .split_placeholder(1, layout::Dir::H, area);
        state.split_origin.insert("split-9".to_string(), 1);
        state.space = Some(2);
        state.layouts.insert(2, Layout::single(9));
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::Failed {
                id: "split-9".to_string(),
                reason: "no".to_string(),
            },
            &mut dirty,
        );
        assert!(!state.layouts.get(&1).unwrap().has_pending());
        assert!(state.layouts.get(&2).unwrap().contains(9));
        assert!(!state.split_origin.contains_key("split-9"));
    }

    /// Only the `+` corner of the spaces title makes a space: the word
    /// itself is a no-op, so a click aimed at the title never makes a space
    /// nobody asked for.
    #[test]
    fn only_the_plus_corner_of_the_spaces_title_makes_a_space() {
        let mut state = bare_state();
        state.cols = 100;
        state.lines = 30;
        state.spaces = vec![qoro_runtime::control::WorkspaceRow {
            id: 1,
            name: "default".to_string(),
            terminals: 0,
        }];
        state.space = Some(1);
        assert!(matches!(
            state.clicked(
                crate::screen::SIDEBAR_TOP as u16,
                crate::sidebar::WIDTH as u16
            ),
            Some(Clicked::NewSpace)
        ));
        assert!(
            state
                .clicked(crate::screen::SIDEBAR_TOP as u16, 5)
                .is_none()
        );
    }

    #[test]
    fn menu_moves_with_arrows_and_jk_and_jumps_by_key() {
        let mut state = bare_state();
        state.menu = Some(0);
        assert!(matches!(menu_key(&mut state, 0, b"j"), MenuOutcome::Moved));
        assert_eq!(state.menu, Some(1));
        assert!(matches!(menu_key(&mut state, 1, b"k"), MenuOutcome::Moved));
        assert_eq!(state.menu, Some(0));
        // A row's own key wins over first letters: `r` is rename tab.
        assert!(matches!(menu_key(&mut state, 5, b"r"), MenuOutcome::Moved));
        assert_eq!(state.menu, Some(2));
        assert!(matches!(menu_key(&mut state, 0, b"?"), MenuOutcome::Moved));
        assert_eq!(state.menu, Some(9));
        // Keyless rows answer to their first letter.
        assert!(matches!(menu_key(&mut state, 7, b"s"), MenuOutcome::Moved));
        assert_eq!(state.menu, Some(10));
        // `x` jumps straight to close tab.
        assert!(matches!(menu_key(&mut state, 0, b"x"), MenuOutcome::Moved));
        assert_eq!(state.menu, Some(4));
        // Enter chooses, esc closes.
        assert!(matches!(
            menu_key(&mut state, 8, b"\r"),
            MenuOutcome::Chose(8)
        ));
        assert_eq!(state.menu, None);
        state.menu = Some(3);
        assert!(matches!(
            menu_key(&mut state, 3, b"\x1b"),
            MenuOutcome::Closed
        ));
        assert_eq!(state.menu, None);
    }

    #[test]
    fn every_menu_row_names_a_dispatched_choice() {
        // The dispatch matches on the same names, so a row added without an
        // arm fails here instead of dying silently behind `_`.
        let names: Vec<&str> = MENU.iter().map(|(name, _, _)| *name).collect();
        assert_eq!(
            names,
            vec![
                "new tab",
                "new space",
                "rename tab",
                "rename space",
                "close tab",
                "split side by side",
                "split stacked",
                "zoom pane",
                "toggle agents view",
                "keybinds",
                "settings",
                "reload manifests",
                "detach",
            ]
        );
    }

    #[test]
    fn choosing_close_tab_asks_to_close_the_focused_terminal() {
        let mut state = bare_state();
        state.focused = 2;
        let mut request = 0;
        let item = MENU.iter().position(|(n, _, _)| *n == "close tab").unwrap();
        match apply_menu_choice(&mut state, item, &mut request) {
            MenuSend::Control(ControlRequest::CloseTerminal { terminal, .. }) => {
                assert_eq!(terminal, 2);
            }
            other => panic!(
                "close tab sent nothing useful: {}",
                matches!(other, MenuSend::Nothing)
            ),
        }
        // Nothing focused is a message, not a request.
        let mut state = bare_state();
        state.focused = 0;
        let mut request = 0;
        assert!(matches!(
            apply_menu_choice(&mut state, item, &mut request),
            MenuSend::Nothing
        ));
        assert_eq!(state.message.as_deref(), Some("nothing to close"));
    }

    #[test]
    fn choosing_reload_says_so_and_renaming_nothing_says_so() {
        let mut state = bare_state();
        let mut request = 0;
        let reload = MENU
            .iter()
            .position(|(n, _, _)| *n == "reload manifests")
            .unwrap();
        assert!(matches!(
            apply_menu_choice(&mut state, reload, &mut request),
            MenuSend::Control(ControlRequest::ReloadAgentManifests { .. })
        ));
        assert_eq!(state.message.as_deref(), Some("reloading agent manifests"));
        let rename = MENU
            .iter()
            .position(|(n, _, _)| *n == "rename tab")
            .unwrap();
        state.focused = 0;
        assert!(matches!(
            apply_menu_choice(&mut state, rename, &mut request),
            MenuSend::Nothing
        ));
        assert!(state.message.is_some(), "silent no-op");
    }

    #[test]
    fn choosing_zoom_fold_and_view_answers() {
        // The zoom row only asks: the tree flips and the bytes go out at the
        // call sites, because they own the sequence numbers.
        let mut state = bare_state();
        let mut request = 0;
        let zoom = MENU.iter().position(|(n, _, _)| *n == "zoom pane").unwrap();
        assert!(matches!(
            apply_menu_choice(&mut state, zoom, &mut request),
            MenuSend::Zoom
        ));
        // View flips locally and sends nothing.
        let view = MENU
            .iter()
            .position(|(n, _, _)| *n == "toggle agents view")
            .unwrap();
        let before = state.view;
        assert!(matches!(
            apply_menu_choice(&mut state, view, &mut request),
            MenuSend::Nothing
        ));
        assert_ne!(state.view, before, "view toggles");
    }

    #[test]
    fn zoom_flip_hides_a_tile_and_unflips_cleanly() {
        // Two tiles side by side: zooming in hides one and resizes the other.
        let mut state = tiled_state();
        let mut control_seq = 0;
        let mut seqs = std::collections::BTreeMap::new();
        let (changed, out) = zoom_flip(&mut state, &mut control_seq, &mut seqs).unwrap();
        assert!(changed, "two tiles zoom");
        assert!(out.is_some(), "hide and resize go out");
        assert_eq!(
            state.layouts.get(&1).unwrap().zoomed,
            Some(1),
            "focused tile fills the pane"
        );
        // Back out: the hidden tile resubscribes.
        let (changed, out) = zoom_flip(&mut state, &mut control_seq, &mut seqs).unwrap();
        assert!(changed, "unzoom answers too");
        assert!(out.is_some(), "show and resize go out");
        assert_eq!(state.layouts.get(&1).unwrap().zoomed, None);
    }

    #[test]
    fn a_menu_click_lands_on_a_row_or_outside_it() {
        let cols = 80;
        let lines = 24;
        let Some((left, top, w, _)) = crate::screen::menu_rect(cols, lines) else {
            panic!("no menu rect on 80x24");
        };
        // The third row's middle cell.
        let row = (top + 2 + 2 + 1) as u16;
        let col = (left + w / 2 + 1) as u16;
        let press = format!("\x1b[<0;{col};{row}M").into_bytes();
        assert_eq!(menu_click(&press, cols, lines), Some(Some(2)));
        // Far away from the centred box is outside.
        assert_eq!(menu_click(b"\x1b[<0;1;1M", cols, lines), Some(None));
        // A release is not a choice.
        let release = format!("\x1b[<0;{col};{row}m").into_bytes();
        assert_eq!(menu_click(&release, cols, lines), None);
    }
    #[test]
    fn a_first_render_from_another_space_never_adopts() {
        let mut state = bare_state();
        state.space = Some(1);
        state.rows = vec![agent(1, 1, "zsh", "working"), agent(2, 2, "zsh", "working")];
        assert!(adopt_render(&state, 1), "same space adopts");
        assert!(!adopt_render(&state, 2), "another space does not adopt");
        assert!(
            !adopt_render(&state, 9),
            "unknown terminal does not adopt behind rows"
        );
        // Space still unknown: the first workspace wins, where the bar parks.
        state.space = None;
        assert!(adopt_render(&state, 1), "first workspace adopts");
        assert!(
            !adopt_render(&state, 2),
            "later workspaces wait for the index"
        );
        state.rows.clear();
        assert!(adopt_render(&state, 9), "nothing listed yet adopts");
    }

    /// Bootstrap parks the space under the keyboard, and a focused terminal
    /// outside the space on screen is pulled back to its first tab: typing
    /// somewhere the person cannot see is never the steady state.
    #[test]
    fn bootstrap_and_index_keep_the_keyboard_in_its_space() {
        // Adoption without a space takes the terminal's workspace with it.
        let mut state = bare_state();
        state.focused = 0;
        state.adopted = false;
        assert_eq!(state.space, None);
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::Agents {
                id: "index".to_string(),
                agents: vec![agent(7, 5, "zsh", "working")],
            },
            &mut dirty,
        );
        assert_eq!(state.focused, 7);
        assert_eq!(state.space, Some(5), "the space follows the adoption");
        // A focused terminal from another space is pulled to this space's
        // first tab through the subscribing path.
        let mut state = bare_state();
        state.space = Some(1);
        state.adopted = true;
        state.focused = 2;
        state.rows = vec![agent(1, 1, "zsh", "working"), agent(2, 2, "zsh", "working")];
        let rows = state.rows.clone();
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::Agents {
                id: "index".to_string(),
                agents: rows,
            },
            &mut dirty,
        );
        assert_eq!(
            state.pending_focus,
            Some(1),
            "the keyboard comes back to its space: {:?}",
            state.pending_focus
        );
    }

    /// Requests are numbered from the clock, not from zero: a run from zero
    /// reused an earlier run's `close-N` and the ledger replayed its answer.
    #[test]
    fn requests_are_numbered_from_the_clock() {
        // 2020-01-01 in nanoseconds: far past any id a run from zero reached.
        assert!(request_seed() > 1_577_836_800 * 1_000_000_000);
    }

    /// A slept tab never renders, so "connecting" must not wait for one: the
    /// index clears it once the focused row is known to be sleeping, and
    /// leaves it for a live row whose render is still on its way.
    #[test]
    fn a_focused_sleeping_tab_does_not_stay_connecting() {
        for (state_word, cleared) in [("sleeping", true), ("idle", false)] {
            let mut state = bare_state();
            state.space = Some(1);
            state.adopted = true;
            state.focused = 1;
            state.message = Some(CONNECTING.to_string());
            let mut dirty = false;
            apply(
                &mut state,
                ControlResponse::Agents {
                    id: "index".to_string(),
                    agents: vec![agent(1, 1, "zsh", state_word)],
                },
                &mut dirty,
            );
            assert_eq!(state.message.is_none(), cleared, "{state_word}");
        }
    }

    /// The first workspace list parks the bar: a keyboard adopted from
    /// another space comes home on the spot, not on the next index, which an
    /// idle session never sends.
    #[test]
    fn the_first_workspace_list_brings_a_stray_keyboard_home() {
        let mut state = bare_state();
        state.adopted = true;
        state.focused = 2;
        state.rows = vec![agent(1, 1, "zsh", "working"), agent(2, 2, "zsh", "working")];
        assert_eq!(state.space, None);
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::Workspaces {
                id: "ws".to_string(),
                workspaces: vec![space(1, "default"), space(2, "other")],
            },
            &mut dirty,
        );
        assert_eq!(state.space, Some(1));
        assert_eq!(
            state.pending_focus,
            Some(1),
            "the stray keyboard is sent home: {:?}",
            state.pending_focus
        );
    }

    /// An index arriving while the focus list is open marks a refetch: a tool
    /// event arrives with the state word unchanged, so the state comparison
    /// cannot trigger it.
    #[test]
    fn an_index_while_open_marks_a_refetch() {
        let mut state = bare_state();
        state.focus = true;
        state.focused = 1;
        state.rows = vec![agent(1, 1, "claude", "working")];
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::Agents {
                id: "index".to_string(),
                agents: vec![agent(1, 1, "claude", "working")],
            },
            &mut dirty,
        );
        assert!(state.focus_fetch, "the list refetches on the index");
    }

    /// A deferred focus applies through one shared helper: the keyboard moves
    /// with its subscription and resize whether the wake was a keystroke or a
    /// daemon message.
    #[tokio::test]
    async fn a_deferred_focus_moves_through_the_shared_drain() {
        let mut state = bare_state();
        state.space = Some(1);
        state.rows = vec![agent(1, 1, "zsh", "working"), agent(2, 1, "zsh", "working")];
        state.focused = 1;
        state.layouts.insert(1, Layout::single(1));
        state.pending_focus = Some(2);
        let mut seqs = std::collections::BTreeMap::new();
        let mut control_seq = 0u32;
        let mut wr = Vec::new();
        let mut dirty = false;
        drain_pending_focus(&mut state, &mut control_seq, &mut seqs, &mut wr, &mut dirty)
            .await
            .expect("drain writes");
        assert_eq!(state.focused, 2, "the keyboard moved");
        assert!(state.pending_focus.is_none(), "the deferral is consumed");
        assert!(!wr.is_empty(), "subscribe and resize went out");
        assert!(dirty);
    }
    fn tool_event(label: &str) -> qoro_runtime::control::FocusEvent {
        qoro_runtime::control::FocusEvent {
            kind: "tool".to_string(),
            label: label.to_string(),
            at_unix: 0,
        }
    }

    #[test]
    fn the_focus_list_scrolls_with_arrows_and_j_k() {
        let mut state = bare_state();
        // Three pane rows: five events do not fit.
        state.lines = 6;
        state.focus = true;
        state.focus_events = (0..5).map(|i| tool_event(&format!("tool {i}"))).collect();
        assert!(matches!(
            focus_key(&mut state, b"\x1b[A"),
            FocusOutcome::Scrolled
        ));
        assert_eq!(state.focus_offset, 1);
        assert!(state.focus, "scrolling never closes");
        assert!(matches!(
            focus_key(&mut state, b"k"),
            FocusOutcome::Scrolled
        ));
        assert_eq!(state.focus_offset, 2);
        // Clamped at the head: no further.
        assert!(matches!(
            focus_key(&mut state, b"\x1b[A"),
            FocusOutcome::Scrolled
        ));
        assert_eq!(state.focus_offset, 2);
        assert!(matches!(
            focus_key(&mut state, b"j"),
            FocusOutcome::Scrolled
        ));
        assert_eq!(state.focus_offset, 1);
        assert!(matches!(
            focus_key(&mut state, b"\x1b[B"),
            FocusOutcome::Scrolled
        ));
        assert_eq!(state.focus_offset, 0);
    }

    #[test]
    fn enter_esc_and_q_close_the_focus_list_without_sending() {
        let mut state = bare_state();
        state.focus = true;
        for bytes in [&b"\r"[..], &b"\n"[..], &b"\x1b"[..], &b"q"[..]] {
            state.focus = true;
            assert!(
                matches!(focus_key(&mut state, bytes), FocusOutcome::Closed),
                "{bytes:?} closes"
            );
            assert!(!state.focus);
        }
    }

    #[test]
    fn typing_closes_the_focus_list_and_reaches_the_terminal() {
        let mut state = bare_state();
        state.focus = true;
        match focus_key(&mut state, b"hello") {
            FocusOutcome::Passthrough(rest) => assert_eq!(rest, b"hello"),
            other => panic!("typing is passed through: {other:?}"),
        }
        assert!(!state.focus);
    }

    #[test]
    fn the_close_toggle_is_swallowed_while_other_prefixes_pass_through() {
        let mut state = bare_state();
        state.focus = true;
        assert!(
            matches!(focus_key(&mut state, b"\x01F"), FocusOutcome::Closed),
            "the toggle that closes is the close itself, never a reopen"
        );
        assert!(!state.focus);
    }

    #[test]
    fn a_focus_listing_for_the_open_terminal_is_kept_and_any_other_is_dropped() {
        let mut state = bare_state();
        state.focus = true;
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::FocusEvents {
                id: "focus-1".to_string(),
                terminal: 1,
                events: vec![tool_event("Read foo.rs")],
            },
            &mut dirty,
        );
        assert!(dirty);
        assert_eq!(state.focus_events.len(), 1);
        // Another terminal's listing is a race with a focus change: dropped.
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::FocusEvents {
                id: "focus-2".to_string(),
                terminal: 9,
                events: vec![tool_event("Read bar.rs")],
            },
            &mut dirty,
        );
        assert!(!dirty);
        assert_eq!(state.focus_events.len(), 1);
        // Closed, a listing is a race with the toggle: dropped.
        state.focus = false;
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::FocusEvents {
                id: "focus-3".to_string(),
                terminal: 1,
                events: vec![],
            },
            &mut dirty,
        );
        assert!(!dirty);
        assert_eq!(state.focus_events.len(), 1);
    }

    #[test]
    fn a_refused_focus_fetch_is_silence_not_news() {
        let mut state = bare_state();
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::Failed {
                id: "focus-1".to_string(),
                reason: "no terminal 1".to_string(),
            },
            &mut dirty,
        );
        assert_eq!(
            state.message, None,
            "an old daemon would shout on every index"
        );
    }

    const R1: i64 = 1 << 40;

    /// A remote that is offline has no terminals but is still somewhere to be: the
    /// interface stays open for it to come back, where without one it would end.
    #[test]
    fn an_offline_remote_keeps_the_interface_open_and_nothing_else_does() {
        let mut state = state();
        state.adopted = true;
        state.spaces = vec![space(1, "space 1"), space(R1 | 1, "devbox:work (offline)")];
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::Agents {
                id: "mux".into(),
                agents: vec![],
            },
            &mut dirty,
        );
        assert!(!state.done, "an offline remote ended the interface");
        state.spaces = vec![space(1, "space 1")];
        apply(
            &mut state,
            ControlResponse::Agents {
                id: "index".into(),
                agents: vec![],
            },
            &mut dirty,
        );
        assert!(
            state.done,
            "with nothing anywhere the interface ends as it always has"
        );
    }

    /// The keyboard comes back when a remote reconnects: nothing focused, the space on
    /// screen is the remote one, and its terminal is listed again.
    #[test]
    fn a_terminal_back_in_the_space_on_screen_takes_the_keyboard() {
        let mut state = state();
        state.adopted = true;
        state.focused = 0;
        state.space = Some(R1 | 1);
        let remote = crate::mux::REMOTE_TERMINAL | 1 << 24 | 3;
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::Agents {
                id: "mux".into(),
                agents: vec![
                    agent(1, 1, "zsh", "idle"),
                    agent(remote, R1 | 1, "claude", "idle"),
                ],
            },
            &mut dirty,
        );
        assert_eq!(state.pending_focus, Some(remote));

        // A sleeping row has nothing behind it to type into.
        let mut state = self::state();
        state.adopted = true;
        state.focused = 0;
        state.space = Some(R1 | 1);
        apply(
            &mut state,
            ControlResponse::Agents {
                id: "mux".into(),
                agents: vec![agent(remote, R1 | 1, "claude", "sleeping")],
            },
            &mut dirty,
        );
        assert_eq!(state.pending_focus, None);
    }

    /// An unnamed space is counted among the spaces of the machine on screen and, on a
    /// remote, carries its prefix, which is what makes it over there.
    #[test]
    fn an_unnamed_space_is_numbered_and_placed_on_the_machine_on_screen() {
        let mut state = state();
        state.spaces = vec![
            space(1, "space 1"),
            space(2, "space 2"),
            space(R1 | 1, "devbox:work"),
        ];
        state.space = Some(1);
        assert_eq!(next_space_name(&state), "space 3");
        state.space = Some(R1 | 1);
        assert_eq!(next_space_name(&state), "devbox:space 2");
        state.creating = 1;
        assert_eq!(next_space_name(&state), "devbox:space 3");
    }

    /// A refused new space stops counting toward the next name.
    #[test]
    fn a_refused_space_no_longer_counts() {
        let mut state = state();
        state.creating = 1;
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::Failed {
                id: "space-4".into(),
                reason: "devbox is offline".into(),
            },
            &mut dirty,
        );
        assert_eq!(state.creating, 0);
    }
}
