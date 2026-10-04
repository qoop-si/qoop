//! The loop: read what the user typed, read what the daemon said, paint when something
//! changed.
//!
//! Nothing here is on a timer. Every wake is an arrival, which is the same rule the daemon
//! holds itself to, and it is the reason an interface with eight terminals open costs
//! nothing while nobody is typing.

use std::io::{self, Write};
use std::path::Path;

use bytes::BytesMut;
use qoro_domain::AgentRuntimeState;
use qoro_runtime::client::{self, Connection, Incoming, TerminalGuard};
use qoro_runtime::control::{AgentRow, ControlRequest, ControlResponse, EventMessage, Place};
use qoro_runtime::daemon::{ClientWire, SnapshotWire, StoredImage};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::keys::{Action, Keys};
use crate::layout::{self, Layout};
use crate::palette::{self, Palette};
use crate::screen::{self, Frame};
use crate::theme::SPACE_INK_COUNT;
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

/// How long an index request may stay unanswered before the next change asks again.
const INDEX_PATIENCE: std::time::Duration = std::time::Duration::from_secs(2);

/// How long a space close waits before it gives up and says what is still in the way.
///
/// Not a refusal and not a hang: the daemon's own teardown ends a process tree in two
/// seconds (`qoro_pty::teardown::TEARDOWN_DEADLINE`), so a close that has not finished by
/// then is not going to, and a person is better told the count than left watching a row
/// that never changes.
const CLOSE_PATIENCE: std::time::Duration = std::time::Duration::from_secs(30);

/// How many times a refused tab is asked again before the chain gives up on it and moves
/// to the next, so the tab is asked one time plus this many. The daemon's only refusal is a command channel that was full at that instant,
/// which a retry fixes; three is enough to tell a full channel from a tab that will not go.
const STUCK_TRIES: usize = 3;

/// How often the column's spinner and sweep step while an agent works: twelve frames a
/// second, enough for a sweep across thirty columns to read as motion rather than jumps.
const MOTION_TICK: std::time::Duration = std::time::Duration::from_millis(80);

/// How often agent costs are sampled while `QORO_USAGE=1`: often enough to follow a
/// build, seldom enough that the percent is a real average and not noise.
const USAGE_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

/// How long a terminal asked for ahead of the index that lists it is waited for: a launcher
/// that opens one and shows it gets there first, and one that never comes is forgotten.
const SHOW_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// How long the terminal under the keyboard is waited for after it went from the index. A
/// remote daemon moving onto a newer qoro ends its tabs and brings them back with the same
/// numbers a few seconds later, and the keyboard goes back to the same one rather than to
/// the first tab of the space.
const FOCUS_RETURN: std::time::Duration = std::time::Duration::from_secs(30);

/// Whether the person asked to see what agents cost: `QORO_USAGE=1`, and nothing else.
fn usage_on() -> bool {
    std::env::var("QORO_USAGE").is_ok_and(|v| v.trim() == "1")
}

/// Work done off the loop's own thread, handed back when it is done.
enum Blocked {
    /// A cost sample: when it was taken, which terminal asked for which group, and what
    /// each group's members had cost by then.
    Usage {
        at: tokio::time::Instant,
        targets: Vec<(u32, i32)>,
        found: std::collections::BTreeMap<i32, Vec<qoro_runtime::ProcessUsage>>,
    },
    /// An agent's last answer read from its transcript, or why it could not be.
    Answer(u32, Result<crate::answer::Answer, String>),
}

/// The newest assistant message of an agent's transcript, as a page, or why there is none.
///
/// Runs on the blocking pool: it reads the agent's own files, and a long transcript takes a
/// moment. The reasons come back in words a person can act on.
fn read_answer(
    program: String,
    session: Option<String>,
    title: String,
) -> Result<crate::answer::Answer, String> {
    last_answer(&program, session.as_deref()).map(|text| crate::answer::Answer::new(title, text))
}

/// The newest assistant message of an agent's transcript, or why there is none.
fn last_answer(program: &str, session: Option<&str>) -> Result<String, String> {
    let dirs = qoro_history::BaseDirs::from_home().map_err(|e| e.to_string())?;
    let window = qoro_history::history(
        program,
        session,
        &dirs,
        1,
        Some(qoro_history::Role::Assistant),
    )
    .map_err(|e| match e {
        qoro_history::Error::MissingId { .. } => {
            "this agent has not reported a session yet: qoro hooks install makes it".to_string()
        }
        other => other.to_string(),
    })?;
    window
        .messages
        .last()
        .map(|m| m.text.clone())
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| "no answer from this agent yet".to_string())
}

/// The local agents whose cost can be sampled, as (terminal, foreground group).
///
/// Another machine's terminals are skipped: a process group number from over there names
/// some other process here.
fn usage_targets(state: &State) -> Vec<(u32, i32)> {
    state
        .rows
        .iter()
        .filter(|r| r.agent && !crate::mux::is_remote_terminal(r.terminal))
        .filter_map(|r| r.pgid.filter(|group| *group > 0).map(|g| (r.terminal, g)))
        .collect()
}

/// Fold one cost sample into what the column shows.
///
/// CPU is taken per process: what each member spent since the previous sample, a member
/// born since then counted from zero, over the wall time between the samples, as a
/// percent of one core. Per process, because a group total falls when a busy member exits
/// and a difference of totals would read a build as idle. A terminal seen for the first
/// time, or whose foreground group changed, has no percent until its next sample.
fn apply_usage(
    state: &mut State,
    at: tokio::time::Instant,
    targets: Vec<(u32, i32)>,
    found: std::collections::BTreeMap<i32, Vec<qoro_runtime::ProcessUsage>>,
) {
    let mut next = sidebar::Usage::new();
    let mut seen = std::collections::BTreeMap::new();
    for (terminal, group) in targets {
        let Some(members) = found.get(&group) else {
            continue;
        };
        let rss = members.iter().map(|(_, _, rss)| rss).sum();
        let now: std::collections::BTreeMap<i32, u64> =
            members.iter().map(|(pid, cpu, _)| (pid.0, *cpu)).collect();
        let cpu_percent = match state.usage_seen.get(&terminal) {
            Some((was, when, before)) if *was == group => {
                let wall = at.duration_since(*when).as_nanos();
                let spent: u64 = now
                    .iter()
                    .map(|(pid, cpu)| cpu.saturating_sub(before.get(pid).copied().unwrap_or(0)))
                    .sum();
                (wall > 0).then(|| (spent as u128 * 100 / wall).min(9999) as u32)
            }
            _ => None,
        };
        seen.insert(terminal, (group, at, now));
        next.insert(terminal, sidebar::Cost { cpu_percent, rss });
    }
    state.usage_seen = seen;
    state.usage = next;
}

/// Whether the column may move at all: yes, unless `QORO_MOTION=0` is set.
/// Motion has its own flag on purpose. `NO_COLOR` stops colour, not movement;
/// the shimmer that needs colour already degrades to a still marker through
/// the colour level, so the two switches never fight.
fn motion_on() -> bool {
    std::env::var("QORO_MOTION").map_or(true, |v| v.trim() != "0")
}

/// Whether the outer terminal reports the mouse usefully, decided from the
/// environment. Terminal.app has no usable mouse reporting, and a terminal
/// that cannot be asked anything (`dumb`, the `linux` console) answers no
/// mouse mode either. Keyboard-only operation always works: the mouse is
/// enhancement, never the input.
fn mouse_on() -> bool {
    mouse_on_for(
        std::env::var("TERM").ok(),
        std::env::var("TERM_PROGRAM").ok(),
    )
}

fn mouse_on_for(term: Option<String>, program: Option<String>) -> bool {
    if term.as_deref() == Some("linux") || term.as_deref() == Some("dumb") {
        return false;
    }
    program.as_deref() != Some("Apple_Terminal")
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
    /// others are live screens sized to their tiles (cropped only when another
    /// client owns the terminal's size).
    snapshots: std::collections::BTreeMap<u32, SnapshotWire>,
    /// The last screen of each terminal this client stopped watching, by terminal. Drawn by
    /// `frame()` in place of a blank pane until the fresh screen arrives, which from another
    /// machine is a round trip or more away, and read by nothing else: every decision still
    /// sees no screen until the daemon sends one. Bounded by the live terminals, since a
    /// terminal that goes takes its entry with it.
    last_seen: std::collections::BTreeMap<u32, SnapshotWire>,
    /// Whether the focused pane may stand in its `last_seen` screen. Set only by `focus()`
    /// and cleared by `forget_screen()`, so a pane emptied for a new terminal or a new space
    /// never shows the screen it held before.
    previewing: bool,
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
    /// The size this client last sent each terminal it lays out, so one whose size did
    /// not change is not sent it again: to the daemon an equal size is a repaint nudge.
    sized: std::collections::BTreeMap<u32, (u16, u16)>,
    /// The tiling tree on screen, per space. Spaces without an entry show a single pane.
    layouts: std::collections::BTreeMap<i64, Layout>,
    /// The other tabs' trees, per space. A tab is a tree: moving to another tab
    /// parks the one on screen here rather than dropping it, and coming back
    /// takes it out again. See `activate`.
    ///
    /// Which terminals are panes is not stored: a pane is any leaf after the
    /// first of a tree on screen or parked, and the first is its tab
    /// (`is_pane`). Agents in panes still list in the sidebar.
    parked: std::collections::BTreeMap<i64, Vec<Layout>>,
    /// Trees to record with the daemon, queued where the change happened and
    /// sent at the top of the loop. Only a person's own split or detach queues
    /// one: pruning and adopting never write, so a stale index or a remote that
    /// reconnects cannot overwrite what is on disk.
    split_writes: Vec<qoro_runtime::control::Pane>,
    /// Recordings sent and not yet answered. While any is out, trees in the
    /// index may predate it and are not adopted.
    split_saving: usize,
    /// A tree came on screen outside a focus change (adopted from the index), so
    /// the loop owes the daemon a subscription sync and a resize.
    resync: bool,
    /// Where each outstanding split was asked for, by request id: the space
    /// and the pane that was split. A split answer carries no workspace, and
    /// the space on screen may have moved since the request, or the tree may
    /// have been parked while another tab of the space has a split of its own
    /// on the way: without this the bind lands in the wrong tree or the
    /// placeholder leaks and blocks every later split. Consumed by the
    /// `TerminalCreated`/`Failed` answer.
    split_origin: std::collections::BTreeMap<String, (i64, u32)>,
    /// Agent commands waiting for their tab, by request id: the line to type once
    /// the shell is there. The daemon only starts a shell (ADR-015), so an agent
    /// is typed the way `qoro new -- <agent>` types it. Consumed by the
    /// `TerminalCreated`/`Failed` answer, with the name the person gave the tab, if
    /// any, which waits for its conversation.
    pending_agents: std::collections::BTreeMap<String, (String, Option<String>)>,
    /// When `config.toml` was last changed, as last seen: checked with the ack (two
    /// seconds), so a change made anywhere holds here with no restart.
    config_stamp: Option<std::time::SystemTime>,
    /// Names owed to the conversations of new agent tabs, by terminal: sent once, as a
    /// session rename, the first time the index shows the agent in a conversation, so the
    /// name follows it through a sleep the way a rename from the box does. Dropped when
    /// the terminal closes first.
    ///
    /// ponytail: a client that quits before the agent reports its session leaves the name
    /// on the tab only, and `/clear` starts a conversation that takes the agent's own title.
    session_titles: std::collections::BTreeMap<u32, String>,
    /// Lines owed to new agent tabs, by terminal. Written at the top of the loop,
    /// where there is always a write in hand; the shell holds the line as
    /// typeahead, so it lands even before its first prompt is drawn.
    agent_typing: Vec<(u32, Vec<u8>)>,
    cols: u16,
    lines: u16,
    message: Option<String>,
    /// A terminal to move to once the loop can write the subscription for it.
    pending_focus: Option<u32>,
    /// The terminal this client showed last time, and where that is kept.
    last: LastFocus,
    /// A terminal to show once an index lists it, and until when: one `qoro focus` named
    /// before this interface listed it (a launcher that opens one and shows it at once gets
    /// there first), or the focused one that went from the index and may come back. Past
    /// its deadline, a terminal that never came is forgotten rather than jumped to.
    ///
    /// The flag is set for the focused one that went: that wait is forgotten the moment the
    /// person puts the keyboard anywhere themselves, so a tab coming back never takes it
    /// from where they went.
    show_when_listed: Option<(u32, std::time::Instant, bool)>,
    /// The id of a create asked for in a space that is not on screen, from a space's `+`.
    /// Its tab is shown once an index lists it, because only the row says which space to
    /// bring on screen with it.
    elsewhere: Option<String>,
    /// A wake asked for from here, as the space and the tab to show once it is back: the
    /// clicked one, or the space's first. Handed to `show_when_listed` by the answer.
    waking: Option<(i64, u32)>,
    /// Which sleeping tab the keyboard rests on while the space on screen is asleep, as
    /// `ctrl-a n/p` picked it. Read through `dim_tab`, which falls back to the first.
    dim: u32,
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
    /// Which list the menu box shows while `menu` is open.
    menu_kind: MenuKind,
    /// The machine being connected from the `+`, between its address and its session.
    connecting: Option<String>,
    /// The id of the session list last asked for. An answer to any other is late, and a
    /// list that opened over whatever the person has moved on to would steal the keys.
    probing: Option<String>,
    /// The id of the install last asked for. Its success is always said; only while it is
    /// still standing does the list of the machine's sessions follow on its own.
    installing: Option<String>,
    /// A machine to ask about its sessions again, once the loop can write: qoro was just
    /// installed there.
    reprobe: Option<String>,
    /// A machine just connected from here, whose first tab the screen moves to when it
    /// arrives. Forgotten as soon as the person goes anywhere themselves.
    joining: Option<String>,
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
    /// An overlay showing something read only: the keys.
    reading: Option<Reading>,
    /// Spaces asked for and not yet answered for.
    ///
    /// Only the naming needs this, and the naming needs it badly: two presses of `ctrl-a w`
    /// before either answer came back both counted the same list and both produced
    /// `space 2`, so the panel showed two rows with one name on them.
    creating: usize,
    /// How the column orders its agents: the tree of spaces, or flat by urgency.
    view: sidebar::View,
    /// The inbox at the top of the column: its list, its height, folded or not. Client-local
    /// like `view`.
    inbox: sidebar::Inbox,
    /// The title row held by the mouse to drag the inbox's edge: the screen row the press
    /// landed on and the list rows the inbox had then.
    inbox_drag: Option<(u16, usize)>,
    /// The spaces folded away under their rule. Client-local like `view`: the daemon
    /// never knows, and a space that goes away leaves this set with the next index.
    shut: sidebar::Shut,
    /// The last press landed in the left column, and no key has gone to the pane since:
    /// the key row names what the column answers to.
    in_column: bool,
    /// `ctrl-a` came last and the next key is a command: the key row names them all.
    prefix_pending: bool,
    /// An agent's last answer, open as a page over the pane.
    answer: Option<crate::answer::Answer>,
    /// What each local agent costs, as last sampled. Empty unless `QORO_USAGE=1`.
    usage: sidebar::Usage,
    /// Each sampled terminal's group, when it was read, and each member's CPU nanoseconds
    /// then, for the next sample's percent.
    usage_seen: std::collections::BTreeMap<
        u32,
        (
            i32,
            tokio::time::Instant,
            std::collections::BTreeMap<i32, u64>,
        ),
    >,
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
    /// The column's animation frame: the working spinner and the activity sweep.
    ///
    /// Zero is the still column. The loop advances it every [`MOTION_TICK`] only while
    /// an agent is working, the column is drawn and motion is on, so a screen with
    /// nothing working never repaints on its own.
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
    /// An agent row held by the mouse: a click when it comes up without moving, a drag
    /// to another space or onto a pane's edge once it moves.
    row_press: Option<RowPress>,
    /// A drop on a pane of another space, waiting for the daemon to move the tab there
    /// first: the move's request id, the terminal, the pane it goes beside, and on
    /// which side.
    drop_after_move: Option<(String, u32, u32, layout::Move)>,
    /// A space being closed, and the tabs still on their way out.
    ///
    /// `sent` is how many of `asked` have been asked, because each one is asked when the
    /// last one answered. The state is kept until the space itself closes or the daemon
    /// refuses, so a refusal says what is still in the way instead of the client having
    /// forgotten it was in the middle of something.
    closing: Option<Closing>,
    /// A close begun by a key rather than a click. The give-up timer is the loop's, and
    /// this is how a key's close reaches it: a close against a daemon that never answers
    /// would otherwise hold the column for ever, because no response would arm the timer.
    close_begun: bool,
    /// The space whose rule is asking whether it should close, once the close has been
    /// asked for and not yet answered.
    ///
    /// The question is on the rule itself rather than on the notice row, because the
    /// person who clicked `x` is looking at the rule and the answer is a second click on
    /// the same two columns. `None` is the ordinary case, and a row that is mid question
    /// answers nothing else: it does not fold, does not navigate and does not sleep.
    armed: Option<i64>,
    /// The space rule whose drawer of controls is out or on its way, one place a frame.
    drawer: Option<Drawer>,
    /// Where the wheel moved the column's list: the terminal the keyboard was on, and the
    /// first list row. A move of the keyboard to another terminal lets the list follow it
    /// again, so a scroll never hides where the keys went.
    list_scroll: Option<(u32, usize)>,
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
    /// The start of a key sequence the read ended in the middle of, for the palette or the
    /// keys search, which are never open together.
    palette_pending: Vec<u8>,
    /// What is typed into the keys overlay's search, `None` while nobody is searching.
    keys_query: Option<String>,
    /// Copy mode, resize mode, a close waiting for its `y`, and what they leave for the
    /// loop to send.
    modes: Modes,
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
    /// The outer window's focus as the terminal last reported it; `None` until it says.
    outer_focus: Option<bool>,
    /// The outer terminal's own foreground and background, as it last answered `OSC 10/11`.
    host_colors: [Option<[u8; 3]>; 2],
    /// Whether the outer terminal last said its scheme is light (`CSI ? 997 ; 2 n`).
    host_light: Option<bool>,
    /// What the outer terminal said to XTVERSION, for [`crate::icons::decide`].
    icon_reply: crate::icons::Reply,
    /// Whether this machine has the icon font where iTerm2 and Terminal look.
    icon_font: bool,
    /// The colours last told to the daemon, so an unchanged answer is not sent again.
    host_sent: Option<([u8; 3], [u8; 3])>,
    /// The daemon predates `HostColors` and said so: not asked again this session.
    host_refused: bool,
    /// The rows last told to the daemon as news, so the same set is not sent again.
    news_sent: Option<Vec<u32>>,
    /// The daemon predates `News` and said so: not told again this session.
    news_refused: bool,
    /// State changes waiting out `notify::DELAY` before they are said, and when the first
    /// of them is due. Empty on a quiet screen, so nothing is armed (ADR-009).
    notes: Vec<crate::notify::Note>,
    notes_due: Option<std::time::Instant>,
    /// Where a desktop note goes, decided once at start.
    route: crate::notify::Route,
    /// Whether the hint row still owes the person why notes have nowhere to go: said once,
    /// the first time one had.
    nowhere: bool,
    /// What `config.toml` said when it was last read: at start, and whenever the settings
    /// box opens. Every overlay that hands the prefix back to the decoder reads it here.
    config: crate::config::Config,
    /// Text being selected with the mouse, or selected and still shown.
    ///
    /// Kept after the button comes up rather than cleared, because a person who has just
    /// copied something looks at it to check they got the right thing. The next press in
    /// the pane clears it.
    selection: Option<crate::selection::Selection>,
    /// A press went to the program in the focused pane and its release has not. The
    /// program is owed the rest of that gesture wherever the pointer goes; without the
    /// flag a release outside the tile was dropped and vim never saw the button come up.
    forwarded_press: bool,
}

/// Which tile a screen position lands in: its terminal, the pane-global cell,
/// and the tile-local cell. Divider cells answer `None`.
type TileHit = (u32, (usize, usize), (usize, usize));

/// An agent row under a held button. `name` is whether the press landed on the row that
/// names the agent rather than the one under it.
#[derive(Clone, Copy, Debug)]
struct RowPress {
    source: Source,
    at: (u16, u16),
    /// Where the pointer is now, which the carried chip follows.
    now: (u16, u16),
    name: bool,
    moved: bool,
    /// Called off while the button is still down (Esc, a resize, the window losing focus,
    /// a box opening): the rest of the gesture is swallowed, so a program in a pane never
    /// sees motion and a release for a press it was never sent.
    dead: bool,
}

/// What a held row carries.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Source {
    /// An agent's tab, by its row in the column.
    Agent(u32),
    /// A pane of a split, by its name on its top edge: always that pane alone, which leaves
    /// its split wherever it is let go in its own space.
    Pane(u32),
    /// A space's rule. `fold` is what a click on it does: fold the space, or else go to it.
    Space { space: i64, fold: bool },
}

/// Where a held row would go if it were let go here.
#[derive(Clone, Copy, Debug, PartialEq)]
enum RowDrop {
    /// Another space, at the end of its tabs: the drop where the column has no order to
    /// point into (the priority list, the folded strip).
    Space(i64),
    /// A place in a space's order, and the column row the landing rule opens before.
    Slot {
        space: i64,
        place: Place,
        rule: usize,
    },
    /// A place on the tab bar of the space on screen: a tab of its own there.
    Bar { space: i64, place: Place },
    /// Where it already is: letting go changes nothing.
    Here,
    /// A carried space, just before `before` in the list or last of its machine's spaces,
    /// and the column row the landing rule opens before.
    SpaceSlot { before: Option<i64>, rule: usize },
    /// Beside this pane of the space on screen, on this side of it.
    Pane(u32, layout::Move),
}

/// A space being closed, and the tabs still on their way out of it.
#[derive(Clone)]
struct Closing {
    space: i64,
    /// Every tab that has to go, the live ones and a sleeping space's own, in the order
    /// they are asked for.
    asked: Vec<u32>,
    sent: usize,
    /// Whether the space itself has been asked, and the id it went out under. The gate that
    /// sends it runs on every index, and a second `CloseWorkspace` for a space the first one
    /// deleted is answered with a refusal that would be reported against a close that
    /// worked; the id is what tells our refusal from `ctrl-a W`'s, which is the same prefix.
    space_asked: bool,
    space_asked_id: Option<String>,
    /// A tab that would not take the close, and how many times it has been asked for it.
    /// The daemon's only refusal here is a full command channel, which is exactly the kind
    /// that succeeds the second time, so the tab is asked again before the chain gives up
    /// on it. The count is only advanced here, never reset by a refusal: a reset would
    /// make the budget unreachable and the tab would be asked until the give-up timer.
    stuck: Option<u32>,
    stuck_tries: usize,
    /// The id each close was sent under and the tab it was for, because a refusal names its
    /// request and not its terminal, and a refusal for anything else is somebody else's.
    pending: Vec<(String, u32)>,
}

/// The modes that own the keyboard for a while, and what they leave for the loop.
///
/// Together because they are one idea: a key means something else until the mode is left,
/// and `apply` (which cannot write) sometimes has to ask the daemon on a mode's behalf.
#[derive(Default)]
struct Modes {
    /// Copy mode over the focused terminal. See `copy`.
    copy: Option<crate::copy::Copy>,
    /// A copy mode selection whose history rows are still being fetched.
    yank: Option<crate::copy::Yank>,
    /// A finished yank's text, waiting for the loop to put it on the clipboard.
    clipboard: Option<String>,
    /// Requests made where no write is in hand, sent at the loop's next write.
    outbox: Vec<ControlRequest>,
    /// Resize mode: `h j k l` move the nearest divider until the mode is left.
    resizing: bool,
    /// A split divider held by the mouse: the path to its split.
    split_drag: Option<layout::SplitPath>,
    /// A close waiting for its `y`: the terminal, and the question on the notice row.
    confirm_close: Option<(u32, String)>,
    /// Rows for a pinned view that went behind the screen have been asked for and not
    /// answered yet, so a streaming terminal asks once per answer rather than per frame.
    refetching: bool,
    /// The start of a key sequence the last read ended inside, held for the next one.
    pending: Vec<u8>,
    /// The last screen of the sleeping tab the keyboard rests on, once the daemon has
    /// sent it, drawn dim in the pane so a person sees where it was before waking it.
    peek: Option<(u32, SnapshotWire)>,
    /// The sleeping tab whose screen has been asked for, so it is asked for once.
    peek_asked: Option<(u32, u16)>,
}

impl Modes {
    /// Whether a mode owns the keyboard.
    fn open(&self) -> bool {
        self.copy.is_some() || self.resizing || self.confirm_close.is_some()
    }
}

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
                    self.snapshot.as_ref().or_else(|| self.preview())
                } else {
                    self.snapshots
                        .get(&terminal)
                        .or_else(|| self.last_seen.get(&terminal))
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
            panes: self.panes(),
            focused: self.focused,
            armed: self.armed,
            drawer: self.drawer_view(),
            snapshot: self
                .peek_view()
                .or(self.snapshot.as_ref())
                .or_else(|| self.preview()),
            cols: self.cols,
            lines: self.lines,
            // A mode speaks for itself on the notice row: its keys, or its question.
            message: self.mode_line().or(self.message.as_deref()),
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
            menu: self.menu.map(|selected| (selected, &self.menu_kind)),
            collapsed: self.collapsed,
            reading: self.reading,
            selection: self.copy_selection().or(self.selection),
            scroll: self.scrolled_rows(),
            anchor: self.anchor.unwrap_or_default(),
            rows_back: self.rows_back(),
            behind: &self.behind,
            tiles,
            // Filled by the paint loop when the focused terminal holds a picture
            // the outer terminal cannot draw. `None` here keeps `frame()` pure:
            // capability is a client property, not daemon state.
            image_notice: None,
            chrome: screen::Chrome {
                shut: &self.shut,
                drag: None,
                usage: &self.usage,
                answer: self.answer.is_some(),
                dim: self.dim_tab().unwrap_or(0),
                list_scroll: self.list_scroll(),
                inbox: self.inbox,
                keys_query: self.keys_query.as_deref(),
                closing: self
                    .modes
                    .confirm_close
                    .as_ref()
                    .map(|(terminal, _)| *terminal),
                mode: if self.modes.confirm_close.is_some() {
                    None
                } else if self.modes.resizing {
                    Some(("resize", screen::RESIZE_HINT))
                } else {
                    self.modes.copy.as_ref().map(|copy| {
                        let keys: &'static [(&'static str, &'static str)] =
                            if copy.status.is_empty() {
                                screen::COPY_HINT
                            } else {
                                &[]
                            };
                        ("copy", keys)
                    })
                },
                keys: if self.prefix_pending {
                    screen::KeyContext::Prefix
                } else if self.in_column
                    && screen::left_for(self.cols, self.collapsed, self.sidebar_width) > 0
                {
                    screen::KeyContext::Sidebar
                } else {
                    screen::KeyContext::Pane
                },
            },
        }
    }

    /// The tabs of the space on screen, in the order the daemon reports them.
    /// Split panes are sub-terminals inside their tab, never top tabs, so
    /// they are filtered here. Agents in panes still list in the sidebar.
    fn tabs(&self) -> Vec<AgentRow> {
        let panes = self.panes();
        screen::in_space(&self.rows, self.space)
            .into_iter()
            .filter(|r| !panes.contains_key(&r.terminal))
            .collect()
    }

    /// Every terminal that is a pane inside somebody's tab, with that tab: the first leaf
    /// of its tree. See `is_pane`.
    fn panes(&self) -> std::collections::BTreeMap<u32, u32> {
        self.trees()
            .flat_map(|tree| {
                let leaves = tree.leaves();
                let tab = leaves.first().copied().unwrap_or_default();
                leaves.into_iter().skip(1).map(move |pane| (pane, tab))
            })
            .collect()
    }

    /// The tab the keyboard is in: the focused terminal, or the tab it is a split of.
    fn focused_tab(&self) -> u32 {
        screen::tab_of(self.focused, &self.panes())
    }

    /// Every tree this client holds, on screen and parked, in every space.
    fn trees(&self) -> impl Iterator<Item = &Layout> {
        self.layouts.values().chain(self.parked.values().flatten())
    }

    /// Whether a terminal is a pane inside somebody's tab rather than a tab:
    /// a leaf after the first of a tree. The first leaf is the tab, so when it
    /// closes the next one takes the tab's place on the bar.
    fn is_pane(&self, terminal: u32) -> bool {
        self.trees()
            .any(|tree| tree.leaves().iter().skip(1).any(|t| *t == terminal))
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
            .clamp(sidebar::SIDEBAR_MIN, sidebar::sidebar_max(self.cols));
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

    /// What an open mode says on the notice row, if one is open.
    fn mode_line(&self) -> Option<&str> {
        if let Some((_, question)) = &self.modes.confirm_close {
            return Some(question);
        }
        self.modes.copy.as_ref().map(|copy| copy.status.as_str())
    }

    /// Copy mode's selection, as the focused tile draws it.
    fn copy_selection(&self) -> Option<crate::selection::Selection> {
        let copy = self.modes.copy.as_ref()?;
        copy.on_screen(focused_rect(self).rows, &|at| {
            copy_row(self, at).unwrap_or_default()
        })
    }

    /// A one based screen position as a pane-local cell, pulled inside the pane when the
    /// pointer has left it: a divider held by the mouse follows the pointer to the edge.
    fn pane_cell(&self, row: u16, col: u16) -> (usize, usize) {
        let area = pane_area(self);
        let first_col = screen::left_for(self.cols, self.collapsed, self.sidebar_width) + 1;
        let x = (col as usize).saturating_sub(first_col);
        let y = (row as usize).saturating_sub(screen::TABS + 1);
        (
            x.min(area.cols.saturating_sub(1)),
            y.min(area.rows.saturating_sub(1)),
        )
    }

    /// Whether a plain press lands on a divider between two tiles, and hold it if so.
    fn split_press(&mut self, col: u16, row: u16) -> bool {
        let first_col = screen::left_for(self.cols, self.collapsed, self.sidebar_width) + 1;
        let area = pane_area(self);
        // Inside the pane only: `pane_cell` pulls a pointer past the edge back in, which is
        // right for a drag already held and wrong for a press on the status rows below.
        if (col as usize) < first_col
            || (col as usize) >= first_col + area.cols
            || (row as usize) <= screen::TABS
            || (row as usize) > screen::TABS + area.rows
        {
            return false;
        }
        let (x, y) = self.pane_cell(row, col);
        let path = self
            .space
            .and_then(|space| self.layouts.get(&space))
            .and_then(|layout| layout.divider_at(area, x, y));
        self.modes.split_drag = path;
        self.modes.split_drag.is_some()
    }

    /// Move a held divider to the pointer. Answers whether it moved: only then does the
    /// frame repaint, and the paint sizes the focused tile once for all the frame's moves.
    fn split_motion(&mut self, col: u16, row: u16) -> bool {
        let Some(path) = self.modes.split_drag.clone() else {
            return false;
        };
        let (x, y) = self.pane_cell(row, col);
        let area = pane_area(self);
        self.space
            .and_then(|space| self.layouts.get_mut(&space))
            .is_some_and(|layout| layout.drag(area, &path, x, y))
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
            if row == 0 || (row as usize) > screen::TABS {
                // The pane. A terminal application that wants the mouse asks for it, and
                // passing a click to one that did not would type its report at a prompt.
                return None;
            }
            // The bar starts where the pane starts, so the column it was clicked at is
            // counted from there and not from the edge of the screen.
            return match tabs::hit(
                &screen::bar_tabs(&self.rows, &self.panes(), self.space, &self.spaces),
                self.focused_tab(),
                pane_cols as usize,
                (col as usize).saturating_sub(left + 1),
                row as usize - 1,
            ) {
                Some(tabs::Hit::Terminal(terminal)) => Some(Clicked::Terminal(terminal)),
                Some(tabs::Hit::Close(_)) => Some(Clicked::CloseFocused),
                Some(tabs::Hit::New) => Some(Clicked::NewTerminal),
                None => None,
            };
        }
        let (hit, first) = self.column_hit(row)?;
        let column = self.column(now_unix, 0);
        if let sidebar::Hit::Terminal(terminal) = hit
            && first
            && !self.collapsed
        {
            if sidebar::on_agent_close(col as usize, terminal, &column) {
                return Some(Clicked::CloseAgent(terminal));
            }
            if sidebar::on_agent_cup(col as usize, terminal, &column) {
                return Some(Clicked::KeepAwakeAgent(terminal));
            }
        }
        // A row that does two things answers with the row; the column decides which.
        match sidebar::act(hit, col as usize, &column, self.collapsed)? {
            sidebar::Act::Space(space) => Some(Clicked::Space(space)),
            sidebar::Act::Fold(space) => Some(Clicked::Fold(space)),
            sidebar::Act::Sleep(space) => Some(Clicked::Sleep(space)),
            sidebar::Act::Wake(space) => Some(Clicked::Wake(space)),
            sidebar::Act::Settings(space) => Some(Clicked::Settings(space)),
            sidebar::Act::KeepAwake(space) => Some(Clicked::KeepAwake(space)),
            sidebar::Act::Drawer(space) => Some(Clicked::Drawer(space)),
            sidebar::Act::New(space) => Some(Clicked::New(space)),
            sidebar::Act::Close(space) => Some(Clicked::Close(space)),
            sidebar::Act::ConfirmClose(space) => Some(Clicked::ConfirmClose(space)),
            sidebar::Act::CancelClose => Some(Clicked::CancelClose),
            sidebar::Act::Terminal(terminal) => Some(Clicked::Terminal(terminal)),
            sidebar::Act::NewSpace => Some(Clicked::NewSpace),
            sidebar::Act::Menu => Some(Clicked::Menu),
            sidebar::Act::SettingsPage => Some(Clicked::SettingsPage),
            sidebar::Act::View => Some(Clicked::View),
            sidebar::Act::Collapse => Some(Clicked::Collapse),
            sidebar::Act::InboxTab(tab) => Some(Clicked::InboxTab(tab)),
            sidebar::Act::InboxFold => Some(Clicked::InboxFold),
        }
    }

    /// The column at clock `now`, every row from the top: what a click and a drop both map
    /// onto. A click maps onto the last paint (`paint_now`); a drag being painted maps onto
    /// the frame it is painted in, so its rule lands on the rows drawn beside it.
    fn column_rows_at(&self, now: i64) -> Vec<(sidebar::Line, Option<sidebar::Hit>)> {
        let column_rows = screen::column_rows(self.lines as usize);
        // Frame zero of the animation: tiers move rows, the tick never does.
        let column = self.column(now, 0);
        if self.collapsed {
            sidebar::rows_collapsed(&column, column_rows)
        } else {
            // The paint gives the foot the pet's rows when a pet is picked, so the
            // click maps onto the same layout it drew.
            sidebar::rows_folded(&column, column_rows, sidebar::current_avatar().as_ref())
        }
    }

    /// The column as the last paint drew it.
    fn column_rows_now(&self) -> Vec<(sidebar::Line, Option<sidebar::Hit>)> {
        self.column_rows_at(self.paint_now)
    }

    /// What the column drew on a one based screen row, and whether that row is the first
    /// of its kind: an agent's name rather than the row under it.
    fn column_hit(&self, row: u16) -> Option<(sidebar::Hit, bool)> {
        // The column, which starts at the top row and runs down to the status line.
        let within = (row as usize).checked_sub(screen::SIDEBAR_TOP)?;
        let rows = self.column_rows_now();
        let hit = rows.get(within).and_then(|(_, hit)| *hit)?;
        // An agent's name row carries its mark and the row under it does not, which holds
        // however the list is scrolled; anything else is first where the row above differs.
        let first = match hit {
            sidebar::Hit::Terminal(_) => rows[within].0.indicator.is_some(),
            _ => within == 0 || rows[within - 1].1 != Some(hit),
        };
        Some((hit, first))
    }

    /// The space a column row belongs to: its rule's, or its tab's, asleep or not.
    fn space_of_hit(&self, hit: sidebar::Hit) -> Option<i64> {
        match hit {
            sidebar::Hit::Space(space) => Some(space),
            sidebar::Hit::Terminal(t) => self
                .rows
                .iter()
                .find(|r| r.terminal == t)
                .map(|r| r.workspace)
                .or_else(|| self.sleeping_space_of(t)),
            _ => None,
        }
    }

    /// The pane whose name on its top edge is at a one based screen position: the handle a
    /// pane is carried by, out to the tab bar or anywhere a row can go. Only where the panes
    /// are framed, which is a split, and only on the panes' top edge: the name of a pane
    /// stacked under another sits on the divider between them, which stays the divider to
    /// resize them (that pane goes by its row in the column).
    fn title_at_screen(&self, row: u16, col: u16) -> Option<u32> {
        let layout = self.layouts.get(&self.space?)?;
        let rects = layout.tile_rects(pane_area(self));
        if rects.len() < 2 {
            return None;
        }
        let left = screen::left_for(self.cols, self.collapsed, self.sidebar_width);
        let x = (col as usize).checked_sub(left + 1)?;
        rects.into_iter().find_map(|(terminal, r)| {
            // Drawn on the row above the pane, two cells in from its corner, as ` name `.
            let top = r.y.checked_sub(1)?;
            if top != 0 || row as usize != screen::TABS + 1 + top {
                return None;
            }
            let agent = self.rows.iter().find(|a| a.terminal == terminal)?;
            let room = r.cols.saturating_sub(2);
            let wide = crate::width::width(&sidebar::label(agent)).min(room.checked_sub(2)?);
            (room >= 3 && x > r.x && x <= r.x + wide + 2).then_some(terminal)
        })
    }

    /// Whether a carried tab is held over another machine's spaces, where it cannot go: the
    /// space on screen, or the one a held row is over in the column.
    fn over_other_machine(&self, terminal: u32) -> bool {
        let Some(press) = self.row_press else {
            return false;
        };
        let Some(from) = self.rows.iter().find(|r| r.terminal == terminal) else {
            return false;
        };
        let left = screen::left_for(self.cols, self.collapsed, self.sidebar_width);
        let under = if (press.now.1 as usize) <= left {
            self.column_hit(press.now.0)
                .and_then(|(hit, _)| self.space_of_hit(hit))
        } else {
            self.space
        };
        under.is_some_and(|s| crate::mux::machine_of(s) != crate::mux::machine_of(from.workspace))
    }

    /// Whether a box a person reads or answers is open over the screen: a held drag ends
    /// when one opens, so its release never lands on the answer.
    fn overlay_open(&self) -> bool {
        self.palette.is_some()
            || self.menu.is_some()
            || self.prompt.is_some()
            || self.reading.is_some()
            || self.modes.confirm_close.is_some()
    }

    /// Whether a terminal is asleep: a slept tab, or a tab of a sleeping space.
    fn is_asleep(&self, terminal: u32) -> bool {
        match self.rows.iter().find(|r| r.terminal == terminal) {
            Some(row) => row.state == "sleeping",
            None => self.sleeping_space_of(terminal).is_some(),
        }
    }

    /// A space's live tabs in the daemon's order, shells and panes included: the order a
    /// drop is placed in, which is more than the column shows.
    fn space_order(&self, space: i64) -> Vec<u32> {
        self.rows
            .iter()
            .filter(|r| r.workspace == space && r.state != "sleeping")
            .map(|r| r.terminal)
            .collect()
    }

    /// The tab a terminal belongs to, lead first: every leaf of its split, or itself.
    fn group_of(&self, terminal: u32) -> Vec<u32> {
        self.trees()
            .map(|tree| tree.leaves())
            .find(|leaves| leaves.contains(&terminal))
            .unwrap_or_else(|| vec![terminal])
    }

    /// What a drop of `terminal` into `space` carries: the whole split when its first row
    /// stays in its own space (a reorder moves a tab, panes and all), the terminal alone
    /// otherwise: leaving for another space, or a pane of the split placed on its own, which
    /// takes it out of the split as a tab of its own.
    fn carried_into(&self, terminal: u32, space: i64) -> Option<Vec<u32>> {
        let from = self.rows.iter().find(|r| r.terminal == terminal)?.workspace;
        if from != space {
            return Some(vec![terminal]);
        }
        // The split moves by the row the column shows first for it: its first pane, or its
        // first agent when the first pane is a shell, which has no row.
        let group = self.group_of(terminal);
        let shown = group
            .iter()
            .find(|t| self.rows.iter().any(|r| r.terminal == **t && r.agent))
            .copied();
        Some(
            if group.first() == Some(&terminal) || shown == Some(terminal) {
                group
            } else {
                vec![terminal]
            },
        )
    }

    /// What a carry of `terminal` takes into `space`: the pane alone when held by its name,
    /// else what its row carries.
    fn carried_for(&self, terminal: u32, space: i64, handle: bool) -> Option<Vec<u32>> {
        if handle {
            Some(vec![terminal])
        } else {
            self.carried_into(terminal, space)
        }
    }

    /// Whether a drop of `terminal` in its own space takes it out of its split: always when
    /// held by its name, else when its row is a pane's rather than the split's.
    fn leaving(&self, terminal: u32, handle: bool) -> bool {
        (handle && self.group_of(terminal).len() > 1) || self.leaves_split(terminal)
    }

    /// Whether `terminal` is a pane of a split that a drop in its own space would take out
    /// of the split, rather than the row that moves the whole split.
    fn leaves_split(&self, terminal: u32) -> bool {
        self.group_of(terminal).len() > 1
            && self
                .rows
                .iter()
                .find(|r| r.terminal == terminal)
                .and_then(|r| self.carried_into(terminal, r.workspace))
                .is_some_and(|carried| carried.len() == 1)
    }

    /// Where a held row would land at a one based screen position, on the column the last
    /// paint drew.
    fn drop_target(&self, source: Source, row: u16, col: u16) -> Option<RowDrop> {
        match source {
            Source::Agent(terminal) => self.drop_at(terminal, row, col, self.paint_now, false),
            Source::Pane(terminal) => self.drop_at(terminal, row, col, self.paint_now, true),
            Source::Space { space, .. } => self.space_drop(space, row, col, self.paint_now),
        }
    }

    /// Where a carried space would land: before the space whose rule is under the pointer,
    /// or after the space whose rows are, with the rule drawn on that space's blank foot.
    /// Anywhere in the list, whichever machine each space is on; only in the tree, where
    /// spaces have rules; never over a pane.
    fn space_drop(&self, space: i64, row: u16, col: u16, now: i64) -> Option<RowDrop> {
        let left = screen::left_for(self.cols, self.collapsed, self.sidebar_width);
        if left == 0 || (col as usize) > left || self.collapsed || self.view != sidebar::View::Tree
        {
            return None;
        }
        let within = (row as usize).checked_sub(screen::SIDEBAR_TOP)?;
        let rows = self.column_rows_at(now);
        let hit_of = |at: usize| rows.get(at).and_then(|(_, hit)| *hit);
        let (hit, owner) = match hit_of(within) {
            Some(hit) => (Some(hit), self.space_of_hit(hit)?),
            None => (None, self.space_of_hit(hit_of(within.checked_sub(1)?)?)?),
        };
        let ids: Vec<i64> = self.spaces.iter().map(|s| s.id).collect();
        if !ids.contains(&owner) {
            return None;
        }
        let (before, rule) = match hit {
            Some(sidebar::Hit::Space(s)) => (Some(s), within),
            _ => {
                let after = ids
                    .iter()
                    .skip_while(|id| **id != owner)
                    .skip(1)
                    .find(|id| **id != space)
                    .copied();
                // Down to the space's blank foot.
                let mut foot = within;
                while hit_of(foot).is_some_and(|h| self.space_of_hit(h) == Some(owner)) {
                    foot += 1;
                }
                (after, foot)
            }
        };
        // Where it already is: before itself, or the place the list would put it anyway.
        let mut then: Vec<i64> = ids.iter().copied().filter(|id| *id != space).collect();
        let at = before
            .and_then(|b| then.iter().position(|id| *id == b))
            .unwrap_or(then.len());
        then.insert(at, space);
        if before == Some(space) || then == ids {
            return Some(RowDrop::Here);
        }
        Some(RowDrop::SpaceSlot { before, rule })
    }

    /// Where a dragged agent row would land at a one based screen position, on the column
    /// at clock `now`: a place in a space's order when over the column, a pane's side when
    /// over the pane, nothing elsewhere. The side is the pane's edge nearest the pointer.
    fn drop_at(
        &self,
        terminal: u32,
        row: u16,
        col: u16,
        now: i64,
        handle: bool,
    ) -> Option<RowDrop> {
        let left = screen::left_for(self.cols, self.collapsed, self.sidebar_width);
        if left > 0 && (col as usize) <= left {
            return self.column_drop(terminal, row, now, handle);
        }
        // The pane is the space on screen's; a tab of another machine cannot join it.
        let from = self.rows.iter().find(|r| r.terminal == terminal)?.workspace;
        if crate::mux::machine_of(self.space?) != crate::mux::machine_of(from) {
            return None;
        }
        if (1..=screen::TABS).contains(&(row as usize)) {
            return self.bar_drop(terminal, row, col, handle);
        }
        let (target, _, (y, x)) = self.tile_at_screen(row, col)?;
        let area = pane_area(self);
        let rect = self
            .space
            .and_then(|space| self.layouts.get(&space))
            .and_then(|layout| {
                layout
                    .tile_rects(area)
                    .into_iter()
                    .find(|(t, _)| *t == target)
            })
            .map_or(area, |(_, rect)| rect);
        let (cols, rows) = (rect.cols.max(1) as f32, rect.rows.max(1) as f32);
        let (x, y) = (x as f32 + 0.5, y as f32 + 0.5);
        let sides = [
            (x / cols, layout::Move::Left),
            (1.0 - x / cols, layout::Move::Right),
            (y / rows, layout::Move::Up),
            (1.0 - y / rows, layout::Move::Down),
        ];
        let side = sides
            .into_iter()
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, side)| side)?;
        Some(RowDrop::Pane(target, side))
    }

    /// Where on the tab bar a carried terminal would land: before the tab under the pointer,
    /// or after the last one. A pane let go there leaves its split for a tab of its own; a
    /// tab moves along the bar; a tab of another space comes into this one.
    fn bar_drop(&self, terminal: u32, row: u16, col: u16, handle: bool) -> Option<RowDrop> {
        let space = self.space?;
        let (pane_cols, _) =
            screen::pane_size_for(self.cols, self.lines, self.collapsed, 0, self.sidebar_width);
        let left = screen::left_for(self.cols, self.collapsed, self.sidebar_width);
        let hit = tabs::hit(
            &screen::bar_tabs(&self.rows, &self.panes(), self.space, &self.spaces),
            self.focused_tab(),
            pane_cols as usize,
            (col as usize).saturating_sub(left + 1),
            row as usize - 1,
        );
        let place = match hit {
            Some(tabs::Hit::Terminal(t) | tabs::Hit::Close(t)) => Place::Before(t),
            _ => Place::End,
        };
        let carried = self.carried_for(terminal, space, handle)?;
        if let Place::Before(t) = place
            && carried.contains(&t)
            && !self.leaving(terminal, handle)
        {
            return Some(RowDrop::Here);
        }
        let from = self.rows.iter().find(|r| r.terminal == terminal)?.workspace;
        if from == space && !self.leaving(terminal, handle) {
            let full = self.space_order(space);
            let mut then: Vec<u32> = full
                .iter()
                .copied()
                .filter(|t| !carried.contains(t))
                .collect();
            let at = match place {
                Place::Before(t) => then.iter().position(|u| *u == t).unwrap_or(then.len()),
                Place::End => then.len(),
            };
            then.splice(at..at, carried.iter().copied());
            if then == full {
                return Some(RowDrop::Here);
            }
        }
        Some(RowDrop::Bar { space, place })
    }

    /// Where in the column a dragged row would land, worked out on the rows the click test
    /// reads (never the painted rule), so the rule cannot move what is under the pointer.
    ///
    /// In the tree an agent's name row means before its tab, the row under it after it
    /// (after the whole split for a split's rows), a space's rule before its first tab, and
    /// a space's sleeping rows or its blank foot the end of its live tabs. The priority
    /// list and the folded strip have no order to point into: any row there is its space,
    /// at the end.
    fn column_drop(&self, terminal: u32, row: u16, now: i64, handle: bool) -> Option<RowDrop> {
        let within = (row as usize).checked_sub(screen::SIDEBAR_TOP)?;
        let rows = self.column_rows_at(now);
        let hit_of = |at: usize| rows.get(at).and_then(|(_, hit)| *hit);
        let (hit, owner) = match hit_of(within) {
            Some(hit) => (Some(hit), self.space_of_hit(hit)?),
            // A blank row belongs to the space right above it, which is its foot; a blank
            // under another blank is the window's padding, and no place at all.
            None => (None, self.space_of_hit(hit_of(within.checked_sub(1)?)?)?),
        };
        // A tab stays on its machine: no place is offered on another one's spaces.
        let from = self.rows.iter().find(|r| r.terminal == terminal)?.workspace;
        if crate::mux::machine_of(owner) != crate::mux::machine_of(from) {
            return None;
        }
        let known = self.spaces.iter().any(|s| s.id == owner);
        if self.collapsed || self.view != sidebar::View::Tree || !known {
            return hit.map(|_| RowDrop::Space(owner));
        }
        let carried = self.carried_for(terminal, owner, handle)?;
        let full = self.space_order(owner);
        let order: Vec<u32> = full
            .iter()
            .copied()
            .filter(|t| !carried.contains(t))
            .collect();
        // The end of the live tabs is drawn above the space's sleeping rows, which follow
        // them in the column.
        let above_sleepers = |mut at: usize| {
            while at > 0
                && matches!(hit_of(at - 1), Some(sidebar::Hit::Terminal(u)) if self.is_asleep(u))
            {
                at -= 1;
            }
            at
        };
        let (place, rule) = match hit {
            None => (Place::End, above_sleepers(within)),
            // A folded space shows none of its tabs, so its rule is its end: nothing hidden
            // is placed before.
            Some(sidebar::Hit::Space(_)) if self.shut.contains(&owner) => {
                // Under what a folded space still shows (the keyboard's own tab), on its foot.
                let mut foot = within + 1;
                while hit_of(foot).is_some_and(|h| self.space_of_hit(h) == Some(owner)) {
                    foot += 1;
                }
                (Place::End, foot)
            }
            Some(sidebar::Hit::Space(_)) => (
                order.first().copied().map_or(Place::End, Place::Before),
                within + 1,
            ),
            Some(sidebar::Hit::Terminal(t)) if self.is_asleep(t) => {
                (Place::End, above_sleepers(within))
            }
            Some(sidebar::Hit::Terminal(t)) => {
                let group = self.group_of(t);
                // An agent's name row carries its mark; the row under it does not. Read off the
                // row itself, so a list scrolled to start on a second row still reads right.
                let first = rows[within].0.indicator.is_some();
                if first && group.first() == Some(&t) {
                    (Place::Before(t), within)
                } else {
                    // After the whole split, in the order and under its lowest row.
                    let last = full.iter().rposition(|u| group.contains(u));
                    let place = full
                        .iter()
                        .skip(last.map_or(0, |at| at + 1))
                        .find(|u| !carried.contains(u) && !group.contains(u))
                        .copied()
                        .map_or(Place::End, Place::Before);
                    let lowest = (0..rows.len())
                        .rev()
                        .find(|at| {
                            matches!(hit_of(*at), Some(sidebar::Hit::Terminal(u)) if group.contains(&u))
                        })
                        .unwrap_or(within);
                    (place, lowest + 1)
                }
            }
            Some(_) => return None,
        };
        // A place that leaves the order as it is, or that names what is carried, is where it
        // already is: no move, and no rule promising one.
        if let Place::Before(t) = place
            && carried.contains(&t)
        {
            return Some(RowDrop::Here);
        }
        // A pane let go anywhere in its own space leaves its split, which is a move even
        // where it already stands in the order.
        if from == owner && !self.leaving(terminal, handle) {
            let mut then = order.clone();
            let at = match place {
                Place::Before(t) => then.iter().position(|u| *u == t).unwrap_or(then.len()),
                Place::End => then.len(),
            };
            then.splice(at..at, carried.iter().copied());
            if then == full {
                return Some(RowDrop::Here);
            }
        }
        Some(RowDrop::Slot {
            space: owner,
            place,
            rule,
        })
    }

    /// The carried row as the frame at clock `now` draws it while the button is held, or
    /// `None` when no row is carried, the drag was called off, or the carried tab is asleep
    /// (it cannot move until it wakes).
    fn drag_paint(&self, now: i64) -> Option<screen::DragPaint> {
        let press = self.row_press.filter(|p| p.moved && !p.dead)?;
        let (terminal, handle) = match press.source {
            Source::Agent(terminal) => (terminal, false),
            Source::Pane(terminal) => (terminal, true),
            Source::Space { space, .. } => return self.space_paint(space, press.now, now),
        };
        let row = self.rows.iter().find(|r| r.terminal == terminal)?;
        if self.is_asleep(terminal) {
            return None;
        }
        let drop = self.drop_at(terminal, press.now.0, press.now.1, now, handle);
        // Staying in its own space it carries its split, wherever the pointer is in it.
        let home = match drop {
            Some(RowDrop::Slot { space, .. } | RowDrop::Bar { space, .. }) => {
                space == row.workspace
            }
            Some(RowDrop::Here) => true,
            _ => false,
        };
        let carried = home
            .then(|| self.carried_for(terminal, row.workspace, handle))
            .flatten()
            .unwrap_or_else(|| vec![terminal]);
        let rule = match drop {
            Some(RowDrop::Slot { space, rule, .. }) => Some((rule, space)),
            _ => None,
        };
        let outline = match drop {
            Some(RowDrop::Pane(pane, side)) if pane != terminal => {
                self.outline(terminal, pane, side)
            }
            _ => None,
        };
        let column = self.column(now, 0);
        // The panes it carries that have a row, which a shell does not.
        let more = carried
            .iter()
            .filter(|t| **t != terminal && self.rows.iter().any(|r| r.terminal == **t && r.agent))
            .count();
        let chip = sidebar::drag_chip(&column, row, more);
        Some(screen::DragPaint {
            carried,
            space: None,
            chip,
            pointer: press.now,
            rule,
            outline,
        })
    }

    /// A carried space as the frame at clock `now` draws it: its rule and every row under it
    /// dimmed, a chip with its name and count in its colour, and its landing rule.
    fn space_paint(&self, space: i64, pointer: (u16, u16), now: i64) -> Option<screen::DragPaint> {
        let row = self.spaces.iter().find(|s| s.id == space)?;
        let mut carried: Vec<u32> = self
            .rows
            .iter()
            .filter(|r| r.workspace == space)
            .map(|r| r.terminal)
            .collect();
        carried.extend(row.sleeping.iter().map(|t| t.terminal));
        let count = self
            .rows
            .iter()
            .filter(|r| r.workspace == space && r.agent)
            .count();
        let rule = match self.space_drop(space, pointer.0, pointer.1, now) {
            Some(RowDrop::SpaceSlot { rule, .. }) => Some((rule, space)),
            _ => None,
        };
        Some(screen::DragPaint {
            carried,
            space: Some(space),
            chip: (
                "\u{25be}".to_string(),
                crate::theme::space_in(&self.spaces, space),
                format!("{} ({count})", sidebar::chip_name(&row.name)),
            ),
            pointer,
            rule,
            outline: None,
        })
    }

    /// The cells `terminal` would take beside `pane` on `side`, on screen, tried the way a
    /// drop tries it: on a copy, and nothing where the drop would be refused.
    fn outline(
        &self,
        terminal: u32,
        pane: u32,
        side: layout::Move,
    ) -> Option<(usize, usize, usize, usize)> {
        let area = pane_area(self);
        let mut tree = self.layouts.get(&self.space?)?.clone();
        tree.remove(terminal);
        if tree.zoomed.is_some()
            || tree.has_pending()
            || !tree.insert_beside(pane, terminal, side, area)
        {
            return None;
        }
        let (_, rect) = tree
            .tile_rects(area)
            .into_iter()
            .find(|(t, _)| *t == terminal)?;
        let left = screen::left_for(self.cols, self.collapsed, self.sidebar_width);
        Some((
            screen::TABS + 1 + rect.y,
            left + 1 + rect.x,
            rect.rows,
            rect.cols,
        ))
    }

    /// The terminal the column raises: the focused one, or the sleeping tab the keys rest on.
    fn column_terminal(&self) -> u32 {
        if self.focused != 0 {
            self.focused
        } else {
            self.dim_tab().unwrap_or(0)
        }
    }

    /// The wheel's first list row, while the keyboard is still on the terminal it was on.
    fn list_scroll(&self) -> Option<usize> {
        self.list_scroll
            .filter(|(terminal, _)| *terminal == self.column_terminal())
            .map(|(_, at)| at)
    }

    /// The most list rows the inbox's edge may be dragged to in this window.
    fn inbox_room(&self) -> usize {
        sidebar::inbox_room(
            &self.column(self.paint_now, 0),
            screen::column_rows(self.lines as usize),
            sidebar::current_avatar().as_ref(),
        )
    }

    /// The pieces the left column is drawn from, at a given clock and animation frame.
    fn column(&self, now_unix: i64, tick: u64) -> sidebar::Column<'_> {
        sidebar::Column {
            spaces: &self.spaces,
            agents: &self.rows,
            focused_space: self.space.unwrap_or_default(),
            focused_terminal: self.column_terminal(),
            tick,
            view: self.view,
            seen: &self.seen_done,
            unseen: &self.unseen_finished,
            now_unix,
            width: self.sidebar_width,
            shut: &self.shut,
            armed: self.armed,
            drawer: self.drawer_view(),
            usage: Some(&self.usage),
            scroll: self.list_scroll(),
            inbox: self.inbox,
            icons: crate::icons::on(),
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
        let n = tabs.len();
        // From a sleeping tab the keyboard was on, the first tab or the last. From a pane
        // of a split, its tab.
        let focused = self.focused_tab();
        let Some(at) = tabs.iter().position(|r| r.terminal == focused) else {
            return Some(tabs[if forward { 0 } else { n - 1 }].terminal);
        };
        let next = if forward {
            (at + 1) % n
        } else {
            (at + n - 1) % n
        };
        Some(tabs[next].terminal)
    }

    /// The awake space after the focused one, wrapping. A sleeping space is passed over:
    /// going there wakes nothing, so it is only reached on purpose.
    fn next_space(&self, forward: bool) -> Option<i64> {
        let at = self
            .spaces
            .iter()
            .position(|w| Some(w.id) == self.space)
            .unwrap_or(0);
        let n = self.spaces.len();
        (1..=n)
            .map(|step| {
                if forward {
                    (at + step) % n
                } else {
                    (at + n - step) % n
                }
            })
            .map(|next| &self.spaces[next])
            .find(|w| !w.asleep)
            .map(|w| w.id)
    }

    /// The focused terminal's last seen screen, while `focus()` lets it stand in for the
    /// one on its way.
    fn preview(&self) -> Option<&SnapshotWire> {
        self.previewing
            .then(|| self.last_seen.get(&self.focused))
            .flatten()
    }

    /// The focused pane's screen goes, and its stand-in with it: what comes next is
    /// another picture, never the one this pane showed before.
    fn forget_screen(&mut self) {
        self.snapshot = None;
        self.previewing = false;
    }

    /// The screen of the sleeping tab the keyboard rests on, when it has arrived.
    fn peek_view(&self) -> Option<&SnapshotWire> {
        let tab = self.dim_tab()?;
        self.modes
            .peek
            .as_ref()
            .filter(|(shown, _)| *shown == tab)
            .map(|(_, snapshot)| snapshot)
    }

    /// Whether this space is listed asleep: every tab of it.
    fn asleep(&self, space: i64) -> bool {
        self.spaces.iter().any(|w| w.id == space && w.asleep)
    }

    /// The sleeping tabs of the space on screen, one per tab (its first pane), whether or
    /// not anything else in it is awake.
    fn dim_tabs(&self) -> Vec<&qoro_runtime::control::SleepingTab> {
        self.spaces
            .iter()
            .find(|w| Some(w.id) == self.space)
            .map_or_else(Vec::new, |w| {
                w.sleeping
                    .iter()
                    .filter(|t| t.lead.is_none_or(|lead| lead == t.terminal))
                    .collect()
            })
    }

    /// The sleeping tab the keyboard rests on: the one `ctrl-a n/p` picked, else the first.
    /// Only while no live terminal has the keyboard.
    fn dim_tab(&self) -> Option<u32> {
        if self.focused != 0 {
            return None;
        }
        let tabs = self.dim_tabs();
        tabs.iter()
            .find(|t| t.terminal == self.dim)
            .or(tabs.first())
            .map(|t| t.terminal)
    }

    /// The space a sleeping tab belongs to, when it is one.
    fn sleeping_space_of(&self, terminal: u32) -> Option<i64> {
        self.spaces
            .iter()
            .find(|w| w.sleeping.iter().any(|t| t.terminal == terminal))
            .map(|w| w.id)
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
    let last = menu_rows(&state.menu_kind).len().saturating_sub(1);
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
                    state.menu = Some((selected + 1).min(last));
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
            byte if byte == state.config.prefix || byte == crate::keys::DETACH => {
                state.menu = None;
                return MenuOutcome::Passthrough(bytes[at - 1..].to_vec());
            }
            letter => {
                if letter == b'k' {
                    state.menu = Some(selected.saturating_sub(1));
                    return MenuOutcome::Moved;
                }
                if letter == b'j' {
                    state.menu = Some((selected + 1).min(last));
                    return MenuOutcome::Moved;
                }
                // A row's own key first, so `r` means rename tab and never
                // reload; then the first letter of a keyless row.
                if state.menu_kind != MenuKind::Main {
                    continue;
                }
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
    (
        "space settings",
        "R",
        "name and colour of what is on screen",
    ),
    ("close tab", "x", "close what is focused"),
    ("split side by side", "v", "a sub-terminal, not a new tab"),
    ("split stacked", "-", "a sub-terminal, not a new tab"),
    ("zoom pane", "z", "fill the pane with this tile, or go back"),
    (
        "copy mode",
        "[",
        "move, select and search the whole history",
    ),
    ("resize panes", "e", "h j k l move the nearest divider"),
    ("equalize panes", "=", "every pane the same share"),
    ("toggle view", "g", "spaces as a tree or agents by urgency"),
    ("keybinds", "?", "every key, and what it does"),
    ("settings", "", "every setting on one page, with your pet"),
    // After "settings": a letter jumps to the first row it starts, and `s` has
    // meant settings for longer than these two rows have existed.
    ("sleep space", "", "every tab of it sleeps"),
    ("wake space", "", "every sleeping tab of it wakes"),
    (
        "reload manifests",
        "",
        "re-read the agent detection overrides",
    ),
    ("connect a machine", "", "another machine's qoro, over ssh"),
    ("detach", "d", "leave, and keep everything running"),
];

/// Which list the menu box is showing. One box, one set of keys and clicks, three lists.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum MenuKind {
    /// Everything, from `ctrl-a m` or the word at the foot of the column.
    #[default]
    Main,
    /// What the `+` beside "spaces" offers.
    Plus,
    /// What a space's `+`, or the tab bar's, offers: a terminal or an agent there. `name`
    /// is the space's, for the title.
    New { space: i64, name: String },
    /// The agents a new tab in `space` can start, found when the list opened.
    Agents {
        space: i64,
        name: String,
        agents: Vec<crate::config::Launcher>,
    },
    /// A machine being connected has no qoro: put this version there, or not.
    Install {
        target: String,
        platform: String,
        root: bool,
    },
}

/// The rows the menu box shows: a name on the left, and on the right its key or what
/// state it is in.
pub fn menu_rows(kind: &MenuKind) -> Vec<(String, String)> {
    match kind {
        MenuKind::Main => MENU
            .iter()
            .map(|(name, key, _)| {
                let right = if key.is_empty() {
                    String::new()
                } else {
                    crate::keys::named(&format!("ctrl-a {key}"))
                };
                (name.to_string(), right)
            })
            .collect(),
        MenuKind::Plus => vec![
            ("new space".to_string(), crate::keys::named("ctrl-a w")),
            ("connect a machine (ssh)".to_string(), String::new()),
        ],
        MenuKind::New { .. } => vec![
            ("agent".to_string(), String::new()),
            ("terminal".to_string(), String::new()),
        ],
        MenuKind::Agents { agents, .. } => agents
            .iter()
            .map(|a| {
                // A long command is cut, so the box stays a box and not the width of the
                // screen: the name already says which line it is.
                let right = if a.command == a.name {
                    String::new()
                } else if crate::width::width(&a.command) > MENU_COMMAND {
                    let cut = crate::width::text(&a.command, MENU_COMMAND - 1);
                    format!("{}\u{2026}", cut.trim_end())
                } else {
                    a.command.clone()
                };
                (a.name.clone(), right)
            })
            .chain([("other command".to_string(), String::new())])
            .collect(),
        MenuKind::Install { platform, .. } => vec![
            (
                format!("install qoro {} there", env!("CARGO_PKG_VERSION")),
                platform.clone(),
            ),
            ("cancel".to_string(), String::new()),
        ],
    }
}

/// The most columns an `[agents]` command takes on the right of the agents list.
const MENU_COMMAND: usize = 30;

/// Open what a `+` offers for `space`.
fn open_new(state: &mut State, space: i64) {
    let name = space_name(state, space);
    state.menu_kind = MenuKind::New { space, name };
    state.menu = Some(0);
}

/// What the menu box says it is.
pub fn menu_title(kind: &MenuKind) -> String {
    match kind {
        MenuKind::Main => " menu".to_string(),
        MenuKind::Plus => " spaces".to_string(),
        MenuKind::New { name, .. } => format!(" new in {name}"),
        MenuKind::Agents { name, .. } => format!(" new agent in {name}"),
        // As root said out loud: every agent started there will run as root.
        MenuKind::Install { target, root, .. } => {
            let who = if *root { " as root" } else { "" };
            format!(" no qoro on {target}{who}")
        }
    }
}

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
    let row = state.rows.iter().find(|r| r.terminal == state.focused)?;
    usable_cwd(row)
}

/// The focused terminal's directory for a new tab in `space`, only when the two are on one
/// machine: a directory is a place on its own machine and nowhere else.
fn focused_cwd_in(state: &State, space: i64) -> Option<String> {
    let focused = state.rows.iter().find(|r| r.terminal == state.focused)?;
    (crate::mux::machine_of(focused.workspace) == crate::mux::machine_of(space))
        .then(|| usable_cwd(focused))
        .flatten()
}

/// A row's last reported directory, when a new tab can start there.
fn usable_cwd(row: &AgentRow) -> Option<String> {
    let cwd = row.cwd.clone()?;
    // A remote terminal's directory is on its own machine, so this one cannot check it;
    // the mux carries it only to that same machine.
    let here = crate::mux::is_remote_terminal(row.terminal) || std::path::Path::new(&cwd).is_dir();
    (!cwd.is_empty() && std::path::Path::new(&cwd).is_absolute() && here).then_some(cwd)
}

/// Where a new tab in `space` starts: the focused terminal's directory when that space is
/// on screen, else the first of its tabs with a usable one, else the daemon's own.
fn space_cwd(state: &State, space: i64) -> Option<String> {
    // The focused terminal is not always in the space on screen (a space just made is shown
    // before its first tab is): its directory only where it is on the same machine.
    if state.space == Some(space)
        && let Some(cwd) = focused_cwd_in(state, space)
    {
        return Some(cwd);
    }
    state
        .rows
        .iter()
        .filter(|r| r.workspace == space)
        .find_map(usable_cwd)
}

/// Remember a create aimed at a space that is not on screen, so its tab is shown with its
/// space once listed rather than focused into the space on screen.
fn follow_elsewhere(state: &mut State, space: i64, ask: ControlRequest) -> ControlRequest {
    if state.space != Some(space)
        && let ControlRequest::CreateTerminal { id, .. } = &ask
    {
        state.elsewhere = Some(id.clone());
    }
    ask
}

/// The agents a `+` offers: the config's `[agents]` first, then every known agent whose
/// command is on this machine's `PATH`.
///
/// ponytail: this machine's PATH even for a remote space; a missing agent there fails in
/// its own shell, where a person sees why. Ask the remote when that proves annoying.
fn installed_agents() -> Vec<crate::config::Launcher> {
    agents_on(
        crate::config::agents(),
        &std::env::var_os("PATH").unwrap_or_default(),
    )
}

/// `own` followed by every known agent found in the directories of `path` and not already
/// started by one of `own`.
fn agents_on(
    own: Vec<crate::config::Launcher>,
    path: &std::ffi::OsStr,
) -> Vec<crate::config::Launcher> {
    let mut out = own;
    let dirs: Vec<std::path::PathBuf> = std::env::split_paths(path).collect();
    for name in qoro_domain::program::agent_names() {
        let listed = out
            .iter()
            .any(|l| l.command.split_whitespace().next() == Some(name));
        if !listed && dirs.iter().any(|dir| dir.join(name).is_file()) {
            out.push(crate::config::Launcher {
                name: name.to_string(),
                command: name.to_string(),
            });
        }
    }
    out
}

/// Ask the daemon for a tab running an agent command, and remember the line to
/// type once the tab opens.
///
/// The daemon only starts a shell (ADR-015): the command is typed, never spawned,
/// the same path `qoro new -- <agent>` takes. The tab takes the command's first
/// word as its name, so it reads as the agent until renamed, unless the person
/// gave it one: that goes on the tab now and on its conversation once the agent
/// says which one it is in. Empty commands never reach here: the prompt treats
/// them as a cancel. The caller owns the request number, incremented the same way
/// every other prompt answer is.
fn start_agent_terminal(
    state: &mut State,
    id: u64,
    command: &str,
    space: Option<i64>,
    given: Option<String>,
) -> ControlRequest {
    let id = format!("agent-{id}");
    let name = given.clone().or_else(|| {
        command
            .split_whitespace()
            .next()
            .map(|first| {
                first
                    .rsplit('/')
                    .next()
                    .filter(|word| !word.is_empty())
                    .unwrap_or(first)
                    .to_string()
            })
            .filter(|word| !word.is_empty())
    });
    state
        .pending_agents
        .insert(id.clone(), (command.to_string(), given));
    ControlRequest::CreateTerminal {
        id,
        // Like every other new tab: the agent starts where the
        // person already is rather than where the daemon started.
        cwd: match space {
            Some(space) => space_cwd(state, space).or_else(|| focused_cwd_in(state, space)),
            None => focused_cwd(state),
        },
        workspace: space,
        name,
    }
}

/// The line an agent tab is owed, from its `TerminalCreated` answer, with the name
/// given to it, or `None` when the answer carries no agent.
///
/// The id prefix is the whole protocol: only `agent-` requests recorded a line
/// in `pending_agents`. A `Failed` answer calls this too and drops the line, so
/// a tab that never opened types nothing.
fn take_agent_command(state: &mut State, id: &str) -> Option<(Vec<u8>, Option<String>)> {
    if !id.starts_with("agent-") {
        return None;
    }
    state.pending_agents.remove(id).map(|(command, name)| {
        let mut line = command.into_bytes();
        line.push(b'\r');
        (line, name)
    })
}

/// Send each name owed to a conversation whose agent has now said which one it is in,
/// once: the owed name goes as it is sent.
fn name_sessions(state: &mut State, request: &mut u64) {
    for row in &state.rows {
        if row.agent_session.is_some()
            && let Some(title) = state.session_titles.remove(&row.terminal)
        {
            *request += 1;
            state.modes.outbox.push(ControlRequest::RenameSession {
                id: format!("title-{request}"),
                terminal: row.terminal,
                title: Some(title),
            });
        }
    }
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
    let ask = |what| Some(Prompt::new(what, ""));
    match std::mem::take(&mut state.menu_kind) {
        MenuKind::Main => {}
        MenuKind::Plus => {
            state.prompt = ask(if item == 0 {
                Naming::Space
            } else {
                Naming::Remote
            });
            return MenuSend::Nothing;
        }
        MenuKind::New { space, name } => {
            if item == 0 {
                state.menu_kind = MenuKind::Agents {
                    space,
                    name,
                    agents: installed_agents(),
                };
                state.menu = Some(0);
                return MenuSend::Nothing;
            }
            *request += 1;
            state.message = Some("starting a terminal".to_string());
            let ask = ControlRequest::CreateTerminal {
                id: format!("new-{request}"),
                cwd: space_cwd(state, space),
                workspace: Some(space),
                name: None,
            };
            return MenuSend::Control(follow_elsewhere(state, space, ask));
        }
        MenuKind::Agents { space, agents, .. } => {
            state.prompt = Some(match agents.get(item) {
                Some(agent) => agent_name(Some(space), agent.command.clone()),
                None => Prompt::new(Naming::Agent(Some(space)), ""),
            });
            return MenuSend::Nothing;
        }
        MenuKind::Install { target, .. } => {
            if item != 0 {
                state.connecting = None;
                return MenuSend::Nothing;
            }
            *request += 1;
            let id = format!("install-{request}");
            state.installing = Some(id.clone());
            state.message = Some(format!("installing qoro on {target}"));
            return MenuSend::Control(ControlRequest::InstallRemote { id, target });
        }
    }
    match MENU[item].0 {
        "connect a machine" => {
            state.prompt = ask(Naming::Remote);
            MenuSend::Nothing
        }
        "new tab" => {
            state.prompt = ask(Naming::Terminal);
            MenuSend::Nothing
        }
        "new space" => {
            state.prompt = ask(Naming::Space);
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
                state.prompt = Some(Prompt::new(Naming::RenameTerminal(state.focused), current));
            } else {
                state.message = Some(format!(
                    "nothing to rename, {} c starts a terminal",
                    crate::keys::prefix_name()
                ));
            }
            MenuSend::Nothing
        }
        "space settings" => {
            match state.space {
                Some(space) => state.prompt = Some(space_settings(state, space)),
                None => state.message = Some("no space yet".to_string()),
            }
            MenuSend::Nothing
        }
        // The rule's `sleep` and `wake`, for a terminal with no mouse.
        "sleep space" => {
            match state.space {
                Some(space) => sleep_space(state, space, request),
                None => state.message = Some("no space yet".to_string()),
            }
            MenuSend::Nothing
        }
        "wake space" => {
            match state.space {
                Some(space) => wake_space(state, space, request),
                None => state.message = Some("no space yet".to_string()),
            }
            MenuSend::Nothing
        }
        "close tab" => {
            if state.focused == 0 {
                state.message = Some("nothing to close".to_string());
                MenuSend::Nothing
            } else if let Some(question) = close_question(state, state.focused) {
                state.modes.confirm_close = Some((state.focused, question));
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
                        state
                            .split_origin
                            .insert(format!("split-{request}"), (s, focused));
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
        "copy mode" => {
            enter_copy(state);
            MenuSend::Nothing
        }
        "resize panes" => {
            enter_resize(state);
            MenuSend::Nothing
        }
        "equalize panes" => {
            equalize(state);
            MenuSend::Nothing
        }
        "toggle view" => {
            state.view = state.view.toggle();
            MenuSend::Nothing
        }
        "keybinds" => {
            state.reading = Some(Reading::Keybinds);
            state.keys_query = None;
            MenuSend::Nothing
        }
        "settings" => {
            // The page is its own program, `qoro settings`, in a tab of its own: `exec`, so
            // the tab goes when the page closes. By name, so a remote space runs that
            // machine's qoro on that machine's file.
            *request += 1;
            let space = state.space;
            MenuSend::Control(start_agent_terminal(
                state,
                *request,
                "exec qoro settings",
                space,
                Some("settings".to_string()),
            ))
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
fn menu_click(bytes: &[u8], cols: u16, lines: u16, kind: &MenuKind) -> Option<Option<usize>> {
    let rows = menu_rows(kind);
    let title = menu_title(kind);
    let mut at = 0;
    let mut found: Option<Option<usize>> = None;
    while let Some(start) = bytes[at..].windows(3).position(|w| w == b"\x1b[<") {
        let from = at + start;
        match crate::keys::report(&bytes[from..]) {
            crate::keys::Report::Whole(used, action) => {
                if let Some(Action::Click { row, col, .. }) = action {
                    found = Some(crate::screen::menu_row_at(
                        cols, lines, &rows, &title, row, col,
                    ));
                }
                at = from + used;
            }
            crate::keys::Report::Broken(dropped) => at = from + dropped,
            crate::keys::Report::Partial => break,
        }
    }
    found
}

/// A read only overlay opened from the menu.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Reading {
    Keybinds,
}

/// A person being asked what to call the thing they just asked for.
///
/// Asked and not assumed. A space called `space 3` and a tab called `zsh` are both names
/// nobody chose, and a fleet of them is a list a person reads by counting rather than by
/// recognising. Empty is a real answer: it means "you name it", and the default is used.
pub struct Prompt {
    pub what: Naming,
    pub text: String,
    /// The name it has, for a rename: shown faint in the empty box, and what an empty
    /// answer keeps. The box opens empty over it, so a new name is typed straight in
    /// rather than after rubbing the old one out, and a look that changes nothing leaves
    /// it as it was.
    pub was: String,
    /// A space's colour, in its settings box and nowhere else.
    pub ink: Option<InkPick>,
    /// The machine a remote space lives on and how it is reached, for its settings box.
    pub machine: Option<(String, qoro_runtime::control::LinkState)>,
    /// The command an agent's name box starts. Held here so a cancel leaves nothing
    /// behind: it goes with the box.
    pub command: Option<String>,
    /// The start of a sequence the last read ended inside (an arrow, a mouse report), held
    /// for the next read.
    pub(crate) held: Vec<u8>,
}

impl Prompt {
    /// A box asking `what`, over the name the thing has (empty for a new one).
    pub fn new(what: Naming, was: impl Into<String>) -> Self {
        Self {
            what,
            text: String::new(),
            was: was.into(),
            ink: None,
            machine: None,
            command: None,
            held: Vec::new(),
        }
    }
}

/// The colour a space had when its settings box opened, and the one picked since. `None`
/// is auto: the colour its id gives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InkPick {
    pub was: Option<u8>,
    pub now: Option<u8>,
}

impl InkPick {
    /// The next pick along the row, 1 to 6 then auto, round again at either end.
    fn step(self, right: bool) -> Self {
        let at = self.now.map_or(SPACE_INK_COUNT, usize::from);
        let next = (at + if right { 1 } else { SPACE_INK_COUNT }) % (SPACE_INK_COUNT + 1);
        Self {
            now: (next < SPACE_INK_COUNT).then_some(next as u8),
            ..self
        }
    }
}

/// The settings box of `space`: its name, its colour and, for a space of another machine,
/// that machine.
fn space_settings(state: &State, space: i64) -> Prompt {
    let row = state.spaces.iter().find(|s| s.id == space);
    let name = row.map(|s| s.name.clone()).unwrap_or_default();
    let ink = row
        .and_then(|s| s.ink)
        .filter(|n| usize::from(*n) < SPACE_INK_COUNT);
    // The machine the rule's chip reads from the name.
    let machine = row
        .and_then(|s| s.link)
        .map(|link| (sidebar::machine_of(&name).0.to_string(), link));
    Prompt {
        ink: Some(InkPick { was: ink, now: ink }),
        machine,
        ..Prompt::new(Naming::SpaceSettings(space), name)
    }
}

/// The box that names a new agent tab before `command` starts in it.
fn agent_name(space: Option<i64>, command: String) -> Prompt {
    Prompt {
        command: Some(command),
        ..Prompt::new(Naming::AgentName(space), "")
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Naming {
    Space,
    Terminal,
    /// Which agent to run in a new tab, from the palette's agent row. The answer is a
    /// command line, not a tab name: the tab takes the command's first word, and the
    /// whole line is typed into its shell once the tab opens. Any command works, so
    /// an agent the table never heard of starts the same way a listed one does. The space
    /// it starts in, or `None` when none is on screen.
    Agent(Option<i64>),
    /// What to call a new agent tab, asked once its command is chosen. The command rides
    /// in the box; empty gives the tab the command's first word.
    AgentName(Option<i64>),
    /// A space's settings: its name, which is a rename, and its colour. Rename what already
    /// exists, which is the common case: a name given at creation is a guess, and the
    /// terminal somebody opened to "look at something" is the one they are still in an
    /// hour later doing the thing it should have been called after.
    SpaceSettings(i64),
    RenameTerminal(u32),
    /// What this machine calls the conversation in a terminal, over the title its agent
    /// gave it. Opened by a click on the name of the agent already under the keyboard.
    RenameSession(u32),
    /// The ssh address of a machine to connect, from the `+` beside the spaces.
    Remote,
}

impl Naming {
    /// What the box says it is asking for.
    pub fn asks(self) -> &'static str {
        match self {
            Naming::Space => "name the new space",
            Naming::Terminal => "name the new terminal",
            Naming::Agent(_) => "which agent to run",
            Naming::AgentName(_) => "name the new agent",
            Naming::SpaceSettings(_) => "space settings",
            Naming::RenameTerminal(_) => "rename this tab",
            Naming::RenameSession(_) => "rename this session",
            Naming::Remote => "ssh address of the machine",
        }
    }

    /// How many columns an answer may take. An address is longer than a name and is shown
    /// in the box, which is wider than the column a name has to fit. An agent command can
    /// carry flags, so it gets the wider box too: all of the box's row after ` > `, less
    /// the one cell the cursor needs after the last letter. At 55 the cursor sat on it.
    pub fn limit(self) -> usize {
        match self {
            Naming::Remote | Naming::Agent(_) | Naming::RenameSession(_) => 54,
            _ => NAME_LIMIT,
        }
    }

    /// What the field holds, written over it.
    pub fn label(self) -> &'static str {
        match self {
            Naming::Agent(_) => "command",
            Naming::Remote => "address",
            _ => "name",
        }
    }

    /// The word on the enter button.
    pub fn verb(self) -> &'static str {
        match self {
            Naming::Space | Naming::Terminal => "make",
            Naming::Agent(_) | Naming::AgentName(_) => "start",
            Naming::Remote => "connect",
            Naming::SpaceSettings(_) => "save",
            Naming::RenameTerminal(_) | Naming::RenameSession(_) => "rename",
        }
    }

    /// Whether ctrl-r gives the thing back its default name: a tab its program's, a
    /// conversation its agent's title. A space has none, and a box that makes something
    /// has nothing yet to reset.
    pub fn resets(self) -> bool {
        matches!(self, Naming::RenameTerminal(_) | Naming::RenameSession(_))
    }

    /// The line under the field: what an empty answer does here, which is not the same
    /// thing in every case, or the shape an answer takes. `was` is the name a rename keeps.
    pub fn note(self, was: &str) -> String {
        match self {
            Naming::Space | Naming::Terminal | Naming::AgentName(_) => {
                "empty gives it the default name".to_string()
            }
            Naming::Agent(_) => "like claude".to_string(),
            Naming::Remote => "like admin@10.0.0.5".to_string(),
            _ if !was.is_empty() => format!("empty keeps \"{was}\""),
            // A space with no name is a space nobody can point at, so there is nothing for
            // empty to mean and it is a cancel.
            Naming::SpaceSettings(_) => "empty is a cancel".to_string(),
            Naming::RenameTerminal(_) => "empty keeps the program name".to_string(),
            Naming::RenameSession(_) => "empty keeps its own title".to_string(),
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
    ///
    /// The whole box comes back, taken off the screen: what it held besides the text (a
    /// colour picked, the command an agent's name is for) is part of the answer.
    Confirm(Box<Prompt>, Option<String>, Vec<u8>),
    /// Closed, and these bytes belong to the key decoder rather than to the box.
    Passthrough(Vec<u8>),
}

/// What a confirmed box asks of the daemon, in order, with the line saying so set. `name`
/// is what was typed, `None` when nothing was.
fn answer_prompt(
    state: &mut State,
    prompt: Prompt,
    name: Option<String>,
    request: &mut u64,
) -> Vec<ControlRequest> {
    *request += 1;
    let request = *request;
    let what = prompt.what;
    let asks = match what {
        // Empty gives the conversation back its agent's own title, as an empty tab name
        // does its program's. Unchanged is a cancel: confirming the agent's own title
        // would pin it, and its next rename would never show.
        Naming::RenameSession(terminal)
            if name.as_deref()
                == state
                    .rows
                    .iter()
                    .find(|r| r.terminal == terminal)
                    .and_then(|r| r.agent_title.as_deref()) =>
        {
            Vec::new()
        }
        Naming::RenameSession(terminal) => vec![ControlRequest::RenameSession {
            id: format!("rename-{request}"),
            terminal,
            title: name,
        }],
        Naming::RenameTerminal(terminal) => vec![ControlRequest::RenameTerminal {
            id: format!("rename-{request}"),
            terminal,
            // Empty hands the tab back to its program's name, which is the only way to
            // undo a name without inventing a second key for it.
            name,
        }],
        // What changed and nothing else: a name typed over itself is not a rename, and a
        // colour left where it was is not a pick. A space with no name is a space nobody
        // can point at, so an empty name is no rename.
        Naming::SpaceSettings(workspace) => {
            let rename = name.filter(|name| *name != prompt.was).map(|name| {
                ControlRequest::RenameWorkspace {
                    id: format!("rename-{request}"),
                    workspace,
                    name,
                }
            });
            let ink = prompt.ink.filter(|pick| pick.now != pick.was).map(|pick| {
                ControlRequest::SetWorkspaceInk {
                    id: format!("ink-{request}"),
                    workspace,
                    ink: pick.now,
                }
            });
            rename.into_iter().chain(ink).collect()
        }
        Naming::Terminal => vec![ControlRequest::CreateTerminal {
            id: format!("new-{request}"),
            // The new tab follows the focused terminal's directory, on the
            // machine of the space the person is looking at.
            cwd: match state.space {
                Some(space) => focused_cwd_in(state, space),
                None => focused_cwd(state),
            },
            // The space the person is looking at.
            workspace: state.space,
            name,
        }],
        // A command line, not a name: empty is a cancel, as with a space's name, and
        // anything else asks what to call the agent before it starts, listed or not.
        Naming::Agent(space) => {
            if let Some(command) = name {
                state.prompt = Some(agent_name(space, command));
            }
            Vec::new()
        }
        Naming::AgentName(space) => prompt
            .command
            .map(|command| {
                let ask = start_agent_terminal(state, request, &command, space, name);
                match space {
                    Some(space) => follow_elsewhere(state, space, ask),
                    None => ask,
                }
            })
            .into_iter()
            .collect(),
        Naming::Space => vec![ControlRequest::CreateWorkspace {
            id: format!("space-{request}"),
            // Empty means "you name it", which is the numbered name this counted before
            // anybody was asked.
            name: name.unwrap_or_else(|| next_space_name(state)),
        }],
        // An address is what the box is for: empty is a cancel, as with a space's name.
        // Asked of the machine first, by the mux: one with no qoro answers with an offer
        // to put it there.
        Naming::Remote => name
            .map(|target| {
                let id = format!("probe-{request}");
                state.probing = Some(id.clone());
                state.connecting = Some(target.clone());
                ControlRequest::ConnectRemote { id, target }
            })
            .into_iter()
            .collect(),
    };
    let Some(first) = asks.first() else {
        return asks;
    };
    if matches!(what, Naming::Space) {
        state.creating += 1;
    }
    state.message = Some(match what {
        Naming::Terminal => "starting a terminal".to_string(),
        Naming::Agent(_) | Naming::AgentName(_) => match first {
            ControlRequest::CreateTerminal {
                name: Some(tab), ..
            } => format!("starting {tab}"),
            _ => "starting an agent".to_string(),
        },
        Naming::Space => "starting a space".to_string(),
        Naming::SpaceSettings(_) => "saved".to_string(),
        Naming::RenameTerminal(_) | Naming::RenameSession(_) => "renamed".to_string(),
        Naming::Remote => format!(
            "connecting to {}",
            state.connecting.as_deref().unwrap_or_default()
        ),
    });
    asks
}

/// Decide what one read of stdin meant while the naming box owns the keyboard.
///
/// Typing goes into the name, `enter` confirms it, `esc` is out, and backspace rubs out.
/// A prefix or a bare detach closes the box and hands the bytes on from there, so
/// `ctrl-a d` still detaches out of it, which is the same rule the palette follows.
/// The most a key sequence split across reads is held for: anything longer that has not
/// ended is not one.
const HELD_MAX: usize = 64;

fn prompt_key(state: &mut State, bytes: &[u8]) -> PromptOutcome {
    let prefix = state.config.prefix;
    let (cols, lines) = (state.cols, state.lines);
    let Some(prompt) = state.prompt.as_mut() else {
        return PromptOutcome::Passthrough(bytes.to_vec());
    };
    let colours = crate::screen::prompt_colours(cols, lines, prompt);
    let input = [std::mem::take(&mut prompt.held).as_slice(), bytes].concat();
    let bytes = input.as_slice();
    let mut at = 0;
    while at < bytes.len() {
        let byte = &bytes[at];
        // An escape sequence, which here is never a name, read the way the modes read
        // one. A mouse report is the one that matters: the box is opened by a click on the
        // `+`, and the release of that very click arrives a moment later. Read byte by
        // byte its leading escape is a cancel, so the box opened and shut again before
        // anybody could type into it, which looked like the button doing nothing at all.
        // A bare esc falls through to the cancel below.
        if *byte == 0x1b {
            let (key, len) = mode_key(bytes, at);
            match key {
                ModeKey::Esc => {}
                // Left and right move a space's colour along its row. Anywhere else, and
                // in a box too small to show the row, they are nothing.
                ModeKey::Left | ModeKey::Right => {
                    if colours && let Some(pick) = prompt.ink.as_mut() {
                        *pick = pick.step(key == ModeKey::Right);
                    }
                    at += len;
                    continue;
                }
                // Split across the read. Held rather than passed on, so half a report
                // cannot land in a name, and half an arrow still moves the colour.
                ModeKey::Partial => {
                    if bytes.len() - at <= HELD_MAX {
                        prompt.held = bytes[at..].to_vec();
                    }
                    break;
                }
                // A press on the box: a button is its key, typed where the report was
                // with whatever came after it still behind, and a swatch is its colour.
                ModeKey::Press => {
                    let hit = match crate::keys::mouse(&bytes[at..at + len]) {
                        Some(Some(crate::keys::Action::Click { row, col, .. })) => {
                            crate::screen::prompt_click(
                                cols,
                                lines,
                                prompt,
                                usize::from(row).saturating_sub(1),
                                usize::from(col).saturating_sub(1),
                            )
                        }
                        _ => None,
                    };
                    match hit {
                        // Cancel closes here: typed ahead of what followed, an escape
                        // could read as the start of a sequence.
                        Some(crate::screen::BoxClick::Key(0x1b)) => {
                            state.prompt = None;
                            return PromptOutcome::Closed;
                        }
                        Some(crate::screen::BoxClick::Key(key)) => {
                            return prompt_key(state, &[&[key][..], &bytes[at + len..]].concat());
                        }
                        Some(crate::screen::BoxClick::Pick(now)) => {
                            if let Some(pick) = prompt.ink.as_mut() {
                                pick.now = now;
                            }
                        }
                        None => {}
                    }
                    at += len;
                    continue;
                }
                // Anything else, a release, the wheel, Delete or a page key, is dropped
                // whole, so the keys after it still answer the box.
                _ => {
                    at += len;
                    continue;
                }
            }
        }
        at += 1;
        let at = at - 1;
        match byte {
            // Enter.
            b'\r' | b'\n' => {
                let text = prompt.text.trim().to_string();
                // A pick the box is too small to show is not an answer anybody saw.
                if !colours && let Some(pick) = prompt.ink.as_mut() {
                    pick.now = pick.was;
                }
                let picked = prompt.ink.is_some_and(|pick| pick.now != pick.was);
                // Nothing typed over a name keeps it, the same as esc, unless a colour was
                // picked: that is still an answer. The keys after the enter still run.
                if text.is_empty() && !prompt.was.is_empty() && !picked {
                    state.prompt = None;
                    return PromptOutcome::Passthrough(bytes[at + 1..].to_vec());
                }
                let Some(taken) = state.prompt.take() else {
                    return PromptOutcome::Closed;
                };
                return PromptOutcome::Confirm(
                    Box::new(taken),
                    (!text.is_empty()).then_some(text),
                    bytes[at + 1..].to_vec(),
                );
            }
            // Escape, which here is a cancel and never the start of a sequence: the arrows
            // are read whole above, and waiting for a next read that may never come would
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
            byte if *byte == prefix || *byte == crate::keys::DETACH => {
                state.prompt = None;
                return PromptOutcome::Passthrough(bytes[at..].to_vec());
            }
            // ctrl-u, the line kill every shell knows: start the answer over.
            0x15 => prompt.text.clear(),
            // ctrl-r, back to the default name, where the thing has one to go back to.
            0x12 if prompt.what.resets() => {
                let Some(taken) = state.prompt.take() else {
                    return PromptOutcome::Closed;
                };
                return PromptOutcome::Confirm(Box::new(taken), None, bytes[at + 1..].to_vec());
            }
            byte if (byte.is_ascii_graphic() || *byte == b' ')
                && crate::width::width(&prompt.text) < prompt.what.limit() =>
            {
                prompt.text.push(*byte as char);
            }
            // The lead byte of a character outside ASCII: a name in a person's own
            // language (`bağlama`) is a name. Taken whole when the read holds all of it.
            0xc2..=0xf4 => {
                let len = match byte {
                    0xc2..=0xdf => 2,
                    0xe0..=0xef => 3,
                    _ => 4,
                };
                let whole = bytes
                    .get(at..at + len)
                    .and_then(|b| std::str::from_utf8(b).ok())
                    .and_then(|s| s.chars().next());
                if let Some(ch) = whole {
                    let wide = crate::width::columns(ch);
                    // A combining mark joins the letter before it: `g` and U+0306 is
                    // the `ğ` a name copied from Finder spells that way.
                    if (wide > 0 || !prompt.text.is_empty())
                        && crate::width::width(&prompt.text) + wide <= prompt.what.limit()
                    {
                        prompt.text.push(ch);
                    }
                }
                // Its continuation bytes fall to the last arm and are dropped.
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

/// The wheel over the open column moves its list, which is longer than the column; true
/// when the list moved.
fn wheel_list(state: &mut State, row: u16, col: u16, up: bool) -> bool {
    let left = screen::left_for(state.cols, state.collapsed, state.sidebar_width);
    if state.collapsed || left == 0 || col as usize > left || (row as usize) < screen::SIDEBAR_TOP {
        return false;
    }
    let column_rows = screen::column_rows(state.lines as usize);
    let column = state.column(state.paint_now, 0);
    // Over the inbox the wheel is the inbox's: its list scrolls, its tab row walks the tabs.
    match state.column_hit(row).map(|(hit, _)| hit) {
        Some(sidebar::Hit::Next(_)) => {
            let at = sidebar::inbox_wheeled(
                &column,
                column_rows,
                sidebar::current_avatar().as_ref(),
                up,
            );
            let moved = at != state.inbox.scroll;
            state.inbox.scroll = at;
            return moved;
        }
        Some(sidebar::Hit::Tabs) if !state.inbox.folded => {
            state.inbox.tab = state.inbox.tab.step(up);
            state.inbox.scroll = 0;
            return true;
        }
        _ => {}
    }
    let at = sidebar::wheeled(
        &column,
        column_rows,
        sidebar::current_avatar().as_ref(),
        up,
        WHEEL_ROWS,
    );
    let next = Some((state.column_terminal(), at));
    let moved = next != state.list_scroll;
    state.list_scroll = next;
    moved
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
    let prefix = state.config.prefix;
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
        // A mouse report goes to the ordinary path whole, list still open: the wheel arm
        // there scrolls the list and a click closes it. Treated as a key, its esc was the
        // close and the rest of the report was typed into the terminal as text.
        if rest.starts_with(b"\x1b[<") {
            return FocusOutcome::Passthrough(rest.to_vec());
        }
        // Everything else closes first; what differs is what survives it.
        close_focus(state);
        // The close keystroke itself is dropped; what follows it is typed.
        let from = if rest[0] == prefix && rest.get(1) == Some(&b'F') {
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

/// What one read of stdin meant while a mode owns the keyboard.
#[derive(Debug, PartialEq, Eq)]
enum ModeOutcome {
    /// Taken by the mode: repaint.
    Stay,
    /// The mode was left, and these bytes are ordinary input after all.
    Passthrough(Vec<u8>),
}

/// One key as the modes read it.
#[derive(Debug, PartialEq, Eq)]
enum ModeKey {
    Byte(u8),
    /// A run of bytes above ASCII: typed text in some other script.
    Text(String),
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Esc,
    /// The wheel, up or not.
    Wheel(bool),
    /// A left button press: a click, which leaves a mode and then acts as itself.
    Press,
    Home,
    End,
    /// A sequence the modes have no use for: dropped whole, never typed.
    Skip,
    /// The start of a sequence the read ended inside, held for the next read.
    Partial,
}

/// The key at `at`, and how many bytes it took.
///
/// A sequence the read ended inside is `Partial`, and the caller holds it for the next read
/// the way the key decoder holds half a mouse report: dropped, its tail would be read as
/// keys of its own and typed into the program in the pane. A lone `ESC` is never held,
/// since it is the key that leaves.
fn mode_key(bytes: &[u8], at: usize) -> (ModeKey, usize) {
    let rest = &bytes[at..];
    match rest {
        // The key decoder's own reading of a mouse report, so the two never disagree.
        [0x1b, b'[', b'<', ..] => match crate::keys::report(rest) {
            crate::keys::Report::Partial => (ModeKey::Partial, rest.len()),
            crate::keys::Report::Broken(used) => (ModeKey::Skip, used.max(1)),
            crate::keys::Report::Whole(used, action) => {
                let key = match action {
                    Some(Action::Wheel { up, .. }) => ModeKey::Wheel(up),
                    Some(Action::Click { .. }) => ModeKey::Press,
                    _ => ModeKey::Skip,
                };
                (key, used)
            }
        },
        [0x1b, b'[', ..] => {
            let Some(end) = rest[2..].iter().position(|b| (0x40..=0x7e).contains(b)) else {
                return (ModeKey::Partial, rest.len());
            };
            let key = match &rest[..end + 3] {
                b"\x1b[A" => ModeKey::Up,
                b"\x1b[B" => ModeKey::Down,
                b"\x1b[C" => ModeKey::Right,
                b"\x1b[D" => ModeKey::Left,
                b"\x1b[5~" => ModeKey::PageUp,
                b"\x1b[6~" => ModeKey::PageDown,
                b"\x1b[H" | b"\x1b[1~" => ModeKey::Home,
                b"\x1b[F" | b"\x1b[4~" => ModeKey::End,
                _ => ModeKey::Skip,
            };
            (key, end + 3)
        }
        [0x1b, b'O'] => (ModeKey::Partial, 2),
        [0x1b, b'O', last, ..] => {
            let key = match last {
                b'A' => ModeKey::Up,
                b'B' => ModeKey::Down,
                b'C' => ModeKey::Right,
                b'D' => ModeKey::Left,
                b'H' => ModeKey::Home,
                b'F' => ModeKey::End,
                _ => ModeKey::Skip,
            };
            (key, 3)
        }
        [0x1b, ..] => (ModeKey::Esc, 1),
        [first, ..] if *first >= 0x80 => {
            let run = rest.iter().take_while(|b| **b >= 0x80).count();
            match std::str::from_utf8(&rest[..run]) {
                Ok(text) => (ModeKey::Text(text.to_string()), run),
                // Cut inside a character at the end of the read: the rest is on its way.
                Err(e) if e.error_len().is_none() && run == rest.len() && e.valid_up_to() == 0 => {
                    (ModeKey::Partial, run)
                }
                Err(e) => {
                    let good = e.valid_up_to().max(1);
                    (
                        ModeKey::Text(String::from_utf8_lossy(&rest[..good]).into_owned()),
                        good,
                    )
                }
            }
        }
        [first, ..] => (ModeKey::Byte(*first), 1),
        [] => (ModeKey::Skip, 0),
    }
}

/// Hand one read to whichever mode is open, after whatever the last read left unfinished.
fn mode_input(state: &mut State, bytes: &[u8], prefix: u8, request: &mut u64) -> ModeOutcome {
    let mut input = std::mem::take(&mut state.modes.pending);
    input.extend_from_slice(bytes);
    let outcome = if let Some(space) = state.armed {
        armed_key(state, space, &input, request)
    } else if state.modes.confirm_close.is_some() {
        confirm_key(state, &input, request)
    } else if state.modes.resizing {
        resize_key(state, &input)
    } else {
        copy_key(state, &input, prefix, request)
    };
    // Held for a mode that is still open, and never more than a sequence's worth: anything
    // longer that has not ended is not one.
    if !state.modes.open() || state.modes.pending.len() > HELD_MAX {
        state.modes.pending.clear();
    }
    outcome
}

/// The question to ask before closing the focused terminal, when closing it would end an
/// agent in the middle of something. Shells and resting agents close at once.
fn close_question(state: &State, terminal: u32) -> Option<String> {
    let row = state.rows.iter().find(|r| r.terminal == terminal)?;
    (row.agent && matches!(row.state.as_str(), "working" | "blocked" | "starting")).then(|| {
        // A capital Y: a blocked agent is usually waiting for a lowercase y of its own, and
        // a stray prefix and x followed by the answer meant for it must not end it.
        format!(
            "close {} ({})? Y closes it, any other key keeps it",
            row.program.as_deref().unwrap_or("this agent"),
            row.state
        )
    })
}

/// Close the focused terminal, or ask first when an agent in it is busy.
///
/// With the keyboard on a sleeping tab instead, that tab is discarded: the daemon closes it
/// for good without waking its space, so there is nothing running to ask about.
fn close_focused(state: &mut State, request: &mut u64) {
    if state.focused == 0 {
        if let Some(terminal) = state.dim_tab() {
            *request += 1;
            state.modes.outbox.push(ControlRequest::CloseTerminal {
                id: format!("close-{request}"),
                terminal,
            });
        }
        return;
    }
    close_terminal(state, state.focused, request);
}

/// A space rule's drawer of controls: which space, how many places are out, and whether it
/// is opening or going back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Drawer {
    space: i64,
    shown: usize,
    open: bool,
}

impl State {
    /// What the column draws of the drawer: the space and how many places are out.
    fn drawer_view(&self) -> Option<(i64, usize)> {
        self.drawer.map(|d| (d.space, d.shown))
    }

    /// Whether the drawer still has a place to move, so the animation clock keeps running.
    fn drawer_moving(&self) -> bool {
        self.drawer.is_some_and(|d| {
            if d.open {
                d.shown < sidebar::DRAWER
            } else {
                true
            }
        })
    }

    /// The toggle on a rule: open that rule's drawer, closing any other at once, or send
    /// an open one back.
    fn toggle_drawer(&mut self, space: i64) {
        self.drawer = match self.drawer {
            Some(d) if d.space == space && d.open => Some(Drawer { open: false, ..d }),
            Some(d) if d.space == space => Some(Drawer { open: true, ..d }),
            _ => Some(Drawer {
                space,
                shown: 0,
                open: true,
            }),
        };
    }

    /// One animation frame: one place more out, or one back, until it is open or gone.
    fn step_drawer(&mut self) {
        self.drawer = match self.drawer {
            Some(d) if d.open => Some(Drawer {
                shown: (d.shown + 1).min(sidebar::DRAWER),
                ..d
            }),
            Some(d) if d.shown > 1 => Some(Drawer {
                shown: d.shown - 1,
                ..d
            }),
            _ => None,
        };
    }

    /// Where the drawer is going, at once: for `QORO_MOTION=0`, which has no frames.
    fn settle_drawer(&mut self) {
        self.drawer = self.drawer.filter(|d| d.open).map(|d| Drawer {
            shown: sidebar::DRAWER,
            ..d
        });
    }
}

/// Turn keeping awake over for a space, or with `terminal` one pane of it: on when it was
/// off, off when it was on, as the last list said. Sent with the value rather than as a
/// toggle, so two quick clicks on a stale list cannot cancel out on the daemon.
fn keep_awake(state: &mut State, space: i64, terminal: Option<u32>, request: &mut u64) {
    let was = match terminal {
        Some(t) => state.rows.iter().any(|r| r.terminal == t && r.keep_awake),
        None => state.spaces.iter().any(|s| s.id == space && s.keep_awake),
    };
    *request += 1;
    state.modes.outbox.push(ControlRequest::SetKeepAwake {
        id: format!("awake-{request}"),
        workspace: space,
        terminal,
        on: !was,
    });
}

/// Close one terminal, a pane and only that pane, or ask first when an agent in it is busy.
/// The last pane of a tab takes the tab with it; in a split the other panes stay. A sleeping
/// tab has nothing running to ask about, and the daemon discards it without waking it.
fn close_terminal(state: &mut State, terminal: u32, request: &mut u64) {
    match close_question(state, terminal) {
        Some(question) => state.modes.confirm_close = Some((terminal, question)),
        None => {
            *request += 1;
            state.modes.outbox.push(ControlRequest::CloseTerminal {
                id: format!("close-{request}"),
                terminal,
            });
        }
    }
}

/// `ctrl-a Z`: put the tab the keyboard is in to sleep, every pane of it, or wake the
/// sleeping tab the keyboard rests on. Sent through the outbox like a close; a refusal comes
/// back on the hint row with the daemon's reason.
fn sleep_toggle(state: &mut State, request: &mut u64) {
    let Some(space) = state.space else {
        state.message = Some("no tab to put to sleep".to_string());
        return;
    };
    if state.focused != 0 {
        *request += 1;
        state.message = Some("putting this tab to sleep".to_string());
        state.modes.outbox.push(ControlRequest::SleepWorkspace {
            id: format!("sleep-tab-{request}"),
            workspace: space,
            terminal: Some(state.focused),
        });
    } else if let Some(tab) = state.dim_tab() {
        wake(state, space, tab, request);
    }
}

/// The rule's own `sleep`: put every tab of that space to sleep. It never wakes: the rule's
/// `wake` does, and a sleeping tab wakes alone, by a click or a key on it.
fn sleep_space(state: &mut State, space: i64, request: &mut u64) {
    *request += 1;
    state.message = Some(format!("putting {} to sleep", space_name(state, space)));
    state.modes.outbox.push(ControlRequest::SleepWorkspace {
        id: format!("sleep-{request}"),
        workspace: space,
        terminal: None,
    });
}

/// The rule's own `wake`: every sleeping tab of that space comes back. Not a tab click,
/// which wakes only its tab, and not `+`, which opens a tab beside the sleeping ones: the
/// word on the rule says this is the whole space.
fn wake_space(state: &mut State, space: i64, request: &mut u64) {
    *request += 1;
    state.message = Some(format!("waking {}", space_name(state, space)));
    state.modes.outbox.push(ControlRequest::WakeWorkspace {
        id: format!("wake-{request}"),
        workspace: Some(space),
        terminal: None,
    });
}

/// Wake one sleeping tab, every pane of it, and show it once the index lists it.
fn wake(state: &mut State, space: i64, tab: u32, request: &mut u64) {
    *request += 1;
    state.waking = Some((space, tab));
    state.message = Some("waking this tab".to_string());
    state.modes.outbox.push(ControlRequest::WakeWorkspace {
        id: format!("wake-tab-{request}"),
        workspace: Some(space),
        terminal: Some(tab),
    });
}

/// Whether an index says a space was put to sleep or woken somewhere else (`qoro sleep`,
/// another client): nothing announces it but its tabs leaving or coming back, so an awake
/// space that held tabs and now lists none, or a sleeping one with tabs listed, asks for
/// the space list again.
fn slept_or_woken(spaces: &[qoro_runtime::control::WorkspaceRow], agents: &[AgentRow]) -> bool {
    spaces.iter().any(|space| {
        let held = agents.iter().filter(|r| r.workspace == space.id).count();
        held != space.terminals
    })
}

/// A space as the column names it, for a sentence on the notice row.
fn space_name(state: &State, space: i64) -> String {
    state
        .spaces
        .iter()
        .find(|w| w.id == space)
        .map_or_else(|| "this space".to_string(), |w| w.name.clone())
}

/// Whether there is nothing awake left to be an interface to: no terminal anywhere, and no
/// awake remote space that comes back on its own when its machine answers. Sleeping spaces
/// do not count, whoever put them to sleep: the interface detaches, as `ctrl-q` does.
fn nothing_awake(state: &State) -> bool {
    state.adopted
        && state.rows.is_empty()
        && !state
            .spaces
            .iter()
            .any(|w| !w.asleep && crate::mux::is_remote_workspace(w.id))
}

/// Whether a read is a mouse report rather than keys, which is what an armed rule has to
/// let through: the question is on a row, and a row is answered with a click.
/// A read that begins a mouse report and has not finished one.
///
/// The decoder buffers the rest of it, so a read that looks like this belongs to the mouse
/// arm and must not be read as a keypress by anything upstream of it.
fn is_report_head(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x1b[<") && !bytes.iter().any(|b| *b == b'M' || *b == b'm')
}

fn is_mouse(bytes: &[u8]) -> bool {
    if !bytes.starts_with(b"\x1b[<") {
        return false;
    }
    // A read is whatever the kernel had, and can carry a key and a report in one. The
    // question is whether anything in it is a mouse report at all, because a read that is
    // nothing but reports is the only shape that can be handed to the mouse arm whole: a
    // mixed read is left to the ordinary path, where the key is a key and the report is a
    // click, which is what both of them are.
    let mut rest = bytes;
    while !rest.is_empty() {
        if !rest.starts_with(b"\x1b[<") {
            return false;
        }
        // A report ends at its `M` (a press) or `m` (a release), and the next report or
        // the next key starts there.
        let end = match rest.iter().position(|b| *b == b'M' || *b == b'm') {
            Some(at) => at + 1,
            None => return false,
        };
        rest = &rest[end..];
    }
    true
}

/// Every tab a close of this space asks the daemon about, in the order they are asked.
///
/// The index and the store, because a sleeping space has no rows in the index and every
/// tab in the store, and a question that says zero would be a lie. Deduped, because a tab
/// can be in both lists when the space fell asleep between this client's last index and the
/// click that armed the question, and [`arm_close`] counts this list rather than counting
/// the two sources itself: the count a person was told and the count the daemon is asked
/// are the same number or one of them is a lie.
fn tabs_to_close(state: &State, space: i64) -> Vec<u32> {
    let mut asked: Vec<u32> = state
        .rows
        .iter()
        .filter(|r| r.workspace == space)
        .map(|r| r.terminal)
        .collect();
    asked.extend(
        state
            .spaces
            .iter()
            .find(|w| w.id == space)
            .map(|w| w.sleeping.iter().map(|t| t.terminal).collect::<Vec<_>>())
            .unwrap_or_default(),
    );
    asked.sort_unstable();
    asked.dedup();
    asked
}

/// The answer to a space rule's own question: `y` closes it, `n` or anything else keeps it.
///
/// Take a rule's question down and say that its space was kept, for a click or a wheel that
/// is not the run loop's `Click` arm and so does not go through [`put_question_down`].
fn take_question(state: &mut State) -> Option<String> {
    state
        .armed
        .take()
        .map(|space| format!("kept {}", space_name(state, space)))
}

/// Put a rule's question down, unless the click is about the question.
///
/// A click puts the question down whatever it lands on, so a rule cannot sit on screen
/// asking while the keyboard is dead and its row has scrolled out of view. The three
/// clicks that are about it keep it: the arm that asked, and the two answers. Everything
/// else puts it down here rather than in an arm, because an arm can only run after every
/// other arm has declined to match, and the arms that change the view are exactly the ones
/// a question must not survive: a question with no rule on screen to ask on still holds the
/// keyboard, and `y` would then close a space over a row nobody can see.
fn put_question_down(state: &mut State, clicked: Option<Clicked>) -> Option<i64> {
    let asked = state.armed.take();
    let about_the_question = matches!(
        clicked,
        Some(Clicked::Close(_)) | Some(Clicked::ConfirmClose(_)) | Some(Clicked::CancelClose)
    );
    if let Some(space) = asked
        && !about_the_question
    {
        state.message = Some(format!("kept {}", space_name(state, space)));
    }
    asked
}

/// Lowercase, unlike the tab close's capital `Y`. That one is about a stray `y` typed into
/// a pane waiting for an answer of its own, and this one is about a key pressed while a
/// rule on screen is asking, where the person has read the question and is answering it.
/// The bytes are not typed anywhere: the question owns them.
fn armed_key(state: &mut State, space: i64, bytes: &[u8], request: &mut u64) -> ModeOutcome {
    state.armed = None;
    if bytes.first() == Some(&b'y') {
        if start_close(state, space, request) {
            state.close_begun = true;
        }
    } else {
        state.message = Some(format!("kept {}", space_name(state, space)));
    }
    ModeOutcome::Stay
}

/// Put a space's rule into the state where it asks whether it should close.
///
/// Nothing is sent. The question is on the rule itself, one click from where the close was,
/// and the answer is a second click on the same row: the rule repaints with `y` and `no` in
/// the two columns its controls occupied.
///
/// The notice row carries what the rule has no room for, which is the part a person reads
/// before answering: how many tabs go, and which of them is working.
fn arm_close(state: &mut State, space: i64) {
    let held: Vec<&AgentRow> = state.rows.iter().filter(|r| r.workspace == space).collect();
    // The same list the close asks the daemon about, deduped, so the count a person reads
    // on the question and the number of `CloseTerminal` requests that follow are one
    // number. A tab in both the index and the store is one tab, not two.
    let count = tabs_to_close(state, space).len();
    let busy: Vec<String> = held
        .iter()
        .filter(|r| r.agent && matches!(r.state.as_str(), "working" | "blocked" | "starting"))
        .map(|r| {
            format!(
                "{} in tab {} is {}",
                r.program.as_deref().unwrap_or("an agent"),
                r.terminal,
                r.state
            )
        })
        .collect();
    state.armed = Some(space);
    state.message = Some(if busy.is_empty() {
        format!(
            "close {}? {count} tab{}",
            space_name(state, space),
            if count == 1 { "" } else { "s" }
        )
    } else {
        format!(
            "close {}? {count} tab{}, {}",
            space_name(state, space),
            if count == 1 { "" } else { "s" },
            busy.join(", ")
        )
    });
}

/// Begin the close a person answered `y` to: the space is marked, and its tabs go one per
/// answer rather than as a burst.
///
/// The burst is the obvious implementation and it is wrong twice over. `CloseTerminal`
/// answers `TerminalClosed` when the terminal's command channel accepts the close, which is
/// before the store row is closed, so the daemon still counts the tab; and `dispatch` sends
/// a request off the connection's read loop when the space lock is not free right now, so a
/// burst can execute out of order and the close of the space can land before a single tab
/// does. One at a time, each pushed when the previous one answers, is the only ordering this
/// protocol can give.
/// Answers whether a close was started, so the caller arms the give-up timer for the
/// keyboard path as well as the mouse one. A close refused because one is already running
/// answers false, and the timer is left alone.
fn start_close(state: &mut State, space: i64, request: &mut u64) -> bool {
    // One close at a time. The `x` is on every rule while a close is in flight, so asking
    // about another space is one click away, and overwriting the state would leave the
    // first space's tabs half closed with nothing chasing them and the give-up message
    // naming the wrong space.
    if let Some(closing) = state.closing.as_ref() {
        state.message = Some(if closing.space == space {
            format!("{} is already closing", space_name(state, space))
        } else {
            format!(
                "{} is still closing, {} waits",
                space_name(state, closing.space),
                space_name(state, space)
            )
        });
        return false;
    }
    let asked = tabs_to_close(state, space);
    let left = asked.len();
    state.closing = Some(Closing {
        space,
        asked,
        sent: 0,
        space_asked: false,
        space_asked_id: None,
        stuck: None,
        stuck_tries: 0,
        pending: Vec::new(),
    });
    if left == 0 {
        // Nothing in it and nothing running: this is the case `CloseWorkspace` was built
        // for, and there is nothing to wait for.
        close_the_space(state, request);
        return true;
    }
    state.message = Some(format!(
        "closing {left} {} of {}",
        plural(left),
        space_name(state, space)
    ));
    close_one(state, request);
    true
}

/// Send the next tab's close, if there is one.
fn close_one(state: &mut State, request: &mut u64) {
    let Some(closing) = state.closing.as_mut() else {
        return;
    };
    // The next tab, or the one that refused if it has not been asked too often already.
    let terminal = match closing.stuck {
        Some(stuck) if closing.stuck_tries < STUCK_TRIES => {
            closing.stuck_tries += 1;
            stuck
        }
        // Asked enough times. The chain leaves this tab behind rather than stalling, and
        // the space is left open: the daemon would refuse a close over a tab that is still
        // there, and the person is told which one now rather than after the wait.
        Some(_) => {
            // `stuck` stays and the counter stays spent, so the give-up can name the tab
            // that would not close: it is the one thing a person can do something about.
            match next_tab(closing) {
                Some(terminal) => terminal,
                None => return,
            }
        }
        None => match next_tab(closing) {
            Some(terminal) => terminal,
            None => return,
        },
    };
    *request += 1;
    let id = format!("close-{request}");
    if let Some(closing) = state.closing.as_mut() {
        closing.pending.push((id.clone(), terminal));
    }
    state
        .modes
        .outbox
        .push(ControlRequest::CloseTerminal { id, terminal });
}

/// The next tab on the list, skipping one that has given up, and counting it as asked.
///
/// The next tab to ask, and the only place `sent` moves. A tab the chain gave up on stays
/// in `asked` (the count a person was told includes it) but is never handed out again, and
/// it does not hold up the tabs behind it.
fn next_tab(closing: &mut Closing) -> Option<u32> {
    let terminal = closing.asked.get(closing.sent).copied()?;
    closing.sent += 1;
    Some(terminal)
}

/// Ask the daemon to close the space itself, which it will only do once nothing in it is
/// left. The answer is not waited for here: the client finds out from the index and from the
/// response, and a refusal names what is still in the way.
fn close_the_space(state: &mut State, request: &mut u64) {
    let Some(closing) = state.closing.as_ref() else {
        return;
    };
    let space = closing.space;
    let Some(closing) = state.closing.as_mut() else {
        return;
    };
    if closing.space_asked {
        return;
    }
    closing.space_asked = true;
    *request += 1;
    let id = format!("close-space-{request}");
    if let Some(closing) = state.closing.as_mut() {
        closing.space_asked_id = Some(id.clone());
    }
    state.modes.outbox.push(ControlRequest::CloseWorkspace {
        id,
        workspace: space,
    });
}

/// Ask the daemon to close the space, once every tab is gone and the daemon's own count is
/// zero.
///
/// Called on the index and on the space list, and both are needed. A waking space empties
/// in the index, because its tabs are live rows. A sleeping space empties in the space
/// list, because its tabs are `sleeping` entries and the index never lists them: the two
/// answers arrive in the order the loop asks for them, the index first, against the list
/// that still says the space holds what it held a moment ago, and nothing asks again after
/// that. With the gate on the index alone a sleeping space waits for the give-up timer and
/// blames a tab that never refused anything.
fn close_gate(state: &mut State, request: &mut u64) {
    if let Some(closing) = state.closing.clone()
        && !closing.space_asked
        && closing.sent >= closing.asked.len()
        && space_is_empty(state, closing.space)
    {
        close_the_space(state, request);
    }
}

/// Whether the daemon's count of a space is zero, as far as this client can tell.
///
/// The index reads the live registry and `CloseWorkspace` counts store rows, and the two
/// agree only while every row is live. A sleeping space is the case they part company on:
/// its exit hook keeps the row open so a wake can bring it back, so the index shows the
/// space empty while the store still holds every tab. The gate is therefore on the tabs
/// being gone rather than on the space waking, because the asleep flag is cleared by a wake
/// or by the close itself and neither is coming on its own.
fn space_is_empty(state: &State, space: i64) -> bool {
    !state.rows.iter().any(|r| r.workspace == space)
        && state
            .spaces
            .iter()
            .find(|w| w.id == space)
            .is_none_or(|w| w.sleeping.is_empty())
}

/// How many tabs a space still holds, counting the store's view and not the index's, so a
/// sleeping space is counted rather than reported as empty.
fn tabs_left(state: &State, space: i64) -> usize {
    let live = state.rows.iter().filter(|r| r.workspace == space).count();
    let sleeping = state
        .spaces
        .iter()
        .find(|w| w.id == space)
        .map_or(0, |w| w.sleeping.len());
    live + sleeping
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "tab" } else { "tabs" }
}

/// The answer to "close it?": `Y` closes, anything else is a no and is not typed.
fn confirm_key(state: &mut State, bytes: &[u8], request: &mut u64) -> ModeOutcome {
    let Some((terminal, _)) = state.modes.confirm_close.take() else {
        return ModeOutcome::Passthrough(bytes.to_vec());
    };
    if bytes.first() != Some(&b'Y') {
        let name = state
            .rows
            .iter()
            .find(|r| r.terminal == terminal)
            .and_then(|r| r.program.clone())
            .unwrap_or_else(|| format!("terminal {terminal}"));
        state.message = Some(format!("kept {name}"));
    } else {
        *request += 1;
        state.modes.outbox.push(ControlRequest::CloseTerminal {
            id: format!("close-{request}"),
            terminal,
        });
    }
    ModeOutcome::Stay
}

/// Open resize mode, when there is a divider to move.
fn enter_resize(state: &mut State) {
    let panes = state
        .space
        .and_then(|space| state.layouts.get(&space))
        .map_or(0, |layout| layout.visible().len());
    let zoomed = state
        .space
        .and_then(|space| state.layouts.get(&space))
        .is_some_and(|layout| layout.zoomed.is_some());
    if zoomed {
        state.message = Some(crate::keys::named("zoomed: ctrl-a z first, then ctrl-a e"));
        return;
    }
    if panes < 2 {
        state.message = Some(crate::keys::named(
            "one pane, nothing to resize: ctrl-a v or ctrl-a - splits it",
        ));
        return;
    }
    state.modes.resizing = true;
}

/// Even every pane out, and size the focused one to its new tile at the next paint.
fn equalize(state: &mut State) {
    if let Some(layout) = state.space.and_then(|space| state.layouts.get_mut(&space)) {
        layout.equalize();
    }
}

/// Decide what one read of stdin meant in resize mode.
///
/// `h j k l` and the arrows move the nearest divider 5%, `H J K L` 20%, `=` evens everything
/// out; `esc`, `enter`, `q` and `e` leave. The prefix leaves and is passed on, so `ctrl-a d`
/// still detaches out of it, and any other key leaves and is read again as ordinary input:
/// a person who starts typing has finished resizing.
fn resize_key(state: &mut State, bytes: &[u8]) -> ModeOutcome {
    let area = pane_area(state);
    let mut at = 0;
    while at < bytes.len() {
        let (key, used) = mode_key(bytes, at);
        let nudge = match key {
            ModeKey::Byte(b'h') | ModeKey::Left => Some((layout::Move::Left, 0.05)),
            ModeKey::Byte(b'j') | ModeKey::Down => Some((layout::Move::Down, 0.05)),
            ModeKey::Byte(b'k') | ModeKey::Up => Some((layout::Move::Up, 0.05)),
            ModeKey::Byte(b'l') | ModeKey::Right => Some((layout::Move::Right, 0.05)),
            ModeKey::Byte(b'H') => Some((layout::Move::Left, 0.2)),
            ModeKey::Byte(b'J') => Some((layout::Move::Down, 0.2)),
            ModeKey::Byte(b'K') => Some((layout::Move::Up, 0.2)),
            ModeKey::Byte(b'L') => Some((layout::Move::Right, 0.2)),
            _ => None,
        };
        if let Some((mv, step)) = nudge {
            if let Some(layout) = state.space.and_then(|space| state.layouts.get_mut(&space)) {
                layout.nudge(area, mv, step);
            }
        } else {
            match key {
                ModeKey::Byte(b'=') => equalize(state),
                ModeKey::Esc | ModeKey::Byte(b'\r' | b'\n' | b'q' | b'e') => {
                    state.modes.resizing = false;
                    let rest = &bytes[at + used..];
                    return if rest.is_empty() {
                        ModeOutcome::Stay
                    } else {
                        ModeOutcome::Passthrough(rest.to_vec())
                    };
                }
                ModeKey::Skip => {}
                ModeKey::Partial => {
                    state.modes.pending = bytes[at..].to_vec();
                    break;
                }
                _ => {
                    // The prefix, a detach, a click, or typing: not this mode's.
                    state.modes.resizing = false;
                    return ModeOutcome::Passthrough(bytes[at..].to_vec());
                }
            }
        }
        at += used.max(1);
    }
    ModeOutcome::Stay
}

/// One row of the focused terminal by its number: history below the screen's first row,
/// the live screen from there. `None` for a history row this client has not fetched.
fn copy_row(state: &State, at: u64) -> Option<String> {
    let total = state.live_behind();
    if at >= total {
        return state
            .snapshot
            .as_ref()?
            .lines
            .get((at - total) as usize)
            .cloned();
    }
    (state.behind.terminal == state.focused)
        .then(|| state.behind.row(at).map(str::to_string))
        .flatten()
}

/// The rows copy mode may move through, and the height of the view.
fn copy_bounds(state: &State) -> crate::copy::Bounds {
    let total = state.live_behind();
    let screen = state.snapshot.as_ref().map_or(1, |s| s.rows.max(1)) as u64;
    crate::copy::Bounds {
        // A full screen program's screen is all there is of it: the history under it
        // belongs to the shell it was started from.
        first: if state.pane_is_full_screen() {
            total
        } else {
            0
        },
        last: total + screen - 1,
        page: focused_rect(state).rows.max(1),
    }
}

/// Open copy mode over the focused terminal, at its cursor, or at the foot of the view when
/// the view is already scrolled back.
fn enter_copy(state: &mut State) {
    let Some(snapshot) = state.snapshot.as_ref().filter(|_| state.focused != 0) else {
        state.message = Some(crate::keys::named(
            "nothing to copy from, ctrl-a c starts a terminal",
        ));
        return;
    };
    let total = state.live_behind();
    let bounds = copy_bounds(state);
    let (top, cursor) = match state.anchor {
        Some(top) => (top, (top + bounds.page as u64 - 1, 0)),
        None => (total, (total + snapshot.cursor.0 as u64, snapshot.cursor.1)),
    };
    let mut copy = crate::copy::Copy::open(state.focused, cursor, top, state.anchor);
    copy.follow(bounds);
    // The copy mode's own selection replaces the mouse's, and the focus list would cover
    // the text being copied.
    state.selection = None;
    close_focus(state);
    state.anchor = Some(copy.top);
    state.modes.copy = Some(copy);
}

/// Leave copy mode, and put the view back where it was when the mode opened.
fn leave_copy(state: &mut State) {
    let Some(copy) = state.modes.copy.take() else {
        return;
    };
    state.anchor = copy.restore;
    if let Some(top) = copy.restore {
        copy_fetch(state, top);
    }
}

/// Ask for the rows behind a view whose top row is `top`, when it reaches behind the screen.
fn copy_fetch(state: &mut State, top: u64) {
    let total = state.live_behind();
    if state.focused != 0 && top < total {
        let want = behind_request(
            state.focused,
            (total - top) as usize,
            focused_rect(state).rows,
        );
        state.modes.outbox.push(want);
    }
}

/// A pinned view (copy mode, or scrolled back) whose rows have gone behind the screen since
/// they were fetched: ask for them.
///
/// Rows the view was showing from the live screen become history as the terminal prints,
/// and nothing else asks for them, so they painted blank at the top of the view. Asked only
/// when the rows the view shows are not all held, and once per answer, so a terminal
/// streaming sixty frames a second costs one request per round trip rather than per frame.
fn refetch_pinned(state: &mut State) {
    let Some(top) = state.anchor else {
        return;
    };
    if state.focused == 0 || state.modes.refetching {
        return;
    }
    let total = state.live_behind();
    let page = focused_rect(state).rows as u64;
    let end = (top + page).min(total);
    if top >= end {
        return;
    }
    let held = state.behind.terminal == state.focused
        && state.behind.row(top).is_some()
        && state.behind.row(end - 1).is_some();
    if held {
        return;
    }
    state.modes.refetching = true;
    state.modes.outbox.push(behind_request(
        state.focused,
        (total - top) as usize,
        page as usize,
    ));
}

/// Decide what one read of stdin meant in copy mode.
///
/// The keys are tmux's vi table: `h j k l` and the arrows, `0 ^ $`, `w b e`, `ctrl-u
/// ctrl-d` half a page, `ctrl-b ctrl-f` and page up and down a page, `g G` the ends, `v` or
/// space and `V` select, `y` or enter copy and leave, `/ ?` search, `n N` again, `esc`
/// puts a selection down and then leaves, `q` leaves. The prefix leaves and is passed on,
/// so a prefix of `ctrl-b` wins over page up. The wheel moves the view; a click leaves and
/// then acts as a click.
fn copy_key(state: &mut State, bytes: &[u8], prefix: u8, request: &mut u64) -> ModeOutcome {
    let Some(mut copy) = state
        .modes
        .copy
        .take()
        .filter(|copy| copy.terminal == state.focused)
    else {
        return ModeOutcome::Passthrough(bytes.to_vec());
    };
    let top_before = copy.top;
    let bounds = copy_bounds(state);
    let half = (bounds.page / 2).max(1) as i64;
    let page = bounds.page.max(1) as i64;
    let mut at = 0;
    let mut outcome = ModeOutcome::Stay;
    let mut leave = false;
    while at < bytes.len() {
        let (key, used) = mode_key(bytes, at);
        let used = used.max(1);
        // The query line owns the keys while it is open.
        if let Some((query, backward)) = copy.query.as_mut() {
            match key {
                ModeKey::Byte(b'\r' | b'\n') => {
                    let (query, backward) = (query.clone(), *backward);
                    copy.query = None;
                    if !query.is_empty() {
                        copy_search(state, &mut copy, query, backward, None, request);
                    }
                }
                ModeKey::Esc => {
                    copy.query = None;
                    copy.status.clear();
                }
                ModeKey::Byte(0x7f | 0x08) => {
                    query.pop();
                }
                // ctrl-u, as in a shell: the whole line.
                ModeKey::Byte(0x15) => query.clear(),
                ModeKey::Partial => {
                    state.modes.pending = bytes[at..].to_vec();
                    break;
                }
                ModeKey::Byte(byte) if byte == prefix || byte == crate::keys::DETACH => {
                    leave = true;
                    outcome = ModeOutcome::Passthrough(bytes[at..].to_vec());
                }
                ModeKey::Byte(byte) if byte == b' ' || byte.is_ascii_graphic() => {
                    query.push(byte as char);
                }
                ModeKey::Text(text) => query.push_str(&text),
                _ => {}
            }
            if let Some((query, backward)) = &copy.query {
                copy.status = format!("{}{query}", if *backward { '?' } else { '/' });
            }
            if leave {
                break;
            }
            at += used;
            continue;
        }
        let row = |at: u64| copy_row(state, at).unwrap_or_default();
        match key {
            ModeKey::Byte(byte) if byte == prefix || byte == crate::keys::DETACH => {
                leave = true;
                outcome = ModeOutcome::Passthrough(bytes[at..].to_vec());
                break;
            }
            ModeKey::Press => {
                leave = true;
                outcome = ModeOutcome::Passthrough(bytes[at..].to_vec());
                break;
            }
            ModeKey::Wheel(up) => copy.scroll(
                if up {
                    -(WHEEL_ROWS as i64)
                } else {
                    WHEEL_ROWS as i64
                },
                bounds,
            ),
            ModeKey::Byte(b'k') | ModeKey::Up => copy.rows(-1, bounds),
            ModeKey::Byte(b'j') | ModeKey::Down => copy.rows(1, bounds),
            ModeKey::Byte(b'h') | ModeKey::Left => copy.sideways(false, &row(copy.cursor.0)),
            ModeKey::Byte(b'l') | ModeKey::Right => copy.sideways(true, &row(copy.cursor.0)),
            ModeKey::Byte(edge @ (b'0' | b'^' | b'$')) => copy.line_edge(edge, &row(copy.cursor.0)),
            ModeKey::Home => copy.line_edge(b'0', &row(copy.cursor.0)),
            ModeKey::End => copy.line_edge(b'$', &row(copy.cursor.0)),
            ModeKey::Partial => {
                state.modes.pending = bytes[at..].to_vec();
                break;
            }
            ModeKey::Byte(motion @ (b'w' | b'b' | b'e')) => copy.word(motion, bounds, &row),
            ModeKey::Byte(0x15) => copy.rows(-half, bounds),
            ModeKey::Byte(0x04) => copy.rows(half, bounds),
            ModeKey::Byte(0x02) | ModeKey::PageUp => copy.rows(-page, bounds),
            ModeKey::Byte(0x06) | ModeKey::PageDown => copy.rows(page, bounds),
            ModeKey::Byte(b'g') => {
                copy.cursor = (bounds.first, 0);
                copy.follow(bounds);
            }
            ModeKey::Byte(b'G') => {
                copy.cursor = (bounds.last, 0);
                copy.follow(bounds);
            }
            ModeKey::Byte(b'v' | b' ') => copy.toggle(crate::copy::Mode::Char),
            ModeKey::Byte(b'V') => copy.toggle(crate::copy::Mode::Line),
            // Copies and leaves; with nothing selected it only leaves, which is what tmux
            // hands expect of the same key.
            ModeKey::Byte(b'y' | b'\r' | b'\n') => {
                if !start_yank(state, &copy) {
                    state.message = Some("nothing was selected, nothing copied".to_string());
                }
                leave = true;
                break;
            }
            ModeKey::Byte(slash @ (b'/' | b'?')) => {
                copy.query = Some((String::new(), slash == b'?'));
                copy.status = (slash as char).to_string();
            }
            ModeKey::Byte(again @ (b'n' | b'N')) => match copy.last.clone() {
                Some((query, backward)) => {
                    let backward = backward != (again == b'N');
                    // Carry on from where the last step stopped, when it stopped for
                    // budget in this same direction; otherwise a new search from here.
                    let resume = copy
                        .search
                        .as_ref()
                        .filter(|s| !s.waiting && s.backward == backward && s.query == query)
                        .and_then(|s| s.resume);
                    copy_search(state, &mut copy, query, backward, resume, request);
                }
                None => copy.status = "no search yet: / looks down, ? looks up".to_string(),
            },
            ModeKey::Esc if copy.mode != crate::copy::Mode::None => {
                copy.mode = crate::copy::Mode::None;
            }
            ModeKey::Esc | ModeKey::Byte(b'q') => {
                leave = true;
                break;
            }
            _ => {}
        }
        at += used;
    }
    if leave {
        state.modes.copy = Some(copy);
        leave_copy(state);
        return outcome;
    }
    state.anchor = Some(copy.top);
    if copy.top != top_before {
        copy_fetch(state, copy.top);
    }
    state.modes.copy = Some(copy);
    outcome
}

/// Search for `query` from the cursor: the rows on the screen first, since the client has
/// them, then the history through the daemon, a bounded step at a time. `resume` carries on
/// a search whose last step ran out of budget.
fn copy_search(
    state: &mut State,
    copy: &mut crate::copy::Copy,
    query: String,
    backward: bool,
    resume: Option<u64>,
    request: &mut u64,
) {
    copy.last = Some((query.clone(), backward));
    let total = state.live_behind();
    let bounds = copy_bounds(state);
    let text = |at: u64| copy_row(state, at);
    let from = match resume {
        Some(resume) => Some(resume),
        None => {
            // The cursor's own row past the cursor, and when the cursor is on the screen the
            // screen rows the way the search goes; for a cursor in the history, only its
            // own row, since the rows past it are history too.
            let local = if copy.cursor.0 >= total {
                total..=bounds.last
            } else {
                copy.cursor.0..=copy.cursor.0
            };
            if let Some(hit) = crate::copy::find_near(&text, local, copy.cursor, &query, backward) {
                copy.cursor = hit;
                copy.follow(bounds);
                copy.search = None;
                copy.status = format!("{}{query}", if backward { '?' } else { '/' });
                return;
            }
            let next = if backward {
                copy.cursor.0.min(total).checked_sub(1)
            } else {
                Some(copy.cursor.0 + 1).filter(|at| *at < total)
            };
            next.filter(|_| bounds.first == 0)
        }
    };
    let Some(from) = from else {
        copy_missed(state, copy, &query, backward);
        return;
    };
    *request += 1;
    let id = format!("search-{request}");
    let started = match (resume, &copy.search) {
        (Some(_), Some(search)) => search.started,
        _ => from,
    };
    state.modes.outbox.push(ControlRequest::SearchScrollback {
        id: id.clone(),
        terminal: copy.terminal,
        query: query.clone(),
        from,
        backward,
    });
    copy.status = format!("searching the history for {query}");
    copy.search = Some(crate::copy::Search {
        id,
        query,
        backward,
        started,
        resume: None,
        waiting: true,
    });
}

/// Nothing found anywhere the search could look. A forward search from the history ends on
/// the screen, which the daemon does not hold: that part is looked at here.
fn copy_missed(state: &State, copy: &mut crate::copy::Copy, query: &str, backward: bool) {
    copy.search = None;
    if !backward {
        let total = state.live_behind();
        let bounds = copy_bounds(state);
        // From the end of the history: the screen's first row is looked at whole.
        let from = (total.max(copy.cursor.0 + 1).saturating_sub(1), usize::MAX);
        let hit = crate::copy::find_near(
            &|at| copy_row(state, at),
            total..=bounds.last,
            from,
            query,
            false,
        );
        if let Some(hit) = hit.filter(|hit| *hit > copy.cursor) {
            copy.cursor = hit;
            copy.follow(copy_bounds(state));
            copy.status = format!("/{query}");
            return;
        }
    }
    copy.status = format!("no match for {query}");
}

/// Fold one answer to a history search into the open copy mode.
fn copy_found(state: &mut State, id: &str, row: Option<u64>, cell: usize, resume: Option<u64>) {
    let Some(mut copy) = state.modes.copy.take() else {
        return;
    };
    let wanted = copy
        .search
        .as_ref()
        .is_some_and(|s| s.waiting && s.id == id);
    if wanted && let Some(search) = copy.search.clone() {
        let top = copy.top;
        match (row, resume) {
            (Some(row), _) => {
                copy.cursor = (row, cell);
                copy.follow(copy_bounds(state));
                copy.search = None;
                copy.status = format!(
                    "{}{}: {} rows back",
                    if search.backward { '?' } else { '/' },
                    search.query,
                    state.live_behind().saturating_sub(row)
                );
            }
            (None, Some(resume)) => {
                let searched = search.started.abs_diff(resume);
                copy.status = format!(
                    "no match in the {} {searched} rows, n searches further",
                    if search.backward { "last" } else { "next" }
                );
                copy.search = Some(crate::copy::Search {
                    resume: Some(resume),
                    waiting: false,
                    ..search
                });
            }
            (None, None) => copy_missed(state, &mut copy, &search.query, search.backward),
        }
        if copy.top != top {
            state.anchor = Some(copy.top);
            let top = copy.top;
            state.modes.copy = Some(copy);
            copy_fetch(state, top);
            return;
        }
    }
    state.modes.copy = Some(copy);
}

/// Start copying copy mode's selection. False when nothing is selected.
///
/// The rows on the screen and those already fetched are taken at once; the rest of the
/// history is asked for in chunks, and the copy happens when the last one lands.
fn start_yank(state: &mut State, copy: &crate::copy::Copy) -> bool {
    let Some((start, end)) = copy.selected() else {
        return false;
    };
    let total = state.live_behind();
    let mut yank = crate::copy::Yank {
        terminal: copy.terminal,
        start,
        end,
        rows: std::collections::BTreeMap::new(),
        total,
        asked: 0,
    };
    for at in start.0..=end.0 {
        if let Some(row) = copy_row(state, at) {
            yank.rows.insert(at, row);
        }
    }
    match yank.next_request() {
        Some(ask) => {
            state.message = Some(format!("copying {} rows", end.0 - start.0 + 1));
            state.modes.outbox.push(ask);
            state.modes.yank = Some(yank);
        }
        None => state.modes.clipboard = Some(yank.text()),
    }
    true
}

/// The pane's size in cells, which is what a sleeping tab's screen is cut to.
fn pane_cells(state: &State) -> (u16, u16) {
    screen::pane_size_for(
        state.cols,
        state.lines,
        state.collapsed,
        0,
        state.sidebar_width,
    )
}

/// Ask for the last screen of the sleeping tab the keyboard rests on, once per tab and
/// pane size, and let go of it when no sleeping tab is on screen any more, so a later
/// sleep asks afresh.
fn ask_peek(state: &mut State) {
    let Some(tab) = state.dim_tab() else {
        state.modes.peek = None;
        state.modes.peek_asked = None;
        return;
    };
    let (_, rows) = pane_cells(state);
    if state.modes.peek_asked == Some((tab, rows)) {
        return;
    }
    state.modes.peek_asked = Some((tab, rows));
    state.modes.outbox.push(ControlRequest::Scrollback {
        id: format!("peek-{tab}"),
        terminal: tab,
        up: rows as usize,
        rows: rows as usize,
    });
}

/// A sleeping tab's rows as a screen: plain text, every row dim, no cursor, so the pane
/// draws it like a live one and nobody mistakes it for something that is running.
fn peek_screen(mut lines: Vec<String>, cols: usize, rows: usize) -> SnapshotWire {
    if lines.is_empty() {
        lines.push("nothing of this tab's screen was kept".to_string());
    }
    lines.drain(..lines.len().saturating_sub(rows));
    let styles = lines
        .iter()
        .map(|line| {
            vec![qoro_runtime::daemon::SpanWire {
                at: 0,
                len: line.chars().count(),
                fg: None,
                bg: None,
                attrs: qoro_runtime::daemon::attrs::DIM,
                link: None,
            }]
        })
        .collect();
    SnapshotWire {
        cols,
        rows,
        cursor: (0, 0),
        lines,
        styles,
        links: Vec::new(),
        modes: qoro_runtime::daemon::ModesWire {
            cursor_visible: false,
            ..Default::default()
        },
        behind: 0,
    }
}

/// Send what the modes asked for, and copy a finished yank. Runs wherever the loop has a
/// write in hand.
async fn drain_modes(
    state: &mut State,
    control_seq: &mut u32,
    wr: &mut (impl tokio::io::AsyncWriteExt + Unpin),
    dirty: &mut bool,
) -> io::Result<()> {
    ask_peek(state);
    for ask in std::mem::take(&mut state.modes.outbox) {
        wr.write_all(&client::encode_control_seq(&ask, control_seq)?)
            .await?;
    }
    if let Some(text) = state.modes.clipboard.take() {
        let mut out = std::io::stdout();
        state.message = Some(match crate::selection::copy(&text, &mut out)? {
            Some(via) => copied(&text, via),
            None => "nothing to copy: the selection is blank".to_string(),
        });
        *dirty = true;
    }
    Ok(())
}

/// Size whatever changed since the last painted frame: tiles a divider moved, however many
/// nudges or drag motions the frame took in, and a terminal nothing has sized yet, such as
/// one a script just opened.
fn size_moved_tile(
    state: &mut State,
    seqs: &mut std::collections::BTreeMap<u32, u32>,
) -> io::Result<Option<BytesMut>> {
    size_terminals(state, seqs)
}

/// Where copy mode's cursor is drawn, one based on the screen, when it is in view.
fn copy_cursor(state: &State) -> Option<(usize, usize)> {
    let copy = state.modes.copy.as_ref()?;
    let rect = focused_rect(state);
    let row = copy_row(state, copy.cursor.0).unwrap_or_default();
    let (pane_row, column) = copy.cursor_on_screen(rect.rows, &row)?;
    let left = screen::left_for(state.cols, state.collapsed, state.sidebar_width);
    Some((
        screen::TABS + rect.y + pane_row + 1,
        left + rect.x + column.min(rect.cols.saturating_sub(1)) + 1,
    ))
}

/// What the interface drew where a person clicked.
#[derive(Clone, Copy, Debug)]
enum Clicked {
    Menu,
    /// The foot's `settings`: the settings page, in a tab.
    SettingsPage,
    /// A tab of the inbox, or a count on its folded row.
    InboxTab(sidebar::InboxTab),
    /// The inbox's caret.
    InboxFold,
    Fold(i64),
    View,
    Collapse,
    Space(i64),
    /// The rule's `sleep`, on the space that rule is for rather than the one on screen:
    /// a person clicking a row means that row.
    Sleep(i64),
    /// The rule's `wake`: every sleeping tab of that space.
    Wake(i64),
    /// The rule's `⋯`: that space's settings.
    Settings(i64),
    /// The rule's cup: keep that space awake, or let idle sleep take it again.
    KeepAwake(i64),
    /// The rule's toggle: open its drawer of controls, or put it back.
    Drawer(i64),
    /// The cup on an agent's row: the same for that agent's pane.
    KeepAwakeAgent(u32),
    /// The rule's close, which only arms the question.
    Close(i64),
    /// `y` on an armed rule: the space and everything in it goes.
    ConfirmClose(i64),
    /// The question is off. Nothing was sent.
    CancelClose,
    Terminal(u32),
    /// The close on the focused tab: the same as `ctrl-a x`, a question first when an agent
    /// in it is busy.
    CloseFocused,
    /// The `x` on an agent's row in the column: that agent's pane, a question first when
    /// it is busy.
    CloseAgent(u32),
    NewTerminal,
    /// A space's `+`: a terminal or an agent in that space.
    New(i64),
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
    run_stream(stream, leftover, notice, Some(last_focus_path(socket))).await
}

/// Where an interface on this socket keeps the terminal it showed last: beside the
/// daemon's socket. Shared by every interface on that daemon on purpose, the last focus
/// written wins: coming back means coming back to where the person last was, in whichever
/// window that was, and `focus` brings that terminal's space along with it.
pub fn last_focus_path(socket: &Path) -> std::path::PathBuf {
    socket.with_file_name("interface-focus")
}

/// The terminal a person left, so coming back lands there rather than on the first tab.
///
/// Read once at start and handed to the first index; written whenever the keyboard
/// moves to another local terminal. Best effort both ways: no file, a stale id or a
/// write that fails is today's first tab, never an error.
#[derive(Default)]
struct LastFocus {
    file: Option<std::path::PathBuf>,
    /// Read from the file, waiting for the first index to name it.
    resume: Option<u32>,
    /// What the file holds now, so an unchanged focus writes nothing.
    written: u32,
}

impl LastFocus {
    fn read(file: Option<std::path::PathBuf>) -> LastFocus {
        let resume = file
            .as_deref()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .and_then(|text| text.trim().parse::<u32>().ok())
            .filter(|t| *t != 0);
        LastFocus {
            file,
            resume,
            written: resume.unwrap_or(0),
        }
    }

    /// Keep `focused` when it is a local terminal the file does not already hold. A
    /// remote terminal's number is this interface's, not the daemon's, and means
    /// nothing to the next one.
    fn keep(&mut self, focused: u32) {
        if focused == 0 || focused == self.written || crate::mux::is_remote_terminal(focused) {
            return;
        }
        self.written = focused;
        if let Some(file) = &self.file {
            // Written aside and renamed over, so another interface reading it never sees
            // half a number.
            let temp = file.with_extension(format!("{}", std::process::id()));
            if std::fs::write(&temp, format!("{focused}\n")).is_ok() {
                let _ = std::fs::rename(&temp, file);
            }
        }
    }
}

/// The interface over a connection that has already done its handshake: a local socket,
/// an `ssh` running `qoro relay`, or the mux that puts several daemons behind one stream.
pub async fn run_stream<S>(
    stream: S,
    leftover: BytesMut,
    notice: Option<String>,
    last_focus: Option<std::path::PathBuf>,
) -> io::Result<()>
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

    let outcome = drive(stream, leftover, notice, last_focus).await;

    drop(guard);
    outcome
}

async fn drive<S>(
    stream: S,
    leftover: BytesMut,
    notice: Option<String>,
    last_focus: Option<std::path::PathBuf>,
) -> io::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (mut rd, mut wr) = tokio::io::split(stream);
    let (cols, lines) = client::outer_size();
    let mut state = State {
        armed: None,
        drawer: None,
        list_scroll: None,
        closing: None,
        close_begun: false,
        rows: Vec::new(),
        focused: 0,
        snapshot: None,
        snapshots: std::collections::BTreeMap::new(),
        last_seen: std::collections::BTreeMap::new(),
        previewing: false,
        images: std::collections::BTreeMap::new(),
        // Read once: what this client's outer terminal can draw decides real
        // picture versus placeholder for the life of the session.
        graphics: crate::graphics::detect(),
        shown_images: std::collections::BTreeMap::new(),
        img_size: (0, 0),
        subs: std::collections::BTreeSet::new(),
        sized: std::collections::BTreeMap::new(),
        layouts: std::collections::BTreeMap::new(),
        parked: std::collections::BTreeMap::new(),
        split_writes: Vec::new(),
        split_saving: 0,
        resync: false,
        split_origin: std::collections::BTreeMap::new(),
        config_stamp: config_stamp(),
        pending_agents: std::collections::BTreeMap::new(),
        session_titles: std::collections::BTreeMap::new(),
        agent_typing: Vec::new(),
        cols,
        lines,
        // What the caller wanted said, or that this is connecting. A notice about a
        // missing hook is worth more than the word "connecting", which is gone by the time
        // anybody reads it.
        message: Some(CONNECTING.to_string()),
        hint: notice,
        pending_focus: None,
        last: LastFocus::read(last_focus),
        show_when_listed: None,
        elsewhere: None,
        waking: None,
        dim: 0,
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
        inbox: sidebar::Inbox::default(),
        inbox_drag: None,

        shut: sidebar::Shut::new(),

        in_column: false,

        prefix_pending: false,

        answer: None,

        usage: sidebar::Usage::new(),

        usage_seen: std::collections::BTreeMap::new(),
        seen_done: sidebar::Seen::new(),
        unseen_finished: sidebar::Unseen::new(),
        tick: 0,
        // Read now, not zero: the first paint lands on the first loop turn,
        // and a click before it must still test a real clock.
        paint_now: sidebar::now_unix(),
        // The default, held to half the window the way a resize holds it, so a window
        // under twice the default never opens with the column past the half-window rule.
        sidebar_width: sidebar::opening_width(cols),
        sidebar_drag: None,
        row_press: None,
        drop_after_move: None,
        sidebar_sized: sidebar::opening_width(cols),
        palette: None,
        palette_pending: Vec::new(),
        keys_query: None,
        menu_kind: MenuKind::Main,
        connecting: None,
        probing: None,
        installing: None,
        reprobe: None,
        joining: None,
        modes: Modes::default(),
        selection: None,
        forwarded_press: false,
        anchor: None,
        behind: Behind::default(),
        outer_focus: None,
        host_colors: [None, None],
        host_light: None,
        icon_reply: crate::icons::Reply::Silent,
        icon_font: false,
        host_sent: None,
        host_refused: false,
        news_sent: None,
        news_refused: false,
        notes: Vec::new(),
        notes_due: None,
        route: crate::notify::Route::Nowhere(crate::notify::Why::NoProgram),
        nowhere: false,
        config: crate::config::Config::default(),
    };
    // Where a desktop note would go, decided once: the environment does not change under a
    // running interface, and inside tmux it costs asking tmux.
    state.route = crate::notify::route(&crate::notify::Env::read());
    state.nowhere = matches!(state.route, crate::notify::Route::Nowhere(_));
    // The file is read before the first frame, so the first frame already names the
    // prefix the person chose. What in it could not be used is said once, on the hint row,
    // beside whatever the caller wanted said.
    let read = crate::config::read();
    use_config(&mut state, &read);
    if let Some(said) = read.hint() {
        state.hint = Some(match state.hint.take() {
            Some(notice) => format!("{notice}; {said}"),
            None => said,
        });
    }

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
    let mut keys = Keys::with_prefix(state.config.prefix);
    let mut connection = Connection::new();
    let mut inbuf = leftover;
    inbuf.reserve(256 * 1024);
    let mut read = [0u8; 64 * 1024];

    let (tx, mut stdin_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    // Weak, so it never keeps the channel open after the reader has gone: it only wakes the
    // stdin arm when a held escape runs out of time and is owed to the person as a key.
    let wake_stdin = tx.downgrade();
    client::spawn_stdin_reader(tx);
    // What the terminal says about itself comes out of the keyboard stream before anything
    // reads a key from it. See `input`.
    let mut reports = crate::input::Reports::default();
    let mut sigwinch =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    // Raw mode means ctrl-c is a byte, not a signal, so this only fires for a `kill -INT`
    // from outside, which otherwise ended the process without restoring the terminal.
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
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

    // No resize here. Terminal 1 by number claimed a terminal that may not be on screen
    // (ADR-007), and its size then matched the adoption resize that follows, which the
    // daemon answers with a one-row repaint nudge: a child reading its size in those
    // 30 ms saw a row too few. The adoption path sizes whichever terminal is focused.

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
    // The colour level, decided once before anything is painted: every painter below
    // reads it, so the first frame already honours NO_COLOR and dumb terminals.
    crate::colorlevel::apply();
    // `1006` asks for those reports in the extended form, because the original one cannot
    // express a column past 223 and this interface is regularly wider than that.
    out.write_all(b"\x1b[?1049h")?;
    // Mouse press, release and drag where the terminal reports them usefully.
    // Everywhere else clicks stay inert and the keyboard runs everything.
    if mouse_on() {
        out.write_all(b"\x1b[?1002h\x1b[?1006h")?;
    }
    // Focus reports (`CSI ? 1004 h`), so the interface knows when the person looked away,
    // and the questions about the terminal's own colours. See `input`.
    out.write_all(b"\x1b[?1004h")?;
    if may_ask_terminal() {
        let tmux = std::env::var_os("TMUX").is_some();
        out.write_all(&crate::colorlevel::tmux_wrapped(ASK_COLORS, tmux))?;
        out.write_all(&crate::colorlevel::tmux_wrapped(ASK_SCHEME, tmux))?;
        // Its name, once and never through tmux, which would swallow or forward it; icons
        // are off under tmux anyway. See `crate::icons`.
        if !tmux {
            out.write_all(ASK_VERSION)?;
            state.icon_reply = crate::icons::Reply::Waiting;
        }
    }
    // The font first, so a terminal that draws from installed fonts finds it on the first
    // frame, then the first guess, which the terminal's answer may still correct.
    state.icon_font = state.config.icons != crate::icons::Setting::Off
        && crate::icons::install(&crate::colorlevel::current_env());
    reicon(&state);
    out.flush()?;
    // The pet and the update notice, read once so the first frame has them; the
    // acknowledgement tick below re-reads them, with no wakeup of their own.
    sidebar::refresh_files();
    let mut dirty = true;
    let mut presenter = present::Presenter::default();
    let mut written = String::new();
    let mut presented = tokio::time::Instant::now();
    // At most one index request in flight. Announces that land while one is out ride on its
    // answer, which asks once more: a burst of tool calls, each one news for the sidebar,
    // costs one round trip at a time rather than one request per announce, and the last
    // change is never lost. A request unanswered for [`INDEX_PATIENCE`] is taken as lost.
    let mut index_asked: Option<tokio::time::Instant> = None;
    // When the close in progress started, so the loop can give up on it rather than
    // leaving a person watching a row that never changes.
    let mut closing_since: Option<tokio::time::Instant> = None;
    // The id of that request, so only its own answer clears it: behind the mux one request
    // can be answered once per machine, and a late answer must not open the gate early.
    let mut index_id = String::new();
    let mut index_owed = false;
    // The column moves only while something works and motion is on: `QORO_MOTION=0`
    // keeps it still. The next frame's deadline, when one is armed.
    let motion = motion_on();
    let mut motion_at: Option<tokio::time::Instant> = None;
    // Costs are sampled only when asked for, the first time straight away.
    let mut usage_at = usage_on().then(tokio::time::Instant::now);
    // Work that blocks (reading /proc, reading a transcript) runs on the blocking pool and
    // comes back here, so the keyboard never waits on a disk. One sample at a time.
    let (blocking_tx, mut blocking_rx) = tokio::sync::mpsc::unbounded_channel::<Blocked>();
    let mut sampling = false;
    // The terminal whose last answer is being read, so a result for any other is dropped.
    let mut answer_for: Option<u32> = None;
    presented = presented.checked_sub(FRAME).unwrap_or(presented);

    let outcome: io::Result<()> = async {
        loop {
            // Splits a person made or took apart since the last turn, recorded so a
            // restart brings the tab back whole. Queued wherever the change was made,
            // sent here, where there is always a write in hand.
            for tree in std::mem::take(&mut state.split_writes) {
                request += 1;
                state.split_saving += 1;
                wr.write_all(&client::encode_control_seq(
                    &ControlRequest::SetSplit {
                        id: format!("tree-{request}"),
                        tree,
                    },
                    &mut control_seq,
                )?)
                .await?;
            }
            // Agent lines owed to new tabs. Queued on the `TerminalCreated`
            // answer below and typed here, where there is always a write in
            // hand, in small messages like a paste: one huge message inflates
            // on the wire and trips the reassembly bound.
            for (terminal, line) in std::mem::take(&mut state.agent_typing) {
                for piece in client::split_input(&line) {
                    wr.write_all(&client::encode_command_to(
                        terminal,
                        // Typed, never spawned: the person's own hands by way
                        // of the palette, no wait edge, like any other typing.
                        &ClientWire::Input {
                            bytes: piece.to_vec(),
                            from_terminal: None,
                            from_token: None,
                        },
                        seqs.entry(terminal).or_default(),
                    )?)
                    .await?;
                }
            }
            // A tree came on screen where nothing could write (the index, a bootstrap):
            // its other tiles still need their subscriptions. Only those: the focused
            // tile's size goes out with the caller's own resize or `adopted_resize`,
            // and a second resize of the same size is a nudge the child can see.
            if std::mem::take(&mut state.resync)
                && let Some(sync) = sync_subs(&mut state, &mut control_seq)?
            {
                wr.write_all(&sync).await?;
                dirty = true;
            }
            if let Some(tell) = news_request(&mut state, &mut request) {
                wr.write_all(&client::encode_control_seq(&tell, &mut control_seq)?)
                    .await?;
            }
            // At most one frame per `FRAME`. The first render after a quiet spell goes out
            // at once, so a keystroke echoes with no added wait; renders arriving closer
            // together than that wait for the timer arm below and go out as one frame.
            // `ctrl-a` waiting for its letter produces no action of its own, so the key row
            // learns it here: a flip either way repaints the row.
            if keys.prefix_pending() != state.prefix_pending {
                state.prefix_pending = keys.prefix_pending();
                dirty = true;
            }
            state.last.keep(state.focused);
            if dirty && presented.elapsed() >= FRAME {
                // An open answer lays out for the pane before the frame borrows the state:
                // once per width, so a long answer is not wrapped again every frame.
                if let Some(answer) = state.answer.as_mut() {
                    let (cols, rows) = screen::pane_size_for(
                        state.cols,
                        state.lines,
                        state.collapsed,
                        0,
                        state.sidebar_width,
                    );
                    answer.layout(cols as usize, (rows as usize).saturating_sub(1));
                }
                // One frame for both paints: a single clock for tiers, order
                // and the hit testing below, and its time remembered for the
                // clicks that land between paints.
                let mut frame = state.frame();
                // The carried row on the rows of this very frame, its clock and all.
                let drag = state.drag_paint(frame.now_unix);
                frame.chrome.drag = drag.as_ref();
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
                // Where a real picture lands: the focused pane's first row, in
                // screen cells, one based.
                let tile_place_at = screen::picture_cell(&frame);
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
                // An open answer covers the pane the same way, over the focus list too.
                if let Some(answer) = state.answer.as_ref()
                    && state.palette.is_none()
                    && state.reading.is_none()
                    && state.prompt.is_none()
                    && state.menu.is_none()
                {
                    screen::paint_answer(&frame, answer, &mut painted);
                }
                // The borrow ends here: the clock is copied out before the
                // state is touched again below.
                let paint_now = frame.now_unix;
                drop(frame);
                state.paint_now = paint_now;
                // Copy mode's cursor, shown where it is: placed last, so it is where the
                // frame leaves the terminal's own cursor.
                if let Some((row, col)) = copy_cursor(&state) {
                    painted.push_str(&screen::at(row, col));
                    painted.push_str("\x1b[?25h");
                }
                written.clear();
                presenter.present(&painted, state.cols, state.lines, &mut written);
                // Pictures, reconciled per frame. Anything placed on the outer
                // screen and no longer desired comes down; the current picture
                // goes up at the focused pane's first row, drawing down. The
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
                    || state.answer.is_some()
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
                    if let Some(resize) = size_terminals(&mut state, &mut seqs)? {
                        wr.write_all(&resize).await?;
                    }
                    state.sidebar_sized = state.sidebar_width;
                }
                // A moved divider sizes the focused tile the same way: once per painted
                // frame, however many nudges or drag motions the frame took in.
                if let Some(resize) = size_moved_tile(&mut state, &mut seqs)? {
                    wr.write_all(&resize).await?;
                }
            }

            // The panel's next tier boundary, if any idle row has one coming.
            // One shot and not an interval: the arm below sleeps until it and
            // then goes back to never. Folded away, the strip shows no tiers,
            // so nothing is armed. See `sidebar::wants_tick`.
            // A working agent on a drawn column arms the next animation frame; nothing
            // working disarms it, and the column stops on whatever frame it was on.
            let moving = motion
                && screen::left_for(state.cols, state.collapsed, state.sidebar_width) > 0
                && (sidebar::moves(&state.column(0, 0)) || state.drawer_moving());
            if !moving {
                motion_at = None;
            } else if motion_at.is_none() {
                motion_at = Some(tokio::time::Instant::now() + MOTION_TICK);
            }
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

            // A held `ESC [` or `ESC ]` waiting to see whether it is a report. `None` on a
            // quiet screen, like the tier boundary, so nothing is armed.
            let hold_at = reports.deadline().map(tokio::time::Instant::from_std);
            // Notes waiting out their second. `None` with nothing held, so nothing is armed.
            let notes_at = state.notes_due.map(tokio::time::Instant::from_std);

            tokio::select! {
                biased;
                _ = ack_tick.tick() => {
                    if let Some(ack) = connection.renders().due_ack(std::time::Instant::now()) {
                        wr.write_all(&ack).await?;
                    }
                    drain_pending_focus(&mut state, &mut control_seq, &mut seqs, &mut wr, &mut dirty).await?;
                    if sidebar::refresh_files() {
                        dirty = true;
                    }
                    if config_moved(&mut state) {
                        let read = crate::config::read();
                        use_config(&mut state, &read);
                        // What in the changed file could not be used is said, as at the start.
                        if let Some(said) = read.hint() {
                            state.hint = Some(said);
                        }
                        dirty = true;
                    }
                }
                _ = sigterm.recv() => return Ok(()),
                _ = sighup.recv() => return Ok(()),
                _ = sigint.recv() => return Ok(()),
                _ = sigwinch.recv() => {
                    let (cols, lines) = client::outer_size();
                    state.cols = cols;
                    state.lines = lines;
                    // Every row moved under a held drag; it ends rather than land on
                    // whatever is under the pointer now.
                    drop_carry(&mut state);
                    // A column pulled to half a wide window stays at most half of a
                    // narrower one, so the pane and the edge to drag never vanish.
                    state.sidebar_width = state
                        .sidebar_width
                        .min(sidebar::sidebar_max(cols));
                    // The window moved: refresh what is shown before
                    // sizing, or the child reflows against the width the pane no longer
                    // has.
                    // Every terminal to its own tile, or to the whole pane for a tab not on
                    // screen, the way it would be drawn.
                    if let Some(resize) = size_terminals(&mut state, &mut seqs)? {
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
                    let Some(raw) = typed else { return Ok(()) };
                    // Reports first, before any overlay or the key decoder sees the bytes:
                    // each of those reads an escape as a cancel.
                    let (bytes, said) = reports.strip(&raw);
                    let reported = !said.is_empty();
                    let mut window_focus = None;
                    let mut ask_again = false;
                    for report in said {
                        if let crate::input::Report::Focus(focused) = report {
                            window_focus = Some(focused);
                        }
                        ask_again |= note_report(&mut state, report);
                        dirty = true;
                    }
                    if reported {
                        retheme(&state);
                    }
                    // The window lost the mouse: the release a held drag waits for may
                    // never come.
                    if window_focus == Some(false) {
                        drop_carry(&mut state);
                    }
                    if ask_again && may_ask_terminal() {
                        // Both questions again, not colours alone: a scheme capable
                        // terminal that switched while away converges on refocus.
                        let tmux = std::env::var_os("TMUX").is_some();
                        out.write_all(&crate::colorlevel::tmux_wrapped(ASK_COLORS, tmux))?;
                        out.write_all(&crate::colorlevel::tmux_wrapped(ASK_SCHEME, tmux))?;
                        out.flush()?;
                    }
                    if let Some(tell) = host_colors_request(&mut state, &mut request) {
                        wr.write_all(&client::encode_control_seq(&tell, &mut control_seq)?)
                            .await?;
                    }
                    // The window's focus, passed on to the pane that has the keyboard when its
                    // program asked for it, the way tmux does: vim reloads a file changed
                    // while the person was away on exactly this.
                    if let Some(focused) = window_focus
                        && wants_focus_events(state.snapshot.as_ref())
                    {
                        let report = if focused { b"\x1b[I" } else { b"\x1b[O" };
                        send_to_pane(&mut state, &mut seqs, &mut wr, report.to_vec()).await?;
                    }
                    if bytes.is_empty() {
                        continue;
                    }
                    // The person is doing something: a machine they connected a moment
                    // ago no longer gets to move the screen when it arrives, and a list
                    // of its sessions no longer gets to open under their next key. A
                    // report the terminal sent about itself is not the person, so it is
                    // stripped above and never reaches this.
                    state.joining = None;
                    state.probing = None;
                    state.installing = None;
                    // Ahead of the keystroke itself: a resize behind the input
                    // loses the race with the shell reading it.
                    drain_adopt_resize(&mut state, &mut control_seq, &mut seqs, &mut wr, &mut dirty).await?;
                    // Esc while a row is carried puts it back, and goes no further.
                    if bytes == b"\x1b" && state.row_press.is_some_and(|p| p.moved && !p.dead) {
                        drop_carry(&mut state);
                        dirty = true;
                        continue;
                    }
                    // The palette owns the keyboard while it is open: typing filters,
                    // enter goes, esc is back. A prefix or a detach inside it closes
                    // the palette and the bytes from there on are ordinary input, so
                    // `ctrl-a d` still detaches.
                    let mut feed: Option<Vec<u8>> = None;
                    // An overlay owns the mouse while it is open: a row held before it
                    // opened, whose release never came, must not drop on its answer.
                    if state.overlay_open() {
                        drop_carry(&mut state);
                    }
                    if state.palette.is_some() {
                        match palette_key(&mut state, &bytes) {
                            PaletteOutcome::Updated | PaletteOutcome::Closed => {
                                dirty = true;
                            }
                            PaletteOutcome::Confirm(item) => {
                                match item {
                                    // A jump to a sleeping tab shows it without waking it.
                                    palette::Item::Terminal(picked)
                                        if let Some(space) = state.sleeping_space_of(picked) =>
                                    {
                                        if let Some(out) = peek(
                                            &mut state, space, picked, &mut control_seq, &mut seqs,
                                        )? {
                                            wr.write_all(&out).await?;
                                        }
                                    }
                                    palette::Item::Sleep => {
                                        sleep_toggle(&mut state, &mut request);
                                    }
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
                                        state.prompt = Some(Prompt::new(Naming::Terminal, ""));
                                    }
                                    palette::Item::NewAgentAsk => {
                                        state.prompt =
                                            Some(Prompt::new(Naming::Agent(state.space), ""));
                                    }
                                    palette::Item::NewAgent(program) => {
                                        state.prompt = Some(agent_name(state.space, program));
                                    }
                                    palette::Item::NewSpace => {
                                        state.prompt = Some(Prompt::new(Naming::Space, ""));
                                    }
                                    palette::Item::CopyMode => enter_copy(&mut state),
                                    palette::Item::Resize => enter_resize(&mut state),
                                    palette::Item::Equalize => equalize(&mut state),
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
                        if let Some(hit) = menu_click(&bytes, state.cols, state.lines, &state.menu_kind) {
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
                        dirty = true;
                        // The keys can be searched: `/` starts it, and while it runs the
                        // bytes are the search's. What it hands back (a prefix and
                        // after) goes through the decoder like any key here.
                        let rest = if state.keys_query.is_some() {
                            keys_search(&mut state, &bytes)
                        } else if state.reading == Some(Reading::Keybinds)
                            && bytes == b"/"
                            && !keys.prefix_pending()
                        {
                            state.keys_query = Some(String::new());
                            None
                        } else {
                            Some(bytes)
                        };
                        // A key or a click closes a thing that is only being read.
                        // Through the one decoder, so half a report carried across
                        // reads is still half a report, not two keys.
                        if let Some(rest) = rest
                            && closes_reading(&keys.feed(&rest))
                        {
                            state.reading = None;
                            state.keys_query = None;
                        }
                    } else if state.prompt.is_some() {
                        // The naming box owns the keyboard the same way the palette does,
                        // and hands a prefix or a detach back for the same reason.
                        match prompt_key(&mut state, &bytes) {
                            PromptOutcome::Typing | PromptOutcome::Closed => dirty = true,
                            PromptOutcome::Confirm(prompt, name, rest) => {
                                // A box that opened another (a command, then the agent's
                                // name) owns what was typed after the enter.
                                // ponytail: those keys are dropped, as the palette drops
                                // its own; feed them to the new box if a paste ever needs it.
                                let asks = answer_prompt(&mut state, *prompt, name, &mut request);
                                if !rest.is_empty() && state.prompt.is_none() {
                                    feed = Some(rest);
                                }
                                for ask in asks {
                                    wr.write_all(&client::encode_control_seq(
                                        &ask, &mut control_seq,
                                    )?).await?;
                                }
                                dirty = true;
                            }
                            // The box closed, so the screen changed even when nothing
                            // follows: an enter that kept the name has no keys behind it.
                            PromptOutcome::Passthrough(rest) => {
                                dirty = true;
                                feed = Some(rest);
                            }
                        }
                    } else if state.armed.is_some()
                        && !is_mouse(&bytes)
                        && !keys.mouse_pending()
                        && !is_report_head(&bytes)
                    {
                        // An armed rule owns the keyboard while it is asking, without being
                        // a mode: its question is on a row rather than an overlay, but the
                        // bytes belong to the question the same way.
                        //
                        // Not the mouse. A click is how the question is answered, and this
                        // arm is reached before the mouse one, so taking the pointer here
                        // would leave a rule asking a question nothing could ever answer.
                        //
                        // And not half a report. One can arrive in two reads, and the
                        // decoder is holding the rest of it, so the half falls through to
                        // it and the whole report reaches the mouse arm when the tail comes.
                        // Reading the half as an answer would close a space the person was
                        // in the middle of asking about.
                        let outcome = mode_input(&mut state, &bytes, keys.prefix(), &mut request);
                        // The give-up timer is armed where the close is created, and a key is
                        // as much a place as a click. Armed here rather than on the next
                        // answer, because a daemon that takes the request and says nothing
                        // is exactly the case the timer exists for, and it never would.
                        if state.close_begun {
                            state.close_begun = false;
                            closing_since = Some(tokio::time::Instant::now());
                        }
                        dirty = true;
                        if let ModeOutcome::Passthrough(rest) = outcome {
                            feed = Some(rest);
                        }
                    } else if state.modes.open() {
                        // A mode owns the keyboard the way the overlays do, and hands the
                        // rest back when it is left.
                        let outcome = mode_input(&mut state, &bytes, keys.prefix(), &mut request);
                        dirty = true;
                        if let ModeOutcome::Passthrough(rest) = outcome {
                            feed = Some(rest);
                        }
                    } else if state.answer.is_some() {
                        // An open answer owns the keyboard: it scrolls and closes, and a stray
                        // key does nothing rather than type into the agent behind it.
                        let (cols, rows) = screen::pane_size_for(
                            state.cols,
                            state.lines,
                            state.collapsed,
                            0,
                            state.sidebar_width,
                        );
                        let body = (rows as usize).saturating_sub(1).max(1);
                        if let Some(answer) = state.answer.as_mut() {
                            match answer.key(&bytes, body) {
                                crate::answer::Key::Closed => state.answer = None,
                                crate::answer::Key::Scrolled => {
                                    answer.layout(cols as usize, body);
                                }
                                crate::answer::Key::Ignored => {}
                                // The prefix, a detach or a click: the page closes and the
                                // bytes run the ordinary path below.
                                crate::answer::Key::Pass => {
                                    state.answer = None;
                                    feed = Some(bytes.clone());
                                }
                            }
                        }
                        dirty = true;
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
                    // The file was read again and the prefix changed: the
                    // decoder starts over with the new one.
                    if keys.prefix() != state.config.prefix {
                        keys = Keys::with_prefix(state.config.prefix);
                    }
                    for action in keys.feed(&input) {
                        // The palette only opens from this loop, so open here means this
                        // read opened it: what came after `ctrl-a space` in the same read
                        // is neither the pane's (a fast tab would complete in the shell)
                        // nor a command. Dropped.
                        // ponytail: a batched read loses those keys; replaying them needs
                        // the palette's outcome handling run from here.
                        if state.palette.is_some() {
                            continue;
                        }
                        match action {
                            Action::Input(bytes) => {
                                // Typed in the same read as the key that opened a mode,
                                // as in `ctrl-a [ k`: the mode's, like everything after it.
                                let bytes = if state.modes.open() {
                                    dirty = true;
                                    match mode_input(&mut state, &bytes, keys.prefix(), &mut request) {
                                        ModeOutcome::Stay => continue,
                                        ModeOutcome::Passthrough(rest) => rest,
                                    }
                                } else {
                                    bytes
                                };
                                // The hint has been read or it has not; either way the
                                // person is working now.
                                state.hint = None;
                                // A key for the pane moves the keyboard's attention back there.
                                if std::mem::take(&mut state.in_column) {
                                    dirty = true;
                                }
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
                                // Enter on a sleeping tab wakes it, the one key that means
                                // "go" when there is nothing to type into.
                                if focused == 0
                                    && bytes.contains(&b'\r')
                                    && let Some(to) = state.dim_tab()
                                    && let Some(space) = state.space
                                {
                                    wake(&mut state, space, to, &mut request);
                                    dirty = true;
                                }
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
                                // Where the press landed is where the key row points next.
                                let in_column = {
                                    let left = screen::left_for(
                                        state.cols,
                                        state.collapsed,
                                        state.sidebar_width,
                                    );
                                    left > 0
                                        && (col as usize) <= left
                                        && (row as usize)
                                            <= (state.lines as usize)
                                                .saturating_sub(screen::STATUS)
                                };
                                if state.in_column != in_column {
                                    state.in_column = in_column;
                                    dirty = true;
                                }
                                // A new press ends whatever gesture came before: a
                                // release swallowed elsewhere (the palette, a divider
                                // drag) must not leave the program owed one.
                                state.forwarded_press = false;
                                drop_carry(&mut state);
                                state.row_press = None;
                                state.inbox_drag = None;
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
                                // A click on the divider, on a tile or inside a running pane
                                // answers that click and returns before the space arm, and a
                                // re-focus moves the column out from under the rule that was
                                // asking. The question comes down here, where every click
                                // passes, rather than in a branch that some clicks never
                                // reach; the three clicks that are about it set it again.
                                let clicked = state.clicked(row, col);
                                let asked_about = put_question_down(&mut state, clicked);

                                // A pane's name on the top edge of the panes is its handle: held,
                                // it carries the pane like a row of the column (to the tab bar
                                // for a tab of its own); let go where it went down, it is a click
                                // on that pane.
                                if !shift && let Some(terminal) = state.title_at_screen(row, col) {
                                    if state.selection.take().is_some() {
                                        dirty = true;
                                    }
                                    state.row_press = Some(RowPress {
                                        source: Source::Pane(terminal),
                                        at: (row, col),
                                        now: (row, col),
                                        name: false,
                                        moved: false,
                                        dead: false,
                                    });
                                    continue;
                                }
                                if !shift && (state.divider_press(col, row) || state.split_press(col, row)) {
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
                                        // The press is the program's, so a selection kept
                                        // from an earlier copy goes down: left up, the drag
                                        // after this press extended it instead of reaching
                                        // the program, and the release copied again.
                                        if state.selection.take().is_some() {
                                            dirty = true;
                                        }
                                        if let Some(bytes) =
                                            mouse_report(0, tile_row, tile_col, true)
                                        {
                                            send_to_pane(&mut state, &mut seqs, &mut wr, bytes)
                                                .await?;
                                            state.forwarded_press = true;
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
                                // A click takes the question down whatever it lands on, so a
                                // rule cannot sit on screen asking while the keyboard is dead
                                // and its row is scrolled out of view. The two arms that
                                // mean to ask set the flag again, because asking is what they
                                // are for. Taken here rather than in a catch-all arm, because
                                // a catch-all can only run after every real arm has declined
                                // to match and would never be reached.
                                // Asked before the flag is taken: `clicked` reads the
                                // armed row's own columns through `act`, and taking the
                                // flag first would read `y` as the `x` under it and ask again.
                                match clicked {
                                    // An agent's row in the column waits for the release:
                                    // let go where it went down it is a click, moved
                                    // first it is a drag to another space or a pane.
                                    Some(Clicked::Terminal(picked)) if in_column => {
                                        // The list stays where the wheel left it when the
                                        // click moves the keyboard to the row it landed on.
                                        if let Some((_, at)) = state.list_scroll {
                                            state.list_scroll = Some((picked, at));
                                        }
                                        let name = matches!(
                                            state.column_hit(row),
                                            Some((sidebar::Hit::Terminal(_), true))
                                        );
                                        state.row_press = Some(RowPress {
                                            source: Source::Agent(picked),
                                            at: (row, col),
                                            now: (row, col),
                                            name,
                                            moved: false,
                                            dead: false,
                                        });
                                    }
                                    // A sleeping tab: a click wakes that tab alone and
                                    // follows it. The palette and ctrl-a n/p only peek.
                                    Some(Clicked::Terminal(picked))
                                        if let Some(space) = state.sleeping_space_of(picked) =>
                                    {
                                        wake(&mut state, space, picked, &mut request);
                                        dirty = true;
                                    }
                                    Some(Clicked::Terminal(picked)) => {
                                        if let Some(bytes) = focus(
                                            &mut state, picked, &mut control_seq, &mut seqs,
                                        )? {
                                            wr.write_all(&bytes).await?;
                                            dirty = true;
                                        }
                                    }
                                    Some(Clicked::CloseFocused) => {
                                        close_focused(&mut state, &mut request);
                                        dirty = true;
                                    }
                                    Some(Clicked::CloseAgent(terminal)) => {
                                        close_terminal(&mut state, terminal, &mut request);
                                        dirty = true;
                                    }
                                    Some(Clicked::Drawer(space)) => {
                                        state.toggle_drawer(space);
                                        if !motion {
                                            state.settle_drawer();
                                        }
                                        dirty = true;
                                    }
                                    Some(Clicked::KeepAwake(space)) => {
                                        keep_awake(&mut state, space, None, &mut request);
                                        dirty = true;
                                    }
                                    Some(Clicked::KeepAwakeAgent(terminal)) => {
                                        if let Some(space) = state
                                            .rows
                                            .iter()
                                            .find(|r| r.terminal == terminal)
                                            .map(|r| r.workspace)
                                        {
                                            keep_awake(&mut state, space, Some(terminal), &mut request);
                                        }
                                        dirty = true;
                                    }
                                    // The tab bar's `+` is the space on screen's `+`.
                                    Some(Clicked::NewTerminal) => {
                                        match state.space {
                                            // A new tab beside sleeping ones leaves them
                                            // asleep.
                                            Some(space) => open_new(&mut state, space),
                                            None => {
                                                state.prompt = Some(Prompt::new(Naming::Terminal, ""));
                                            }
                                        }
                                        dirty = true;
                                    }
                                    Some(Clicked::New(space)) => {
                                        open_new(&mut state, space);
                                        dirty = true;
                                    }
                                    // Two things a new space can be: one here, or the
                                    // spaces of another machine. `ctrl-a w` still goes
                                    // straight to the first.
                                    Some(Clicked::NewSpace) => {
                                        state.menu_kind = MenuKind::Plus;
                                        state.menu = Some(0);
                                        dirty = true;
                                    }
                                    // A space's rule in the tree waits for the release too: let go
                                    // where it went down it folds or goes there, moved first it
                                    // carries the space to another place in the list.
                                    Some(Clicked::Fold(space) | Clicked::Space(space))
                                        if in_column
                                            && !state.collapsed
                                            && state.view == sidebar::View::Tree =>
                                    {
                                        state.row_press = Some(RowPress {
                                            source: Source::Space {
                                                space,
                                                fold: matches!(clicked, Some(Clicked::Fold(_))),
                                            },
                                            at: (row, col),
                                            now: (row, col),
                                            name: false,
                                            moved: false,
                                            dead: false,
                                        });
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
                                        state.menu_kind = MenuKind::Main;
                                        state.menu = Some(0);
                                        dirty = true;
                                    }
                                    Some(Clicked::SettingsPage) => {
                                        // The menu's own settings row, so the two never differ.
                                        let item = MENU
                                            .iter()
                                            .position(|(name, _, _)| *name == "settings")
                                            .unwrap_or(0);
                                        if let MenuSend::Control(ask) =
                                            apply_menu_choice(&mut state, item, &mut request)
                                        {
                                            wr.write_all(&client::encode_control_seq(
                                                &ask,
                                                &mut control_seq,
                                            )?)
                                            .await?;
                                        }
                                        dirty = true;
                                    }
                                    Some(Clicked::Fold(space)) => {
                                        if !state.shut.remove(&space) {
                                            state.shut.insert(space);
                                        }
                                        dirty = true;
                                    }
                                    Some(Clicked::Sleep(space)) => {
                                        // Asking about another space while one is asking
                                        // here: the sleep goes through and the question on
                                        // the other row is gone, so it is said.
                                        if let Some(was) = asked_about.filter(|w| *w != space) {
                                            state.message = Some(format!(
                                                "kept {}",
                                                space_name(&state, was)
                                            ));
                                        }
                                        sleep_space(&mut state, space, &mut request);
                                        dirty = true;
                                    }
                                    Some(Clicked::Wake(space)) => {
                                        wake_space(&mut state, space, &mut request);
                                        dirty = true;
                                    }
                                    Some(Clicked::Settings(space)) => {
                                        state.prompt = Some(space_settings(&state, space));
                                        dirty = true;
                                    }
                                    // The close is a two step, and the first step is this
                                    // one: the rule repaints itself as a question and
                                    // nothing is asked of the daemon yet.
                                    Some(Clicked::Close(space)) => {
                                        arm_close(&mut state, space);
                                        dirty = true;
                                    }
                                    // `y`: the tabs go, one per answer, and the space with
                                    // them once the daemon's own count says there is
                                    // nothing left in it.
                                    Some(Clicked::ConfirmClose(space)) => {
                                        state.armed = None;
                                        if start_close(&mut state, space, &mut request) {
                                            closing_since = Some(tokio::time::Instant::now());
                                        }
                                        dirty = true;
                                    }
                                    // Anything else on the row, or anywhere else while a
                                    // rule is armed, takes the question down.
                                    Some(Clicked::CancelClose) => {
                                        if let Some(space) = asked_about {
                                            state.message = Some(format!(
                                                "kept {}",
                                                space_name(&state, space)
                                            ));
                                        }
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
                                            size_terminals(&mut state, &mut seqs)?
                                        {
                                            wr.write_all(&resize).await?;
                                        }
                                        dirty = true;
                                    }
                                    Some(Clicked::InboxTab(tab)) => {
                                        state.inbox.tab = state.inbox.picked(tab);
                                        state.inbox.scroll = 0;
                                        state.inbox.folded = false;
                                        dirty = true;
                                    }
                                    Some(Clicked::InboxFold) => {
                                        state.inbox.folded = !state.inbox.folded;
                                        dirty = true;
                                    }
                                    // The title away from its words is the inbox's edge:
                                    // held, it drags the inbox taller or shorter.
                                    None => {
                                        if in_column
                                            && !state.collapsed
                                            && !state.inbox.folded
                                            && state.inbox_room() > 0
                                            && matches!(
                                                state.column_hit(row),
                                                Some((sidebar::Hit::Title, _))
                                            )
                                        {
                                            let rows = state.inbox.rows.min(state.inbox_room());
                                            state.inbox_drag = Some((row, rows));
                                        }
                                    }
                                }
                            }
                            Action::Drag { row, col, shift } => {
                                // Like a click: the drag starts on the live pane,
                                // never on the list under the overlay.
                                if state.focus {
                                    close_focus(&mut state);
                                    dirty = true;
                                }
                                // A held row, an agent's or a space's: a drag once it leaves
                                // its row, and the notice row says what letting go here would do.
                                if state.overlay_open() {
                                    drop_carry(&mut state);
                                }
                                if let Some(press) = state.row_press.as_mut() {
                                    if press.dead {
                                        continue;
                                    }
                                    press.moved |=
                                        row != press.at.0 || col.abs_diff(press.at.1) > 2;
                                    if press.moved {
                                        // The chip rides with the pointer, so every motion
                                        // of a carried row is a paint.
                                        press.now = (row, col);
                                        let source = press.source;
                                        let drop = state.drop_target(source, row, col);
                                        state.message = Some(drop_hint(&state, source, drop));
                                        dirty = true;
                                    }
                                    continue;
                                }
                                // The held title moves the inbox's edge with the pointer.
                                if let Some((from, rows)) = state.inbox_drag {
                                    let want = (rows as i64 + i64::from(row) - i64::from(from))
                                        .clamp(1, state.inbox_room().max(1) as i64)
                                        as usize;
                                    if want != state.inbox.rows {
                                        state.inbox.rows = want;
                                        dirty = true;
                                    }
                                    continue;
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
                                // A held split divider latches the same way.
                                if state.modes.split_drag.is_some() {
                                    if state.split_motion(col, row) {
                                        dirty = true;
                                    }
                                    continue;
                                }
                                let Some((pane_row, pane_col)) = state.in_pane(row, col).or_else(|| {
                                    state
                                        .forwarded_press
                                        .then(|| clamped_to_focused(&state, row, col))
                                }) else {
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
                                // The inbox's edge is let go where it is.
                                if state.inbox_drag.take().is_some() {
                                    continue;
                                }
                                // A held row comes up: a drop where it went, or the click it
                                // was. A click on the name of the agent already under the
                                // keyboard renames its conversation.
                                if state.overlay_open() {
                                    drop_carry(&mut state);
                                }
                                if let Some(press) = state.row_press.take() {
                                    if press.dead {
                                        continue;
                                    }
                                    // A held space's rule: carried to its place, or the click it
                                    // was, folding the space or going to it.
                                    if let Source::Space { space, fold } = press.source {
                                        if press.moved {
                                            state.message = None;
                                            let drop = state.drop_target(press.source, row, col);
                                            if let Some(ask) = drop.and_then(|drop| {
                                                drop_space(&mut state, space, drop, &mut request)
                                            }) {
                                                wr.write_all(&client::encode_control_seq(
                                                    &ask, &mut control_seq,
                                                )?)
                                                .await?;
                                            }
                                        } else if fold {
                                            if !state.shut.remove(&space) {
                                                state.shut.insert(space);
                                            }
                                        } else if let Some(bytes) = enter_space(
                                            &mut state, space, &mut control_seq, &mut seqs,
                                        )? {
                                            wr.write_all(&bytes).await?;
                                        }
                                        dirty = true;
                                        continue;
                                    }
                                    let (terminal, handle) = match press.source {
                                        Source::Agent(terminal) => (terminal, false),
                                        Source::Pane(terminal) => (terminal, true),
                                        Source::Space { .. } => continue,
                                    };
                                    if press.moved {
                                        state.message = None;
                                        let drop = state.drop_target(press.source, row, col);
                                        if let Some(ask) = drop.and_then(|drop| {
                                            drop_row(&mut state, terminal, drop, &mut request, handle)
                                        }) {
                                            wr.write_all(&client::encode_control_seq(
                                                &ask, &mut control_seq,
                                            )?)
                                            .await?;
                                        }
                                        drain_pending_focus(
                                            &mut state, &mut control_seq, &mut seqs, &mut wr,
                                            &mut dirty,
                                        )
                                        .await?;
                                    } else if let Some(space) = state.sleeping_space_of(terminal) {
                                        wake(&mut state, space, terminal, &mut request);
                                    } else if press.name && terminal == state.focused {
                                        rename_from_row(&mut state, terminal);
                                    } else if let Some(bytes) = focus(
                                        &mut state, terminal, &mut control_seq, &mut seqs,
                                    )? {
                                        wr.write_all(&bytes).await?;
                                    }
                                    dirty = true;
                                    continue;
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
                                                size_terminals(&mut state, &mut seqs)?
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
                                    // The paint after a release sizes the tabs off screen
                                    // that waited out the drag.
                                    dirty = true;
                                    continue;
                                }
                                if state.modes.split_drag.take().is_some() {
                                    dirty = true;
                                    continue;
                                }
                                let inside = state.in_pane(row, col).or_else(|| {
                                    state
                                        .forwarded_press
                                        .then(|| clamped_to_focused(&state, row, col))
                                });
                                state.forwarded_press = false;
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
                                    // Over the column, which is where a question is drawn
                                    // and where scrolling moves it out of view.
                                    if let Some(said) = take_question(&mut state) {
                                        state.message = Some(said);
                                        dirty = true;
                                    }
                                    if wheel_list(&mut state, row, col, up) {
                                        // The rows moved under a held drag: what letting go
                                        // would do now.
                                        if let Some(press) =
                                            state.row_press.filter(|p| p.moved && !p.dead)
                                        {
                                            let drop = state.drop_target(
                                                press.source, press.now.0, press.now.1,
                                            );
                                            state.message =
                                                Some(drop_hint(&state, press.source, drop));
                                        }
                                        dirty = true;
                                    }
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
                            Action::Redraw => {
                                // A fresh presenter, or the repaint writes only rows that
                                // differ from its cache, which is none: the key existed
                                // for a screen the outer terminal mangled, and did nothing.
                                presenter = present::Presenter::default();
                                state.shown_images.clear();
                                dirty = true;
                            }
                            // With nothing awake in the space the keys move between its
                            // sleeping tabs without waking anything, so one can be picked
                            // to wake or discard. With a tab awake they go to the tabs.
                            Action::Next | Action::Previous
                                if state.dim_tab().is_some() && state.tabs().is_empty() =>
                            {
                                let tabs: Vec<u32> =
                                    state.dim_tabs().iter().map(|t| t.terminal).collect();
                                let at = tabs
                                    .iter()
                                    .position(|t| Some(*t) == state.dim_tab())
                                    .unwrap_or(0);
                                let n = tabs.len();
                                state.dim = if matches!(action, Action::Next) {
                                    tabs[(at + 1) % n]
                                } else {
                                    tabs[(at + n - 1) % n]
                                };
                                dirty = true;
                            }
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
                                state.prompt = Some(Prompt::new(Naming::Terminal, ""));
                                dirty = true;
                            }
                            Action::CreateSpace => {
                                state.prompt = Some(Prompt::new(Naming::Space, ""));
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
                                        "space still holds {held} terminal{}, close them first ({} x)",
                                        if held == 1 { "" } else { "s" },
                                        crate::keys::prefix_name()
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
                                state.prompt = Some(Prompt::new(Naming::RenameTerminal(state.focused), current));
                                dirty = true;
                            }
                            Action::Rename => {}
                            Action::Menu => {
                                state.menu_kind = MenuKind::Main;
                                state.menu = Some(0);
                                dirty = true;
                            }
                            Action::Keybinds => {
                                state.reading = Some(Reading::Keybinds);
                                state.keys_query = None;
                                dirty = true;
                            }
                            Action::Collapse => {
                                state.collapsed = !state.collapsed;
                                // The pane's width changed with the column, and a child
                                // that is not told reflows its output against a width it no
                                // longer has. Every tile is told its own.
                                if let Some(resize) =
                                    size_terminals(&mut state, &mut seqs)?
                                {
                                    wr.write_all(&resize).await?;
                                }
                                dirty = true;
                            }
                            Action::SpaceSettings => {
                                if let Some(space) = state.space {
                                    state.prompt = Some(space_settings(&state, space));
                                    dirty = true;
                                }
                            }

                            // A busy agent is asked about first: one stray `x` after the
                            // prefix must not end an hour of its work.
                            Action::Close => {
                                close_focused(&mut state, &mut request);
                                dirty = true;
                            }
                            Action::Sleep => {
                                sleep_toggle(&mut state, &mut request);
                                dirty = true;
                            }
                            Action::Resize => {
                                enter_resize(&mut state);
                                dirty = true;
                            }
                            Action::Equalize => {
                                equalize(&mut state);
                                dirty = true;
                            }
                            Action::CopyMode => {
                                enter_copy(&mut state);
                                dirty = true;
                            }
                            Action::Palette => {
                                state.palette = Some(Palette::with_agents(crate::config::agents()));
                                dirty = true;
                            }
                            // The first of the next row's queue, the way a click on that row
                            // goes there. Standing on a finished agent reads it, so the next
                            // press finds the one after: wants, then failed, then done.
                            Action::UpNext => {
                                let next = sidebar::queue(
                                    &state.rows,
                                    state.focused,
                                    &state.seen_done,
                                    &state.unseen_finished,
                                )
                                .first()
                                .map(|row| row.terminal);
                                match next {
                                    Some(picked) => {
                                        if let Some(bytes) = focus(
                                            &mut state, picked, &mut control_seq, &mut seqs,
                                        )? {
                                            wr.write_all(&bytes).await?;
                                        }
                                    }
                                    None => {
                                        state.message = Some("nothing needs you".to_string());
                                    }
                                }
                                dirty = true;
                            }
                            Action::ToggleView => {
                                state.view = state.view.toggle();
                                dirty = true;
                            }
                            // The focused agent's last answer, read off the loop: a page over the
                            // pane when it arrives, a sentence on the notice row when it cannot.
                            Action::Answer => {
                                let row = state.rows.iter().find(|r| r.terminal == state.focused);
                                match row {
                                    None => {
                                        state.message = Some("nothing focused to read".to_string());
                                    }
                                    Some(row) if crate::mux::is_remote_terminal(row.terminal) => {
                                        state.message = Some(
                                            "that agent keeps its answers on another machine".to_string(),
                                        );
                                    }
                                    Some(row) if !row.agent => {
                                        state.message =
                                            Some("not an agent, so no answer to read".to_string());
                                    }
                                    // While it works or waits, its newest message is a tool call
                                    // more often than an answer; the page waits for it to stop.
                                    Some(row) if row.state == "blocked" => {
                                        state.message = Some(
                                            "waiting on you: answer it in its pane, the page opens once it stops"
                                                .to_string(),
                                        );
                                    }
                                    Some(row) if matches!(row.state.as_str(), "working" | "starting") => {
                                        state.message = Some(
                                            "still working: its last answer opens once it stops"
                                                .to_string(),
                                        );
                                    }
                                    // Already being read: one read at a time, the page opens
                                    // when it arrives.
                                    Some(row) if answer_for == Some(row.terminal) => {}
                                    Some(row) => {
                                        let terminal = row.terminal;
                                        answer_for = Some(terminal);
                                        let program = row.program.clone().unwrap_or_default();
                                        let session = row.agent_session.clone();
                                        let title = row
                                            .agent_title
                                            .clone()
                                            .or_else(|| row.program.clone())
                                            .unwrap_or_else(|| "agent".to_string());
                                        let tx = blocking_tx.clone();
                                        tokio::task::spawn_blocking(move || {
                                            let _ = tx.send(Blocked::Answer(
                                                terminal,
                                                read_answer(program, session, title),
                                            ));
                                        });
                                        state.message =
                                            Some("reading the last answer".to_string());
                                    }
                                }
                                dirty = true;
                            }
                            // Only in the tree, and only a space with agents or sleeping tabs in it: a fold with
                            // nothing under it shows nothing, and would hide the first agent that
                            // arrives later without anybody having seen it fold.
                            Action::FoldSpace => {
                                if let Some(space) = state.space
                                    && state.view == sidebar::View::Tree
                                    && (state.rows.iter().any(|a| a.agent && a.workspace == space)
                                        || state.spaces.iter().any(|w| {
                                            w.id == space && !w.sleeping.is_empty()
                                        }))
                                    && !state.shut.remove(&space)
                                {
                                    state.shut.insert(space);
                                }
                                dirty = true;
                            }
                            Action::Focus => {
                                if state.focus {
                                    close_focus(&mut state);
                                    dirty = true;
                                } else if state.focused == 0 {
                                    state.message = Some(format!(
                                        "nothing to focus, {} c starts a terminal",
                                        crate::keys::prefix_name()
                                        ),
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
                                    state.message = Some(format!(
                                        "nothing to split, {} c starts a terminal",
                                        crate::keys::prefix_name()
                                        ),
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
                                            state.split_origin.insert(format!("split-{request}"), (s, focused));
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
                                        // Out of the tree is a tab again. Recorded as
                                        // the tree that is left: a lone leaf clears it.
                                        if let Some(tree) = layout.to_pane() {
                                            state.split_writes.push(tree);
                                        }
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
                                        state.message = Some(format!(
                                            "only one pane, {} x closes its terminal",
                                            crate::keys::prefix_name()
                                            ),
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
                    drain_modes(&mut state, &mut control_seq, &mut wr, &mut dirty).await?;
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
                                    refetch_pinned(&mut state);
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
                                        // Bootstrap the tiling state to match: the tree this
                                        // terminal is in (one pane unless the index already
                                        // brought its tab's split), and its subscription.
                                        // `activate` owes the other panes theirs.
                                        if let Some(space) = state.space {
                                            activate(&mut state, space, terminal);
                                            state.subs.insert(terminal);
                                        }
                                        // And the size to match: the child was sized for
                                        // whoever had it before (a restore spawns at the
                                        // default), so without this the first paint shows
                                        // a small screen until anything moves the focus.
                                        if let Some(resize) =
                                            size_terminals(&mut state, &mut seqs)?
                                        {
                                            wr.write_all(&resize).await?;
                                        }
                                        dirty = true;
                                    }
                                }
                            }
                            // Only a raw connection says this, and this one is not.
                            Incoming::Raw { .. } => {}
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
                            // `qoro focus`, from a notch or a launcher: the same switch a
                            // click on the row makes. A terminal this interface does not
                            // list yet waits for the index that names it.
                            Incoming::Event(EventMessage::Focus { terminal }) => {
                                if !state.rows.iter().any(|r| r.terminal == terminal) {
                                    state.show_when_listed =
                                        Some((terminal, std::time::Instant::now() + SHOW_WAIT, false));
                                    refresh = true;
                                } else {
                                    // The newest ask wins over one still waiting.
                                    state.show_when_listed = None;
                                    if let Some(bytes) =
                                        focus(&mut state, terminal, &mut control_seq, &mut seqs)?
                                    {
                                        wr.write_all(&bytes).await?;
                                        dirty = true;
                                    }
                                }
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
                                        // The mux holds merged lists back until the
                                        // link answers again; this is that question.
                                        | ControlResponse::SplitSaved { .. }
                                        | ControlResponse::WorkspaceAsleep { .. }
                                        | ControlResponse::WorkspaceAwake { .. }
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
                                        | ControlResponse::WorkspaceAsleep { .. }
                                        | ControlResponse::WorkspaceAwake { .. }
                                );
                                // A row in a space this client does not list: another
                                // client (`qoro new --space`) made it, and no answer
                                // here said so. Asked for once the first list is in,
                                // so the bootstrap does not ask twice. A slept row can
                                // outlive its space, and would ask on every refresh.
                                if let ControlResponse::Agents { agents, .. } = &response
                                    && !state.spaces.is_empty()
                                    && agents.iter().any(|r| {
                                        r.state != "sleeping"
                                            && r.workspace != 0
                                            && !state.spaces.iter().any(|s| s.id == r.workspace)
                                    })
                                {
                                    refresh_spaces = true;
                                }
                                if let ControlResponse::Agents { agents, .. } = &response
                                    && slept_or_woken(&state.spaces, agents)
                                {
                                    refresh_spaces = true;
                                }
                                let index_answer = match &response {
                                    // The index this loop asked for, not the mux's own pushes.
                                    ControlResponse::Agents { id, .. } | ControlResponse::Failed { id, .. } => {
                                        !index_id.is_empty() && *id == index_id
                                    }
                                    _ => false,
                                };
                                if index_answer {
                                    index_asked = None;
                                    refresh |= std::mem::take(&mut index_owed);
                                }
                                apply(&mut state, response, &mut request, &mut dirty);
                                if state.closing.is_none() {
                                    closing_since = None;
                                }
                            }
                        }
                    }
                    if let Some(ack) = connection.renders().due_ack(std::time::Instant::now()) {
                        wr.write_all(&ack).await?;
                    }
                    // A new space's first terminal follows the
                    // focused terminal's directory, which still names the space
                    // the person came from at this point, when the new space is on
                    // the same machine.
                    for space in std::mem::take(&mut state.wants_terminal_in) {
                        let cwd = focused_cwd_in(&state, space);
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
                    // qoro was just put on a machine the person is connecting: connect it now,
                    // as if it had been there all along.
                    if let Some(target) = state.reprobe.take() {
                        request += 1;
                        let id = format!("probe-{request}");
                        state.probing = Some(id.clone());
                        state.connecting = Some(target.clone());
                        wr.write_all(&client::encode_control_seq(
                            &ControlRequest::ConnectRemote { id, target },
                            &mut control_seq,
                        )?).await?;
                    }
                    drain_pending_focus(&mut state, &mut control_seq, &mut seqs, &mut wr, &mut dirty).await?;
                    drain_modes(&mut state, &mut control_seq, &mut wr, &mut dirty).await?;
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
                    if refresh && index_asked.is_some_and(|at| at.elapsed() < INDEX_PATIENCE) {
                        index_owed = true;
                    } else if refresh {
                        request += 1;
                        wr.write_all(&client::encode_control_seq(
                            &ControlRequest::ListAgents { id: format!("index-{request}") },
                            &mut control_seq,
                        )?).await?;
                        index_asked = Some(tokio::time::Instant::now());
                        index_id = format!("index-{request}");
                    }
                    if refresh_spaces {
                        request += 1;
                        wr.write_all(&client::encode_control_seq(
                            &ControlRequest::ListWorkspaces { id: format!("spaces-{request}") },
                            &mut control_seq,
                        )?).await?;
                    }
                }
                // A held escape that nothing completed: it was a key. Released into the
                // filter, and the stdin arm is woken to type it through the ordinary path.
                _ = async {
                    match hold_at {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => {
                    reports.expire();
                    if let Some(wake) = wake_stdin.upgrade() {
                        let _ = wake.try_send(Vec::new());
                    }
                }
                _ = async {
                    match notes_at {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => {
                    fire_notes(&mut state, &mut out)?;
                }
                // A close that has not finished in [`CLOSE_PATIENCE`] is not going to.
                // Said once, with what is left, rather than left as a row that never
                // changes. A tab the daemon would not take is named, because that is the
                // one thing the person can do something about.
                _ = async {
                    match closing_since {
                        Some(at) => tokio::time::sleep_until(at + CLOSE_PATIENCE).await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Some(space) = state.closing.as_ref().map(|c| c.space) {
                        let left = tabs_left(&state, space);
                        let stuck = state.closing.as_ref().and_then(|c| c.stuck);
                        // On the tab first, not on the count. A tab the daemon would not
                        // take is still listed by the index, because it never tore down,
                        // so a match that asked the count first would never name it.
                        state.message = Some(match (stuck, left) {
                            (Some(terminal), _) => format!(
                                "tab {terminal} would not close, {} kept",
                                space_name(&state, space)
                            ),
                            (None, 0) => format!(
                                "close of {} gave up, a tab nobody answered for",
                                space_name(&state, space)
                            ),
                            (None, left) => format!(
                                "close of {} gave up, {left} {} still there",
                                space_name(&state, space),
                                plural(left)
                            ),
                        });
                        state.closing = None;
                    }
                    closing_since = None;
                    dirty = true;
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
                // The next animation frame, armed only while an agent works. It only
                // marks the frame dirty: the FRAME arm still decides when it goes out,
                // and the presenter writes only the cells the frame changed.
                _ = async {
                    match motion_at {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => {
                    state.tick = state.tick.wrapping_add(1).max(1);
                    state.step_drawer();
                    motion_at = Some(tokio::time::Instant::now() + MOTION_TICK);
                    dirty = true;
                }
                // The next cost sample, armed only while `QORO_USAGE=1`.
                _ = async {
                    match usage_at {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => {
                    usage_at = Some(tokio::time::Instant::now() + USAGE_EVERY);
                    let targets = usage_targets(&state);
                    if targets.is_empty() {
                        state.usage.clear();
                        state.usage_seen.clear();
                    } else if !sampling {
                        sampling = true;
                        let tx = blocking_tx.clone();
                        tokio::task::spawn_blocking(move || {
                            let groups: Vec<qoro_runtime::Pgid> =
                                targets.iter().map(|(_, g)| qoro_runtime::Pgid(*g)).collect();
                            let found = qoro_runtime::group_processes(&groups);
                            let at = tokio::time::Instant::now();
                            let _ = tx.send(Blocked::Usage { at, targets, found });
                        });
                    }
                }
                // Blocking work handed back.
                Some(done) = blocking_rx.recv() => match done {
                    Blocked::Usage { at, targets, found } => {
                        sampling = false;
                        apply_usage(&mut state, at, targets, found);
                        dirty = true;
                    }
                    // Only the answer last asked for, and only while its agent still has the
                    // keyboard: a slow read for a terminal left behind opens nothing.
                    Blocked::Answer(terminal, _) if answer_for != Some(terminal) || state.focused != terminal => {
                        if answer_for == Some(terminal) {
                            answer_for = None;
                        }
                    }
                    // An overlay that took the keyboard meanwhile keeps it: the page would
                    // otherwise open out of nowhere when it closes, over whatever the person
                    // is typing by then.
                    Blocked::Answer(_, Ok(_))
                        if state.palette.is_some() || state.prompt.is_some() || state.menu.is_some() =>
                    {
                        answer_for = None;
                        state.message = None;
                    }
                    Blocked::Answer(_, Ok(answer)) => {
                        answer_for = None;
                        // A `ctrl-a` typed while the answer was read belongs to nothing now.
                        keys.clear_prefix();
                        // The page is the answer to "reading the last answer": that
                        // sentence has nothing left to say.
                        state.message = None;
                        state.answer = Some(answer);
                        dirty = true;
                    }
                    Blocked::Answer(_, Err(why)) => {
                        answer_for = None;
                        state.message = Some(why);
                        dirty = true;
                    }
                },
                // A change owed to an index request that never answered: after
                // `INDEX_PATIENCE` it is taken as lost and the owed change asks on its
                // own, so the last change of a burst is never left waiting for the next.
                _ = async {
                    match index_asked.filter(|_| index_owed) {
                        Some(at) => tokio::time::sleep_until(at + INDEX_PATIENCE).await,
                        None => std::future::pending().await,
                    }
                } => {
                    index_owed = false;
                    request += 1;
                    wr.write_all(&client::encode_control_seq(
                        &ControlRequest::ListAgents { id: format!("index-{request}") },
                        &mut control_seq,
                    )?).await?;
                    index_asked = Some(tokio::time::Instant::now());
                    index_id = format!("index-{request}");
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
    let _ = out.write_all(&exit_trailer());
    let _ = out.flush();
    outcome
}

/// The bytes that leave the host terminal usable, built pure so tests pin them.
///
/// Modes off in the reverse order of enable, then the cursor forced visible and
/// the attributes reset, then out of the alternate screen. The visible cursor
/// and reset are the point: a program that hid the cursor must not leave the
/// host cursor invisible, and no chrome colour may leak into the next prompt.
fn exit_trailer() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1b[?2031l\x1b[?1004l\x1b[?1006l\x1b[?1002l\x1b[?1000l");
    out.extend_from_slice(b"\x1b[?25h\x1b[0m\x1b[?1049l");
    out
}

/// Whether `config.toml` changed since it was last seen, by its modified time: a change from
/// `qoro settings` in a tab, `qoro sound`, `qoro config set` or an editor. A file that is
/// gone, or comes back, counts as a change too.
fn config_moved(state: &mut State) -> bool {
    let now = config_stamp();
    std::mem::replace(&mut state.config_stamp, now) != now
}

fn config_stamp() -> Option<std::time::SystemTime> {
    std::fs::metadata(crate::config::path())
        .and_then(|m| m.modified())
        .ok()
}

/// Put a read of `config.toml` in effect: the prefix every overlay and the screen's words
/// use. The decoder follows the prefix on the next keystroke (see the stdin arm).
fn use_config(state: &mut State, read: &crate::config::Read) {
    state.config = read.config;
    crate::keys::show_prefix(read.config.prefix);
    retheme(state);
    // Icons turned back on in a running session get their font now, not at the next start.
    if !state.icon_font && state.config.icons != crate::icons::Setting::Off {
        state.icon_font = crate::icons::install(&crate::colorlevel::current_env());
    }
    reicon(state);
}

/// Decide again whether the painters draw icons, from the config, what the terminal said
/// and whether the font is in place. True when that changed, so the column repaints.
fn reicon(state: &State) -> bool {
    crate::icons::set(crate::icons::decide(
        state.config.icons,
        &crate::colorlevel::current_env(),
        &state.icon_reply,
        state.icon_font,
        crate::colorlevel::level() == crate::colorlevel::Level::Mono,
    ))
}

/// Keep what the terminal said about itself.
///
/// True when its colours are worth asking for again: it came back into focus, which is
/// how a terminal that never reports its scheme is caught having switched while the
/// person was away, or it said its scheme changed, and the background changed with it.
fn note_report(state: &mut State, report: crate::input::Report) -> bool {
    use crate::input::Report;
    match report {
        Report::Focus(focused) => {
            state.outer_focus = Some(focused);
            focused
        }
        Report::Color { background, rgb } => {
            state.host_colors[usize::from(background)] = Some(rgb);
            false
        }
        Report::Terminal(answer) => {
            state.icon_reply = crate::icons::Reply::Named(answer);
            reicon(state);
            false
        }
        // Every terminal answers DA1, and after XTVERSION when it has a name to give: an
        // answer to DA1 with none before it is a terminal that has none.
        Report::Attributes => {
            if state.icon_reply == crate::icons::Reply::Waiting {
                state.icon_reply = crate::icons::Reply::Silent;
                reicon(state);
            }
            false
        }
        Report::Scheme { light } => {
            let switched = state.host_light.is_some_and(|was| was != light);
            state.host_light = Some(light);
            switched
        }
    }
}

/// What the interface asks the terminal about itself: its foreground and background, then
/// to say whenever its light or dark scheme changes (`CSI ? 2031 h`) and what it is now
/// (`CSI ? 996 n`). The answers come back on the keyboard stream and `input` takes them
/// out of it. Terminals that know none of these ignore them.
const ASK_COLORS: &[u8] = b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\";
const ASK_SCHEME: &[u8] = b"\x1b[?2031h\x1b[?996n";
/// Its name (XTVERSION, `CSI > 0 q`) and then its device attributes (`CSI c`), which every
/// terminal answers: the second answer arriving says the first one is not coming.
const ASK_VERSION: &[u8] = b"\x1b[>0q\x1b[c";

/// Whether the terminal may be asked at all. The Linux console answers none of these and
/// has been seen to print the question instead; a `dumb` terminal is a pipe, and asking
/// a pipe prints garbage into whatever reads next.
fn may_ask_terminal() -> bool {
    std::env::var("TERM").map_or(true, |term| term != "linux" && term != "dumb")
}

/// Pick the palette from what is known, most certain first: the file, then the background
/// the terminal answered with, then the scheme it said, then `COLORFGBG`, then dark.
fn retheme(state: &State) {
    use crate::config::Theme;
    let light = match state.config.theme {
        Theme::Dark => false,
        Theme::Light => true,
        Theme::Auto => state.host_colors[1]
            .map(crate::theme::is_light_background)
            .or(state.host_light)
            .or_else(|| {
                std::env::var("COLORFGBG")
                    .ok()
                    .and_then(|value| crate::theme::colorfgbg_is_light(&value))
            })
            .unwrap_or(false),
    };
    crate::theme::set_light(light);
    crate::theme::set_ground(state.host_colors[1]);
}

/// The colours to tell the daemon, when both are known, they are not what it was last
/// told, and it has not already said it does not know how to hear them.
fn host_colors_request(state: &mut State, request: &mut u64) -> Option<ControlRequest> {
    let (Some(fg), Some(bg)) = (state.host_colors[0], state.host_colors[1]) else {
        return None;
    };
    if state.host_refused || state.host_sent == Some((fg, bg)) {
        return None;
    }
    state.host_sent = Some((fg, bg));
    *request += 1;
    Some(ControlRequest::HostColors {
        id: format!("host-{request}"),
        fg,
        bg,
    })
}

/// The rows this interface shows as news (finished while the keyboard was elsewhere, or a
/// `done` not read), when they are not what the daemon was last told. Told so that every
/// other index, the notch among them, says the same "done" this one does. Only this
/// machine's rows: the numbers of another machine's are not this daemon's.
fn news_request(state: &mut State, request: &mut u64) -> Option<ControlRequest> {
    if state.news_refused {
        return None;
    }
    let news: Vec<u32> = state
        .rows
        .iter()
        .filter(|r| !crate::mux::is_remote_workspace(r.workspace))
        .filter(|r| sidebar::is_new(r, state.focused, &state.seen_done, &state.unseen_finished))
        .map(|r| r.terminal)
        .collect();
    if state.news_sent.as_ref() == Some(&news) {
        return None;
    }
    state.news_sent = Some(news.clone());
    *request += 1;
    Some(ControlRequest::News {
        id: format!("news-{request}"),
        terminals: news,
    })
}

/// Hold a desktop note for a row that has just started needing a person who is not
/// looking at it: another tab has the keyboard, or the whole window lost the focus.
///
/// `qoro_attention` decides whether the change is worth it and how urgent it is, the same
/// rule a push to a phone follows. Each note waits out its own `notify::DELAY`, and is
/// checked again then (see [`fire_notes`]). True when it will reach the desktop, so the
/// bell for the same change stays quiet; with nowhere to send it, the bell rings as it
/// always did and the hint row says why, once.
fn note(state: &mut State, row: &AgentRow, runtime: AgentRuntimeState) -> bool {
    let looking = row.terminal == state.focused && state.outer_focus != Some(false);
    if looking || !(state.config.desktop || state.config.sound) {
        return false;
    }
    let terminal = qoro_domain::TerminalId::new(row.terminal.to_string());
    let Some(push) = qoro_attention::Push::for_state(0, &terminal, runtime, 0) else {
        return false;
    };
    let generation = sidebar::gen_of(row);
    let held = state
        .notes
        .iter()
        .any(|n| n.terminal == row.terminal && n.generation == generation && n.state == row.state);
    if !held {
        let due = std::time::Instant::now() + crate::notify::DELAY;
        state.notes.push(crate::notify::Note {
            terminal: row.terminal,
            state: row.state.clone(),
            generation,
            label: sidebar::label(row),
            what: match push.reason {
                qoro_attention::Reason::AgentBlocked => "is blocked",
                qoro_attention::Reason::AgentFailed => "failed",
                _ => "finished",
            },
            space: state
                .spaces
                .iter()
                .find(|s| s.id == row.workspace)
                .map(|s| s.name.clone())
                .unwrap_or_default(),
            urgent: push.priority == qoro_attention::Priority::Urgent,
            due,
        });
        state.notes_due = Some(state.notes_due.map_or(due, |at| at.min(due)));
    }
    match state.route {
        crate::notify::Route::Nowhere(why) if state.config.desktop => {
            if std::mem::take(&mut state.nowhere) {
                state.hint = Some(crate::notify::nowhere_hint(why, &crate::config::path()));
            }
            false
        }
        _ => state.config.desktop,
    }
}

/// Say the notes that have waited their full second, as one note: whatever is still true
/// of them. A row that went back to work, or that the person has since come to look at,
/// is dropped, so a blip says nothing. Notes still waiting keep the timer armed.
fn fire_notes(state: &mut State, out: &mut impl Write) -> io::Result<()> {
    let now = std::time::Instant::now();
    let (ready, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut state.notes)
        .into_iter()
        .partition(|note| note.due <= now);
    state.notes_due = waiting.iter().map(|note| note.due).min();
    state.notes = waiting;
    let due: Vec<crate::notify::Note> = ready
        .into_iter()
        .filter(|note| {
            let still = state.rows.iter().any(|r| {
                r.terminal == note.terminal
                    && r.state == note.state
                    && sidebar::gen_of(r) == note.generation
            });
            let looking = note.terminal == state.focused && state.outer_focus != Some(false);
            still && !looking
        })
        .collect();
    if due.is_empty() {
        return Ok(());
    }
    let body = crate::notify::clean(&crate::notify::say(&due));
    if state.config.desktop {
        match state.route {
            crate::notify::Route::Escape { escape, tmux } => {
                out.write_all(&crate::notify::escape(escape, tmux, &body))?;
                out.flush()?;
            }
            crate::notify::Route::System(program) => {
                crate::notify::spawn(&crate::notify::call(program, &body));
            }
            crate::notify::Route::Nowhere(_) => {}
        }
    }
    // Played here and not on the far side of an ssh session, where nobody is listening.
    if state.config.sound
        && std::env::var_os("SSH_CONNECTION").is_none()
        && let Some(play) = crate::notify::sound(
            due.iter().any(|note| note.urgent),
            cfg!(target_os = "macos"),
            state.config.done_sound,
            state.config.alert_sound,
        )
    {
        crate::notify::spawn(&play);
    }
    Ok(())
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
    let prefix = state.config.prefix;
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
                    // Shift-tab, the filter before. Only as a read of its own, like tab.
                    Some(b'Z') if i == 0 && buf.len() == 3 => {
                        palette.cycle(false);
                        i += 3;
                    }
                    // A mouse report, swallowed whole, read the way the key decoder
                    // reads one. Split across the read, it is stashed like the key
                    // decoder stashes its own.
                    Some(b'<') => match crate::keys::report(&buf[i..]) {
                        crate::keys::Report::Whole(used, _) | crate::keys::Report::Broken(used) => {
                            i += used
                        }
                        crate::keys::Report::Partial => {
                            state.palette_pending = buf[i..].to_vec();
                            break;
                        }
                    },
                    // Any other sequence (arrows with modifiers, function keys),
                    // skipped whole: nothing to do with it here, and it must not reach
                    // a shell, nor its tail the query (`Delete` used to type `~`).
                    // A control byte before the final one ends it unfinished, as it does
                    // for the parser, and is read as itself: held on, an `ESC [` that
                    // never finished swallowed Enter, esc and the detach after it.
                    Some(_) => match skip_sequence(&buf, i) {
                        Some(next) => i = next,
                        None => {
                            state.palette_pending = buf[i..].to_vec();
                            break;
                        }
                    },
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
            // Tab, the next filter. Only as a read of its own: a tab inside a paste is
            // text the palette cannot hold, dropped as any control byte is.
            // ponytail: no bracketed paste here, so two tabs batched into one read (ssh,
            // autorepeat) are dropped too; turn on ?2004 if that ever bites.
            b'\t' if buf.len() == 1 => {
                palette.cycle(true);
                i += 1;
            }
            byte if byte == prefix => {
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

/// Where the split asked for by `id` is waiting: its space, and which of that
/// space's trees (`None` for the one on screen, else the parked index). The
/// tree is the one holding the pane that was split, since it may have been
/// parked since and another tab of the space may have a split pending too. An
/// answer with no record falls back to the space on screen.
fn split_home(state: &mut State, id: &str) -> Option<(i64, Option<usize>)> {
    let (space, split) = match state.split_origin.remove(id) {
        Some((space, split)) => (space, Some(split)),
        None => (state.space?, None),
    };
    let waits = |layout: &Layout| layout.has_pending() && split.is_none_or(|t| layout.contains(t));
    if state.layouts.get(&space).is_some_and(waits) {
        return Some((space, None));
    }
    let at = state.parked.get(&space)?.iter().position(waits)?;
    Some((space, Some(at)))
}

/// The tree `split_home` named.
fn tree_at(state: &mut State, space: i64, at: Option<usize>) -> Option<&mut Layout> {
    match at {
        None => state.layouts.get_mut(&space),
        Some(at) => state.parked.get_mut(&space)?.get_mut(at),
    }
}

/// Take the terminals `gone` names out of every tree, on screen and parked.
///
/// A leaf nobody can focus is a zombie pane. A tree on screen left with no
/// leaves and no pending split goes, so the next focus reseeds a single pane;
/// a parked tree left with one leaf goes too, and that leaf is a tab again.
/// Never writes to the daemon: what is gone is gone there already.
fn prune_trees(state: &mut State, gone: impl Fn(u32) -> bool) {
    let State {
        layouts, parked, ..
    } = &mut *state;
    for layout in layouts.values_mut().chain(parked.values_mut().flatten()) {
        for terminal in layout.leaves().into_iter().filter(|t| gone(*t)) {
            layout.remove(terminal);
        }
    }
    layouts.retain(|_, layout| !layout.leaves().is_empty() || layout.has_pending());
    for trees in parked.values_mut() {
        trees.retain(|layout| layout.leaf_count() > 1 || layout.has_pending());
    }
}

/// Take a tab that moved to another space, by this client or another, out of the trees
/// of the space it left. A tree left with one pane goes the way `prune_trees` sends it.
fn prune_moved(state: &mut State, agents: &[AgentRow]) {
    let space_of = |t: u32| agents.iter().find(|r| r.terminal == t).map(|r| r.workspace);
    let State {
        layouts, parked, ..
    } = &mut *state;
    let trees = layouts
        .iter_mut()
        .map(|(space, layout)| (*space, layout))
        .chain(
            parked
                .iter_mut()
                .flat_map(|(space, trees)| trees.iter_mut().map(|layout| (*space, layout))),
        );
    for (space, layout) in trees {
        for terminal in layout.leaves() {
            if space_of(terminal).is_some_and(|w| w != space) {
                layout.remove(terminal);
            }
        }
    }
    prune_trees(state, |_| false);
}

/// Where a place in a space's order is, said by the rows a person can see: the agent it
/// goes above, or the agent it goes below when the tab it goes before is a shell the column
/// does not show.
fn place_words(state: &State, terminal: u32, space: i64, place: Place) -> String {
    let agent = |t: u32| state.rows.iter().find(|r| r.terminal == t && r.agent);
    let carried = state
        .carried_into(terminal, space)
        .unwrap_or_else(|| vec![terminal]);
    let order: Vec<u32> = state
        .space_order(space)
        .into_iter()
        .filter(|t| !carried.contains(t))
        .collect();
    let at = match place {
        Place::Before(t) => {
            if let Some(row) = agent(t) {
                return format!("above {}", sidebar::session_name(row));
            }
            order.iter().position(|u| *u == t).unwrap_or(order.len())
        }
        Place::End => order.len(),
    };
    if at == order.len() {
        return format!("to the end of {}", space_name(state, space));
    }
    match order[..at].iter().rev().find_map(|t| agent(*t)) {
        Some(row) => format!("below {}", sidebar::session_name(row)),
        None => format!("to the top of {}", space_name(state, space)),
    }
}

/// What letting a carried space go here would do, as the notice row says it.
fn space_hint(state: &State, space: i64, drop: Option<RowDrop>) -> String {
    let name = space_name(state, space);
    match drop {
        Some(RowDrop::Here) => "already here".to_string(),
        Some(RowDrop::SpaceSlot {
            before: Some(before),
            ..
        }) => format!("move {name} above {}", space_name(state, before)),
        Some(RowDrop::SpaceSlot { before: None, .. }) => format!("move {name} to the end"),
        _ => "drop on the list of spaces to put it in another place".to_string(),
    }
}

/// Ask the daemon to put a carried space where it was let go, or nothing when it is already
/// there.
fn drop_space(
    state: &mut State,
    space: i64,
    drop: RowDrop,
    request: &mut u64,
) -> Option<ControlRequest> {
    let RowDrop::SpaceSlot { before, .. } = drop else {
        return None;
    };
    *request += 1;
    // The list moving under the eye is the answer; a line saying so would outlive it.
    state.message = None;
    Some(ControlRequest::MoveWorkspace {
        id: format!("order-{request}"),
        workspace: space,
        before,
    })
}

/// The id a reorder inside its own space is asked under.
const REORDER: &str = "reorder-";

/// The id a pane taken out of its split for a tab of its own is asked under.
const LEAVE: &str = "leave-";

/// Let go of a held row without dropping it, and of the hint it was showing.
///
/// The press stays, called off, until its release: the button is still down, and what it
/// does next belongs to nobody.
fn drop_carry(state: &mut State) {
    if let Some(press) = state.row_press.as_mut() {
        if press.moved && !press.dead {
            state.message = None;
        }
        press.dead = true;
    }
}

/// What letting a dragged row go here would do, as the notice row says it.
fn drop_hint(state: &State, source: Source, drop: Option<RowDrop>) -> String {
    let (terminal, handle) = match source {
        Source::Agent(terminal) => (terminal, false),
        Source::Pane(terminal) => (terminal, true),
        Source::Space { space, .. } => return space_hint(state, space, drop),
    };
    const ANYWHERE: &str = "drop on a space, or on a pane's edge to split it";
    let name = |t: u32| {
        state
            .rows
            .iter()
            .find(|r| r.terminal == t)
            .map_or_else(|| format!("tab {t}"), sidebar::session_name)
    };
    let from = state
        .rows
        .iter()
        .find(|r| r.terminal == terminal)
        .map(|r| r.workspace);
    if state.is_asleep(terminal) {
        return "a sleeping tab stays where it is, wake it first".to_string();
    }
    if drop.is_none() && state.over_other_machine(terminal) {
        return "a tab stays on its machine".to_string();
    }
    match drop {
        Some(RowDrop::Here) => "already here".to_string(),
        Some(RowDrop::SpaceSlot { .. }) => ANYWHERE.to_string(),
        Some(RowDrop::Space(space)) if Some(space) == from => ANYWHERE.to_string(),
        Some(RowDrop::Space(space)) => {
            format!("move {} to {}", name(terminal), space_name(state, space))
        }
        Some(RowDrop::Bar { space, place }) => {
            let at = match place {
                Place::Before(t) => format!("before {}", name(t)),
                Place::End => "at the end".to_string(),
            };
            if Some(space) != from {
                format!(
                    "move {} to {}, {at}",
                    name(terminal),
                    space_name(state, space)
                )
            } else if state.leaving(terminal, handle) {
                format!("take {} out of its split, a tab {at}", name(terminal))
            } else {
                format!("move tab {} {at}", name(terminal))
            }
        }
        Some(RowDrop::Slot { space, place, .. }) if Some(space) == from => {
            let words = place_words(state, terminal, space, place);
            if state.leaving(terminal, handle) {
                format!("take {} out of its split, {words}", name(terminal))
            } else {
                format!("move {} {words}", name(terminal))
            }
        }
        Some(RowDrop::Slot { space, place, .. }) => {
            match place_words(state, terminal, space, place) {
                words if words.starts_with("to ") => format!("move {} {words}", name(terminal)),
                words => format!(
                    "move {} to {}, {words}",
                    name(terminal),
                    space_name(state, space)
                ),
            }
        }
        Some(RowDrop::Pane(pane, _)) if pane == terminal => ANYWHERE.to_string(),
        Some(RowDrop::Pane(pane, side)) => {
            let side = match side {
                layout::Move::Left => "left of",
                layout::Move::Right => "right of",
                layout::Move::Up => "above",
                layout::Move::Down => "below",
            };
            format!("put {} {side} {}", name(terminal), name(pane))
        }
        None => ANYWHERE.to_string(),
    }
}

/// Let a dragged agent row go. Answers the move to ask the daemon for, when there is one.
///
/// A drop on a pane of the space the tab is already in changes this client's trees and
/// records them, nothing more. A drop in another space asks the daemon to move the tab
/// first, and the trees follow its answer ([`ControlResponse::TerminalMoved`]): the
/// daemon takes the tab out of the split it left, and a split recorded before that could
/// lose the tab again.
fn drop_row(
    state: &mut State,
    terminal: u32,
    drop: RowDrop,
    request: &mut u64,
    handle: bool,
) -> Option<ControlRequest> {
    let Some(from) = state
        .rows
        .iter()
        .find(|r| r.terminal == terminal && r.state != "sleeping")
        .map(|r| r.workspace)
    else {
        state.message = Some("a sleeping tab stays in its space, wake it first".to_string());
        return None;
    };
    let (space, beside, at) = match drop {
        RowDrop::Here | RowDrop::SpaceSlot { .. } => return None,
        RowDrop::Space(space) if space == from => return None,
        RowDrop::Space(space) => (space, None, None),
        RowDrop::Slot { space, place, .. } | RowDrop::Bar { space, place } => {
            (space, None, Some(place))
        }
        RowDrop::Pane(pane, _) if pane == terminal => return None,
        RowDrop::Pane(pane, side) => (state.space?, Some((pane, side)), None),
    };
    if let Some((pane, side)) = beside {
        // Tried on a copy: a drop with no room for the pane changes nothing.
        let mut tree = state.layouts.get(&space)?.clone();
        tree.remove(terminal);
        if tree.zoomed.is_some() || tree.has_pending() {
            state.message = Some("unzoom, or let the split on its way land, first".to_string());
            return None;
        }
        if !tree.insert_beside(pane, terminal, side, pane_area(state)) {
            state.message = Some("no room for another pane there".to_string());
            return None;
        }
        if space == from {
            // The person moved it: a focused tab that went away no longer takes the
            // keyboard back. Here and not in `place`, which a daemon's answer shares.
            forget_return(state);
            place(state, space, terminal, tree);
            return None;
        }
    }
    *request += 1;
    // What the move is says so in its id, decided when it is asked: its answer is told apart
    // by that and never by the rows, which an index can change before the answer arrives.
    let leave = space == from && state.leaving(terminal, handle);
    let id = if leave {
        format!("{LEAVE}{request}")
    } else if space == from {
        format!("{REORDER}{request}")
    } else {
        format!("move-{request}")
    };
    if let Some((pane, side)) = beside {
        state.drop_after_move = Some((id.clone(), terminal, pane, side));
    }
    // A split reorders by its first pane, whichever of its rows was carried.
    let terminal = match state.carried_into(terminal, space) {
        Some(carried) if space == from && !leave => carried[0],
        _ => terminal,
    };
    state.message = Some(if leave {
        "taking it out of its split".to_string()
    } else if space == from {
        "reordering".to_string()
    } else {
        format!("moving to {}", space_name(state, space))
    });
    Some(ControlRequest::MoveTerminal {
        id,
        terminal,
        workspace: space,
        place: at,
        leave_split: leave,
    })
}

/// Put `tree`, which holds `terminal`, on screen in `space`, take the terminal out of
/// every other tree, record what changed, and give it the keyboard.
///
/// The new tree is recorded first: a recorded tree replaces every stored one that shares
/// a pane with it, so the trees the tab left, written after, are not taken with it.
fn place(state: &mut State, space: i64, terminal: u32, tree: Layout) {
    let State {
        layouts,
        parked,
        split_writes,
        ..
    } = &mut *state;
    let mut left = Vec::new();
    let others = layouts
        .iter_mut()
        .filter(|(s, _)| **s != space)
        .map(|(_, layout)| layout)
        .chain(parked.values_mut().flatten());
    for layout in others.filter(|layout| layout.contains(terminal)) {
        layout.remove(terminal);
        left.extend(layout.to_pane());
    }
    split_writes.extend(tree.to_pane());
    split_writes.extend(left);
    layouts.insert(space, tree);
    prune_trees(state, |_| false);
    state.space = Some(space);
    if state.focused == terminal {
        state.resync = true;
        state.adopted_resize = true;
    } else {
        state.pending_focus = Some(terminal);
    }
}

/// Open the naming box over the conversation in a terminal, or over the tab when its
/// agent has not said which conversation it is: then the tab's name is what its row shows.
fn rename_from_row(state: &mut State, terminal: u32) {
    let Some(row) = state.rows.iter().find(|r| r.terminal == terminal) else {
        return;
    };
    state.prompt = Some(if row.agent_session.is_some() {
        Prompt::new(
            Naming::RenameSession(terminal),
            row.agent_title.clone().unwrap_or_default(),
        )
    } else {
        Prompt::new(
            Naming::RenameTerminal(terminal),
            row.name.clone().unwrap_or_default(),
        )
    });
}

/// Take the index's split trees for tabs this client holds no split of.
///
/// A client that already has a split over any of the same terminals keeps its
/// own: it made or adopted it, and it writes every change it makes. A tree of
/// one pane is not a split and yields, which is what lets a restart's first
/// render (a single pane) grow back into its tab. Nothing is adopted while a
/// recording is out: the index may predate it.
fn adopt_splits(state: &mut State, agents: &[AgentRow], splits: Vec<qoro_runtime::control::Pane>) {
    if state.split_saving > 0 {
        return;
    }
    let space_of = |t: u32| agents.iter().find(|r| r.terminal == t).map(|r| r.workspace);
    for tree in splits {
        let Some(tree) = tree.retain(&|t| space_of(t).is_some()) else {
            continue;
        };
        let leaves = tree.leaves();
        let Some(space) = space_of(leaves[0]) else {
            continue;
        };
        let held = state.trees().any(|layout| {
            (layout.leaf_count() > 1 || layout.has_pending())
                && layout.leaves().iter().any(|t| leaves.contains(t))
        });
        if leaves.len() < 2 || held || leaves.iter().any(|t| space_of(*t) != Some(space)) {
            continue;
        }
        state
            .parked
            .entry(space)
            .or_default()
            .push(Layout::from_pane(&tree));
        // The single pane on screen is one of its terminals: the tab takes its place.
        if let Some(focus) = state
            .layouts
            .get(&space)
            .map(|layout| layout.focus)
            .filter(|f| leaves.contains(f))
        {
            state.layouts.remove(&space);
            activate(state, space, focus);
            // The focused tile just shrank into its split.
            state.adopted_resize = true;
        }
    }
}

/// Put the tree holding `to` on screen in `space`, with the keyboard in `to`.
///
/// The one place a tab switch changes trees. The tree on screen is parked when
/// it is a split (or one is on its way), never dropped: dropping it is what
/// left a split tab's panes on no tab and in no tree after moving away. A
/// terminal in no tree gets a tree of its own.
fn activate(state: &mut State, space: i64, to: u32) {
    if let Some(layout) = state.layouts.get_mut(&space)
        && layout.focus_terminal(to)
    {
        return;
    }
    let parked = state.parked.entry(space).or_default();
    let mut next = match parked.iter().position(|tree| tree.contains(to)) {
        Some(at) => parked.remove(at),
        None => Layout::single(to),
    };
    next.focus_terminal(to);
    // Other tiles came on screen, and a caller without a write in hand (the
    // index, a bootstrap) cannot subscribe to them: the loop does, next turn.
    state.resync = true;
    if let Some(old) = state.layouts.insert(space, next)
        && (old.leaf_count() > 1 || old.has_pending())
    {
        state.parked.entry(space).or_default().push(old);
    }
}

/// Make sure the space on screen has a tiling tree, seeding a single pane
/// from the focused terminal (or the first tab) when it has none.
///
/// Answers whether a split came on screen, which changes the focused tile's
/// size: a caller with no write in hand owes the resize (`adopted_resize`).
fn ensure_layout(state: &mut State) -> bool {
    let Some(space) = state.space else {
        return false;
    };
    if state.layouts.contains_key(&space) {
        return false;
    }
    // The focused terminal even when it is a pane: `activate` brings its tab.
    let seed = state
        .rows
        .iter()
        .find(|r| r.terminal == state.focused && r.workspace == space)
        .map(|r| r.terminal)
        .or_else(|| state.tabs().first().map(|r| r.terminal))
        .unwrap_or(0);
    if seed == 0 {
        return false;
    }
    activate(state, space, seed);
    state
        .layouts
        .get(&space)
        .is_some_and(|l| l.leaf_count() > 1)
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
        && let Some(bytes) = move_focus(state, next, control_seq, seqs)?
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
        if let Some(resize) = size_terminals(state, seqs)? {
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
    if let Some(resize) = size_terminals(state, seqs)? {
        out.extend_from_slice(&resize);
    }
    Ok((true, if out.is_empty() { None } else { Some(out) }))
}

/// The onlookers' screens outside `keep` move to `last_seen` instead of being dropped, so a
/// tab come back to shows its last screen while the fresh one is on its way.
fn park_screens(state: &mut State, keep: &std::collections::BTreeSet<u32>) {
    let (kept, left): (
        std::collections::BTreeMap<_, _>,
        std::collections::BTreeMap<_, _>,
    ) = std::mem::take(&mut state.snapshots)
        .into_iter()
        .partition(|(t, _)| keep.contains(t));
    state.snapshots = kept;
    state.last_seen.extend(left);
}

/// The focused screen and every onlooker's go to `last_seen`: the client stops watching
/// all of them, and any may be come back to.
fn park_all(state: &mut State) {
    if state.focused != 0
        && let Some(screen) = state.snapshot.take()
    {
        state.last_seen.insert(state.focused, screen);
    }
    park_screens(state, &std::collections::BTreeSet::new());
}

fn sync_subs(state: &mut State, control_seq: &mut u32) -> io::Result<Option<BytesMut>> {
    let want: std::collections::BTreeSet<u32> = visible_set(state).into_iter().collect();
    // A visible terminal with no screen here is asked again even when already subscribed:
    // the daemon answers a repeat with its live screen and no second watcher. Otherwise a
    // pane that dropped its copy (a space left and come back to) stays blank until the
    // terminal prints.
    let blank = |t: &u32| {
        if *t == state.focused {
            state.snapshot.is_none()
        } else {
            !state.snapshots.contains_key(t)
        }
    };
    let drop: Vec<u32> = state.subs.difference(&want).copied().collect();
    let add: Vec<u32> = want
        .iter()
        .copied()
        .filter(|t| !state.subs.contains(t) || blank(t))
        .collect();
    if drop.is_empty() && add.is_empty() {
        return Ok(None);
    }
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

/// Size every terminal this interface lays out to the rectangle it is drawn in, that is:
/// the tiles on screen, every parked tab and every other space's tree laid out
/// in the same pane, and any other live terminal as a whole-pane tab. A child that is
/// told its real width formats for it, whether it is on screen now or when its tab comes
/// back.
///
/// A terminal is sent its size only when it changed: to the daemon an equal size is a
/// repaint nudge, one row short for 30 ms. That covers an attach too: the first size a
/// run sends a terminal always goes out, and when it equals the one the terminal already
/// has, the daemon nudges it. Forcing a second one on adoption shrank the child a row for
/// 30 ms after it had been sized, and a shell asked its size in that window read the
/// wrong one. A pane hidden by zoom keeps its last size. A terminal
/// another client owns is refused by the daemon and stays viewport-cropped (ADR-007).
fn size_terminals(
    state: &mut State,
    seqs: &mut std::collections::BTreeMap<u32, u32>,
) -> io::Result<Option<BytesMut>> {
    let area = pane_area(state);
    let size = |r: layout::Rect| (r.cols as u16, r.rows as u16);
    let asleep: std::collections::BTreeSet<u32> = state
        .rows
        .iter()
        .filter(|row| row.state == "sleeping")
        .map(|row| row.terminal)
        .collect();
    let mut want: std::collections::BTreeMap<u32, (u16, u16)> = state
        .rows
        .iter()
        .filter(|row| !asleep.contains(&row.terminal))
        .map(|row| (row.terminal, size(area)))
        .collect();
    let mut zoomed_away = std::collections::BTreeSet::new();
    // Parked first, so the tree on screen is what a terminal in both is sized by.
    for tree in state
        .parked
        .values()
        .flatten()
        .chain(state.layouts.values())
    {
        let tiles = tree.tile_rects(area);
        for leaf in tree.leaves() {
            if !tiles.iter().any(|(t, _)| *t == leaf) {
                want.remove(&leaf);
                zoomed_away.insert(leaf);
            }
        }
        want.extend(tiles.into_iter().map(|(t, r)| (t, size(r))));
    }
    want.retain(|t, _| !asleep.contains(t));
    // Off screen and left for later: everything while a divider or the panel is being
    // dragged, so a hundred terminals do not reflow on every frame of a drag (the paint
    // after the release sizes them); and another machine's terminals, whose geometry is
    // for a person at that machine to take until one here looks at them.
    let visible: std::collections::BTreeSet<u32> = visible_set(state)
        .into_iter()
        .chain([state.focused])
        .collect();
    let dragging = state.sidebar_drag.is_some() || state.modes.split_drag.is_some();
    let later: std::collections::BTreeSet<u32> = want
        .keys()
        .copied()
        .filter(|t| !visible.contains(t) && (dragging || crate::mux::is_remote_terminal(*t)))
        .collect();
    want.retain(|t, _| !later.contains(t));
    if state.focused != 0 {
        want.insert(state.focused, size(focused_rect(state)));
    }
    let mut out = BytesMut::new();
    for (&terminal, &(cols, rows)) in &want {
        if cols == 0 || rows == 0 || state.sized.get(&terminal) == Some(&(cols, rows)) {
            continue;
        }
        out.extend_from_slice(&client::encode_command_to(
            terminal,
            &ClientWire::Resize { cols, rows },
            seqs.entry(terminal).or_default(),
        )?);
    }
    state
        .sized
        .retain(|t, _| want.contains_key(t) || zoomed_away.contains(t) || later.contains(t));
    state.sized.extend(want);
    Ok((!out.is_empty()).then_some(out))
}

fn focus(
    state: &mut State,
    to: u32,
    control_seq: &mut u32,
    seqs: &mut std::collections::BTreeMap<u32, u32>,
) -> io::Result<Option<BytesMut>> {
    forget_return(state);
    move_focus(state, to, control_seq, seqs)
}

/// The person put the keyboard somewhere themselves: a focused tab that went away is no
/// longer waited for. Only that wait: one for a terminal asked for by name stays.
fn forget_return(state: &mut State) {
    if matches!(state.show_when_listed, Some((_, _, true))) {
        state.show_when_listed = None;
    }
}

/// Move the keyboard to `to`, for whoever chose it: `focus` for the person, a deferred
/// focus for what the interface picked on its own.
fn move_focus(
    state: &mut State,
    to: u32,
    control_seq: &mut u32,
    seqs: &mut std::collections::BTreeMap<u32, u32>,
) -> io::Result<Option<BytesMut>> {
    if to == 0 || to == state.focused {
        return Ok(None);
    }
    // The keyboard is in a live tab now, not resting on a sleeping one.
    state.dim = 0;
    let from = state.focused;
    let from_wants = wants_focus_events(state.snapshot.as_ref());
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
    // A tab outside the tree on screen brings its own tree, and the one it
    // replaces is parked for the way back.
    if let Some(space) = state.space {
        activate(state, space, to);
    }
    // Promote the already-visible snapshot instead of waiting for the next
    // render: the subscription never dropped, so the picture is fresh.
    if let Some(snap) = state.snapshots.remove(&to) {
        if let Some(old) = state.snapshot.take() {
            state.snapshots.insert(from, old);
        }
        state.snapshot = Some(snap);
        state.previewing = false;
    } else {
        // Another terminal's screen, and another terminal's history. Both are
        // dropped rather than carried, or the new tab opens scrolled into rows
        // that are not its own.
        if let Some(old) = state.snapshot.take() {
            state.snapshots.insert(from, old);
        }
        state.forget_screen();
        // Its last seen screen, if any, stands in until the daemon's answer lands: only
        // drawn, so nothing above (history, selection, focus reports) reads it.
        state.previewing = true;
    }
    state.anchor = None;
    state.behind = Behind::default();
    state.selection = None;
    // Copy mode and a close waiting for its answer belong to the terminal they were
    // opened over.
    state.modes.copy = None;
    state.modes.confirm_close = None;
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
    // Out of the onlookers: a snapshot kept past its unsubscription is a screen that can
    // never update again, so it is only last seen from here on.
    let visible: std::collections::BTreeSet<u32> = visible_set(state).into_iter().collect();
    park_screens(state, &visible);
    let mut out = BytesMut::new();
    if let Some(sync) = sync_subs(state, control_seq)? {
        out.extend_from_slice(&sync);
    }
    // The keyboard moved: the pane it left hears it lost the focus and the one it came to
    // hears it gained it, each only if its program asked, the way tmux tells its panes.
    // A pane whose screen this client has not seen yet cannot be asked, and is not told.
    for (terminal, wants, report) in [
        (from, from_wants, b"\x1b[O"),
        (to, wants_focus_events(state.snapshot.as_ref()), b"\x1b[I"),
    ] {
        if wants && terminal != 0 {
            out.extend_from_slice(&client::encode_command_to(
                terminal,
                &ClientWire::Input {
                    bytes: report.to_vec(),
                    from_terminal: None,
                    from_token: None,
                },
                seqs.entry(terminal).or_default(),
            )?);
        }
    }
    if let Some(resize) = size_terminals(state, seqs)? {
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

/// Whether the program on this screen asked to hear about focus (`CSI ? 1004 h`).
fn wants_focus_events(snapshot: Option<&SnapshotWire>) -> bool {
    snapshot.is_some_and(|s| s.modes.focus_events)
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
    forget_return(state);
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
            if let Some(resize) = size_terminals(state, seqs)? {
                out.extend_from_slice(&resize);
            }
            Ok(Some(out))
        }
        Some(first) => focus(state, first, control_seq, seqs),
        // Asleep: the keyboard rests on its tabs, and waking is a key away.
        None if state.asleep(space) => unfocus(state, &asleep_hint(), control_seq),
        None => unfocus(
            state,
            &format!(
                "this space is empty, {} c starts a terminal",
                crate::keys::prefix_name()
            ),
            control_seq,
        ),
    }
}

/// What the hint row says while the keyboard rests on a sleeping tab.
fn asleep_hint() -> String {
    crate::keys::named(
        "asleep: enter or ctrl-a Z wakes this tab, ctrl-a n/p picks a tab, ctrl-a x discards it",
    )
}

/// Put the keyboard on a sleeping tab without waking it: its space comes on screen with
/// that tab showing its last screen, and the wake stays a key away. In a space with tabs
/// awake the keyboard is taken back out of the one entering put it in.
fn peek(
    state: &mut State,
    space: i64,
    tab: u32,
    control_seq: &mut u32,
    seqs: &mut std::collections::BTreeMap<u32, u32>,
) -> io::Result<Option<BytesMut>> {
    state.dim = tab;
    let mut out = enter_space(state, space, control_seq, seqs)?;
    if state.focused != 0 {
        state.dim = tab;
        if let Some(more) = unfocus(state, &asleep_hint(), control_seq)? {
            out.get_or_insert_with(BytesMut::new)
                .extend_from_slice(&more);
        }
    }
    Ok(out)
}

/// Take the keyboard out of every terminal, because this space has none to put it in.
///
/// The unsubscribe is the point. Leaving the old terminal focused kept every keystroke
/// going to a terminal in a space the person had left and was not looking at, kept its
/// output painting under this space's tab bar, and left `ctrl-a x` closing it. Nothing on
/// screen said any of that was happening.
fn unfocus(state: &mut State, why: &str, control_seq: &mut u32) -> io::Result<Option<BytesMut>> {
    park_all(state);
    state.focused = 0;
    state.forget_screen();
    state.modes.copy = None;
    state.modes.confirm_close = None;
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

/// Open the list an answer about another machine brings (its sessions, or the offer to
/// install qoro there), but only as the answer to the question still standing and only
/// over nothing else. An answer can take seconds, and a list that opened over a box the
/// person has moved on to, or one too big to be painted, would still take their keys.
fn open_answer_list(state: &mut State, id: &str, kind: MenuKind) {
    if state.probing.as_deref() != Some(id) {
        return;
    }
    state.probing = None;
    let target = match &kind {
        MenuKind::Install { target, .. } => target.clone(),
        MenuKind::Main | MenuKind::Plus | MenuKind::New { .. } | MenuKind::Agents { .. } => {
            String::new()
        }
    };
    let fits = crate::screen::menu_rect(
        state.cols,
        state.lines,
        &menu_rows(&kind),
        &menu_title(&kind),
    )
    .is_some();
    let busy = state.menu.is_some()
        || state.prompt.is_some()
        || state.palette.is_some()
        || state.reading.is_some()
        || state.focus;
    if !fits || busy {
        state.message = Some(if fits {
            format!("{target} answered while a box was open; connect again to pick")
        } else {
            format!("the window is too small for what {target} answered")
        });
        return;
    }
    state.message = None;
    state.menu_kind = kind;
    state.menu = Some(0);
}

/// Fold one answer into what is on screen.
fn apply(state: &mut State, response: ControlResponse, request: &mut u64, dirty: &mut bool) {
    if response.request_id() == "behind" {
        state.modes.refetching = false;
    }
    match response {
        // Rows for a copy, gathered on their own and never into the view's window: the
        // window is what is drawn, and a copy's rows are not what the person is looking at.
        ControlResponse::Scrollback {
            id,
            terminal,
            from,
            total,
            lines,
        } if id.starts_with("yank-") => {
            let Some(yank) = state
                .modes
                .yank
                .as_mut()
                .filter(|yank| yank.terminal == terminal)
            else {
                return;
            };
            let next = if yank.take(from, total, lines) {
                yank.next_request()
            } else {
                None
            };
            match next {
                Some(ask) => state.modes.outbox.push(ask),
                None => {
                    state.modes.clipboard = state.modes.yank.take().map(|yank| yank.text());
                }
            }
        }
        ControlResponse::ScrollbackMatch {
            id,
            terminal,
            row,
            col,
            resume,
        } => {
            if terminal == state.focused {
                copy_found(state, &id, row, col, resume);
                *dirty = true;
            }
        }
        // An older daemon cannot read the search and says so with no id, since it could
        // not read the id either. The one search waiting is what it is answering.
        ControlResponse::Failed { id, reason }
            if id.is_empty()
                && reason.contains("unknown variant")
                && state
                    .modes
                    .copy
                    .as_ref()
                    .and_then(|copy| copy.search.as_ref())
                    .is_some_and(|search| search.waiting) =>
        {
            if let Some(copy) = state.modes.copy.as_mut() {
                copy.search = None;
                copy.status = "search needs a newer daemon: stop the session to upgrade".into();
            }
            *dirty = true;
        }
        ControlResponse::Failed { id, reason }
            if id.starts_with("search-") || id.starts_with("yank-") =>
        {
            if id.starts_with("yank-") {
                state.modes.yank = None;
                state.message = Some(reason);
            } else if let Some(copy) = state.modes.copy.as_mut() {
                copy.search = None;
                copy.status = reason;
            }
            *dirty = true;
        }
        ControlResponse::Scrollback {
            id,
            terminal,
            lines,
            ..
        } if id.starts_with("peek-") => {
            // Only the tab still being looked at: an answer that outlived a wake, or a
            // move to another tab, is for a screen nobody is showing.
            if state
                .modes
                .peek_asked
                .is_some_and(|(tab, _)| tab == terminal)
            {
                let (cols, rows) = pane_cells(state);
                state.modes.peek =
                    Some((terminal, peek_screen(lines, cols as usize, rows as usize)));
                *dirty = true;
            }
        }
        // A sleeping tab whose screen cannot be read is shown as an empty one, not as an
        // error on the notice row over a tab nobody asked to open.
        ControlResponse::Failed { id, .. } if id.starts_with("peek-") => {
            let tab = id["peek-".len()..].parse::<u32>().ok();
            if tab.is_some() && state.modes.peek_asked.map(|(t, _)| t) == tab {
                let (cols, rows) = pane_cells(state);
                state.modes.peek =
                    tab.map(|tab| (tab, peek_screen(Vec::new(), cols as usize, rows as usize)));
                *dirty = true;
            }
        }
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
                // The first render and the index may both have come before
                // this, with no space to build a tree in: build it now, or a
                // restored split tab shows one pane until the next index.
                if state.focused != 0 && ensure_layout(state) {
                    state.adopted_resize = true;
                }
            }
            // A space folds the moment it falls asleep, or first shows up asleep, and opens
            // when it wakes: only the edge moves the fold, so a sleeping space a person
            // opened again stays open.
            let was = |id: i64| state.spaces.iter().find(|w| w.id == id).map(|w| w.asleep);
            for space in &workspaces {
                match (was(space.id), space.asleep) {
                    (None | Some(false), true) => {
                        state.shut.insert(space.id);
                    }
                    (Some(true), false) => {
                        state.shut.remove(&space.id);
                    }
                    _ => {}
                }
            }
            let slept_here = state.space.is_some_and(|here| was(here) == Some(false))
                && workspaces
                    .iter()
                    .any(|w| Some(w.id) == state.space && w.asleep);
            state.spaces = workspaces;
            // A fold outlives nothing it folded: a space that went away takes its fold
            // with it, so a later space reusing the id does not start folded.
            let live: sidebar::Shut = state.spaces.iter().map(|s| s.id).collect();
            state.shut.retain(|id| live.contains(id));
            // The space on screen went to sleep: the screen moves on to the next awake
            // space and the keyboard to its first tab. With none awake anywhere there is
            // nothing to move to, and the interface detaches instead.
            if slept_here && let Some(next) = state.next_space(true) {
                state.space = Some(next);
                state.pending_focus = state.tabs().first().map(|r| r.terminal);
            }
            // The list's own counts too: this answer can land before the first index, with
            // no row known yet, and an interface that just opened must not leave for that.
            state.done =
                nothing_awake(state) && state.spaces.iter().all(|w| w.asleep || w.terminals == 0);
            // A sleeping space's emptiness is this answer's news and not the index's, so a
            // close waiting on one is asked here as well.
            close_gate(state, request);
            *dirty = true;
        }
        // A daemon older than this interface sleeps the whole space whatever was asked, and
        // does not say which tabs changed: said, so the person knows why every tab went.
        ControlResponse::WorkspaceAsleep {
            id, changed: None, ..
        } if id.starts_with("sleep-tab-") => {
            state.message = None;
            state.hint = Some(
                "the running daemon is older: the whole space slept; restart it (qoro stop) \
                 to sleep one tab"
                    .to_string(),
            );
            *dirty = true;
        }
        ControlResponse::WorkspaceAsleep {
            workspace,
            terminals,
            stayed,
            ..
        } => {
            let tabs = if terminals == 1 { "tab" } else { "tabs" };
            state.message = Some(format!(
                "{terminals} {tabs} of {} asleep",
                space_name(state, workspace)
            ));
            // What stayed awake, and why, stays on the hint row like a refusal does.
            if !stayed.is_empty() {
                // Each reason names the panes of its tab and why.
                let why: Vec<String> = stayed.iter().map(|s| s.reason.clone()).collect();
                state.hint = Some(format!("stayed awake: {}", why.join("; ")));
            }
            *dirty = true;
        }
        ControlResponse::WorkspaceAwake {
            workspace,
            terminals,
            resumed,
            ..
        } => {
            state.message = Some(format!(
                "{terminals} tabs of {} back, {resumed} agents resumed",
                space_name(state, workspace)
            ));
            // The tab asked for goes on screen once the index lists it again.
            if let Some((space, to)) = state.waking
                && space == workspace
            {
                state.waking = None;
                state.show_when_listed = Some((to, std::time::Instant::now() + SHOW_WAIT, false));
            }
            *dirty = true;
        }
        // A space close the daemon refused. `ctrl-a W` asks for one directly and never sets
        // `closing`, and its refusal is the notice row it has always been: a new client and
        // an old daemon, a sleeping space whose tabs the client's own count cannot see, and
        // the session's default are all refusals a person has to read.
        //
        // A refusal for the close this client is running re-arms the gate, because the tabs
        // that caused it are on their way out and the next index will find the space empty.
        // A refusal with nothing left to wait for is the one that repeats, and the give-up
        // timer is what ends that rather than a loop.
        ControlResponse::Failed { id, reason } if id.starts_with("close-space-") => {
            let ours = state
                .closing
                .as_ref()
                .filter(|c| c.space_asked_id.as_deref() == Some(id.as_str()))
                .map(|c| c.space);
            match ours {
                Some(space) => {
                    let left = tabs_left(state, space);
                    if let Some(closing) = state.closing.as_mut() {
                        closing.space_asked = false;
                        closing.space_asked_id = None;
                    }
                    state.hint = Some(if left == 0 {
                        reason
                    } else {
                        format!("{reason}; {left} {} still there", plural(left))
                    });
                }
                None => {
                    // Both rows, like the tab refusal below: a running agent's render
                    // wipes the notice row without asking, and a refusal that needs
                    // reading is the one message that must not vanish on contact.
                    state.hint = Some(reason.clone());
                    state.message = Some(reason);
                }
            }
            *dirty = true;
        }
        // A tab close refused: its command channel was full, which is the one refusal a
        // close can make, and which the next attempt usually gets through. The space is
        // not closed over a tab that is still there, and the refusal is asked again rather
        // than ending the chain on the first one.
        //
        // Both rows, ours and anybody else's. The notice row is the one a single tab's
        // refusal has always had, and the hint row is where it survives: a pane with a
        // running agent renders over the notice row without asking, so a refusal that
        // needs reading would vanish at the moment the person reached for the keyboard.
        ControlResponse::Failed { id, reason } if id.starts_with("close-") => {
            // Which tab this refusal is about, and whether it is ours at all: a refused
            // `ctrl-a x` on a single tab is somebody else's close, and it keeps the notice
            // row it has always had, because a hint is wiped by the next keystroke and a
            // refusal that needs reading is the one message that must not vanish on contact.
            let ours = state.closing.as_mut().and_then(|c| {
                let at = c.pending.iter().position(|(asked, _)| *asked == id)?;
                let (_, terminal) = c.pending.remove(at);
                // The budget is one tab's, not the chain's. A tab the chain has given up
                // on never closes, so without this every tab behind it would inherit an
                // allowance of zero and be abandoned on its first refusal, which is the
                // case the budget exists for.
                if c.stuck != Some(terminal) {
                    c.stuck_tries = 0;
                }
                c.stuck = Some(terminal);
                Some(terminal)
            });
            // Ours asks the tab again and says what is left of the space on the hint row
            // below; somebody else's stays the ordinary refusal of one tab, on the notice
            // row it has always had. The hint is written either way, so neither reason can
            // be wiped by a pane's own render.
            state.hint = Some(reason.clone());
            if ours.is_some() {
                close_one(state, request);
            } else {
                state.message = Some(reason.clone());
            }
            *dirty = true;
        }
        // A sleep or wake the daemon refused: the reason names the tab and why, and it stays
        // on the hint row until the person does something, rather than one frame.
        // What came of putting a newer qoro on a remote, said by the mux unasked: the same
        // row as its waiting line, for the same reason.
        ControlResponse::Failed { id, reason } if id == crate::mux::UPDATE_NOTICE => {
            state.hint = Some(reason);
            *dirty = true;
        }
        // A name given at start, put on the conversation unasked: nobody pressed anything,
        // so a refusal (a remote too old to name a session) is not said. The tab keeps it.
        ControlResponse::Failed { id, .. } if id.starts_with("title-") => {}
        ControlResponse::Failed { id, reason }
            if id.starts_with("sleep-") || id.starts_with("wake-") =>
        {
            if id.starts_with("wake-tab-") {
                state.waking = None;
            }
            state.message = None;
            state.hint = Some(reason);
            *dirty = true;
        }
        ControlResponse::WorkspaceCreated { workspace, .. } => {
            state.creating = state.creating.saturating_sub(1);
            state.space = Some(workspace);
            // The old space's screens are kept as last seen, for the way back.
            park_all(state);
            state.forget_screen();
            // A new space starts bare: no tree. The old space's subscriptions
            // stay in `subs` until the next sync drops them, since `apply`
            // cannot write: forgetting them here meant the unsubscribe never
            // went out, the daemon kept them, and coming back subscribed to a
            // terminal it still watched, which asks it for no screen. The
            // render filter drops the old space's renders by space meanwhile.
            state.layouts.remove(&workspace);
            state.parked.remove(&workspace);
            // Asked for here rather than by the caller, because a space with no terminal in
            // it shows an empty pane and a bar with only a `+` on it, and the first thing
            // anybody does in a new space is start a terminal.
            state.wants_terminal_in.push(workspace);
            *dirty = true;
        }
        ControlResponse::WorkspaceClosed { workspace, .. } => {
            // The close this client asked for, answered, and only that one. The answer
            // arrives for any space that went, including one another client or the mux
            // reconciled away, and clearing our close on someone else's would strand the
            // tabs still on their way out of ours.
            if state.closing.as_ref().is_some_and(|c| c.space == workspace) {
                state.closing = None;
            }
            if state.armed == Some(workspace) {
                state.armed = None;
            }
            // Its drawer too: ids are given again, and a new space must not open with it out.
            if state.drawer.is_some_and(|d| d.space == workspace) {
                state.drawer = None;
            }
            // Its tree goes with it: a layout for a space that no longer exists would
            // reseed nothing and leak the entry. The space itself moves to whatever the
            // next list answers: `None` reads as "pick the first" on that path.
            state.layouts.remove(&workspace);
            state.parked.remove(&workspace);
            if state.space == Some(workspace) {
                state.space = None;
                state.forget_screen();
            }
            *dirty = true;
        }
        ControlResponse::Agents { agents, splits, .. } => {
            // First, so the bootstrap below already knows which rows are panes.
            adopt_splits(state, &agents, splits);
            // Back where the person left: the first index names what is still there, and
            // `focus` brings its space and, for a pane, its tab. The bootstrap below
            // adopts in that space, so the first frame is already the right one.
            if let Some(row) = state
                .last
                .resume
                .take()
                .and_then(|t| agents.iter().find(|r| r.terminal == t))
            {
                state.space = Some(row.workspace);
                state.pending_focus = Some(row.terminal);
            }
            if state.focused == 0 && !state.adopted {
                // Only before this client has ever had a terminal. The daemon subscribes a
                // fresh client to what is already open, so this is the bootstrap and no
                // subscription of our own is needed. Later, nothing focused is a space with
                // nothing in it, and reaching into another space's terminal here is how a
                // person ends up typing somewhere they cannot see. Without a space yet
                // the first workspace wins, the way the workspace list and the first
                // render both pick it.
                // A tab, not a pane inside one: the pane's tab brings it on screen.
                let first = match state.space {
                    Some(s) => agents
                        .iter()
                        .find(|r| r.workspace == s && !state.is_pane(r.terminal)),
                    None => {
                        let min = agents.iter().map(|r| r.workspace).min();
                        agents
                            .iter()
                            .find(|r| Some(r.workspace) == min && !state.is_pane(r.terminal))
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
            prune_trees(state, |t| !agents.iter().any(|r| r.terminal == t));
            prune_moved(state, &agents);
            // TerminalClosed and Agents prune the local set silently: the
            // daemon ended the terminal, which ends its subscription
            // server-side too, so there is nothing to unsubscribe from. The
            // assumption is one-directional on purpose: pruning an id the
            // daemon still streams would resurface as a render for an unknown
            // terminal, which the filter above drops.
            let live: std::collections::BTreeSet<u32> = agents.iter().map(|r| r.terminal).collect();
            state.snapshots.retain(|t, _| live.contains(t));
            state.last_seen.retain(|t, _| live.contains(t));
            state.subs.retain(|t| live.contains(t));
            // Pictures too, up to 4 MiB each, or every terminal that exited on its own
            // kept its last one for the life of this client.
            state.images.retain(|t, _| live.contains(t));
            // And what each was last sized to: one that comes back (a remote daemon that
            // restarted while its link was down restores its tabs at 80x24) has to be
            // sized again, and an equal cached size would send it nothing.
            state.sized.retain(|t, _| live.contains(t));
            if state.focused != 0 && !agents.iter().any(|r| r.terminal == state.focused) {
                // Kept for a while in case it comes back under the same number, which a
                // remote daemon moving onto a newer qoro does. One that is gone for good is
                // never listed again and is forgotten. A terminal asked for by name is an
                // ask of the person's own, and stays the one waited for; an earlier tab of
                // theirs that went (one they closed) gives way to this one.
                if !matches!(state.show_when_listed, Some((_, _, false))) {
                    state.show_when_listed = Some((
                        state.focused,
                        std::time::Instant::now() + FOCUS_RETURN,
                        true,
                    ));
                }
                state.focused = 0;
                state.forget_screen();
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
                // Only sleeping tabs left is not empty: the space list says so next.
                let dozing = state
                    .space
                    .and_then(|space| state.spaces.iter().find(|w| w.id == space))
                    .is_some_and(|w| !w.sleeping.is_empty());
                if state.pending_focus.is_none() && !dozing {
                    state.message = Some(format!(
                        "this space is empty, {} c starts a terminal",
                        crate::keys::prefix_name()
                    ));
                }
            }
            // A machine just connected from here has its first tab listed: the screen goes
            // there, which is what the person connected it for.
            if let Some(name) = state.joining.clone() {
                let prefix = format!("{name}:");
                let arrived = agents.iter().find(|r| {
                    r.state != "sleeping"
                        && !state.is_pane(r.terminal)
                        && state
                            .spaces
                            .iter()
                            .any(|w| w.id == r.workspace && w.name.starts_with(&prefix))
                });
                if let Some(row) = arrived {
                    // The machine the person just connected: where they asked to go.
                    forget_return(state);
                    state.joining = None;
                    state.space = Some(row.workspace);
                    state.pending_focus = Some(row.terminal);
                    state.message = None;
                }
            }
            // Nothing focused, nothing on its way, and the space on screen has a tab: that
            // tab, through the subscribing path. This is what brings the keyboard back to a
            // remote space when its host reconnects, and a tab that appears in the empty
            // space a person is looking at is theirs to see. A sleeping row has no process
            // behind it to type into.
            // Nor while the keyboard rests on a sleeping tab a person picked: that is where
            // they are, with tabs awake beside it or not.
            let peeking = state.dim != 0 && state.dim_tab() == Some(state.dim);
            if state.adopted
                && state.focused == 0
                && state.pending_focus.is_none()
                && !peeking
                && let Some(space) = state.space
            {
                state.pending_focus = agents
                    .iter()
                    .find(|r| {
                        r.workspace == space && !state.is_pane(r.terminal) && r.state != "sleeping"
                    })
                    .map(|r| r.terminal);
            }

            // A terminal that has just started needing a person. The bell is the local
            // notification: it is what the person's own terminal emulator turns into a
            // badge or a bounce, and it is the one channel that works with no account, no
            // daemon of somebody else's, and no network.
            for row in &agents {
                let before = state.was.get(&row.terminal).cloned();
                let wants_someone = matches!(row.state.as_str(), "blocked" | "failed" | "done");
                if wants_someone && before.as_ref().is_some_and(|b| *b != row.state) {
                    let runtime = match row.state.as_str() {
                        "blocked" => AgentRuntimeState::Blocked,
                        "failed" => AgentRuntimeState::Failed,
                        _ => AgentRuntimeState::Done,
                    };
                    // The desktop says it instead when it can, a second later; the bell
                    // stays quiet so a terminal that turns bells into notes says it once.
                    if !note(state, row, runtime) {
                        state.ring = true;
                    }
                    state.message = Some(format!("{} is {}", sidebar::label(row), row.state));
                }
                // A screen agent that worked and now sits idle while the
                // keyboard is elsewhere finished without ever reporting done.
                // Same news as a daemon done, same bell, same `new` row.
                // Working only: `starting` is a session opening (a new tab, a
                // restore, `/clear`), and one that settles to idle did nothing.
                let was_working = before.as_deref() == Some("working");
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
                        if !note(state, row, AgentRuntimeState::Done) {
                            state.ring = true;
                        }
                        state.message = Some(format!("{} finished", sidebar::label(row)));
                    }
                }
                // The one under the keyboard, finishing while the person is in another
                // window: nothing on this screen changes for them, so only the desktop
                // can say it.
                if was_working && row.state == "idle" && row.terminal == state.focused {
                    note(state, row, AgentRuntimeState::Done);
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
            name_sessions(state, request);
            // A space being closed waits for this: the daemon counts store rows and the
            // index reads the live registry, so the index showing a space empty is the
            // earliest the space can be asked to close, and asking earlier is a refusal
            // that names tabs which are on their way out anyway.
            close_gate(state, request);
            // Nothing left anywhere. The interface is a way into terminals, so with none of
            // them it is a screen a person has to know a key to escape from. Ending is what
            // every multiplexer does when its last window closes, and the daemon stays: it
            // is the client that goes. A remote that is connecting or offline is still
            // somewhere to be: its space is listed, and it comes back on its own.
            state.done = nothing_awake(state);
            // After the first-tab fallback above, and over it: a focused tab that comes back
            // takes the keyboard back from the first tab of its space.
            if let Some((terminal, until, returning)) = state.show_when_listed.take()
                && std::time::Instant::now() < until
            {
                if state.rows.iter().any(|r| r.terminal == terminal) {
                    state.pending_focus = Some(terminal);
                } else {
                    state.show_when_listed = Some((terminal, until, returning));
                }
            }
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
            if state.focused != 0 && ensure_layout(state) {
                state.adopted_resize = true;
            }
            *dirty = true;
        }
        // The daemon moved a tab this client dragged. The index says so too, a moment
        // later; the row changes here so the keyboard can follow it now.
        ControlResponse::TerminalMoved {
            id,
            terminal,
            workspace,
        } => {
            // A reorder: the tab stays in its space, its split and the keyboard with it. The
            // index the daemon announces redraws the order.
            // A pane taken out of its split: the daemon took it out, and this client's tree
            // lets it go too, whichever pane of the split it was. The keyboard on it follows it
            // to its own tab, never left on a terminal no longer on screen.
            if id.starts_with(LEAVE) {
                prune_trees(state, |t| t == terminal);
                if state.focused == terminal {
                    state.space = Some(workspace);
                    activate(state, workspace, terminal);
                    state.adopted_resize = true;
                }
                state.message = Some("a tab of its own now".to_string());
                *dirty = true;
                return;
            }
            if id.starts_with(REORDER) {
                state.message = Some("reordered".to_string());
                *dirty = true;
                return;
            }
            if let Some(row) = state.rows.iter_mut().find(|r| r.terminal == terminal) {
                row.workspace = workspace;
            }
            let beside = state
                .drop_after_move
                .take_if(|(asked, ..)| *asked == id)
                .filter(|(_, t, ..)| *t == terminal)
                .and_then(|(_, _, pane, side)| {
                    let mut tree = state.layouts.get(&workspace)?.clone();
                    tree.insert_beside(pane, terminal, side, pane_area(state))
                        .then_some(tree)
                });
            match beside {
                Some(tree) => place(state, workspace, terminal, tree),
                // A tab of its own there, and the keyboard follows it.
                None => {
                    prune_trees(state, |t| t == terminal);
                    if state.focused == terminal {
                        state.space = Some(workspace);
                        activate(state, workspace, terminal);
                        state.adopted_resize = true;
                    } else {
                        state.pending_focus = Some(terminal);
                    }
                }
            }
            state.message = Some(format!("moved to {}", space_name(state, workspace)));
            *dirty = true;
        }
        ControlResponse::TerminalCreated { id, terminal, .. } => {
            // Split panes bind by id correlation: only a `split-` answer may
            // claim a placeholder, and it claims the one in the space the
            // request was made in. A racing `new-` tab must not steal it, and
            // a split answer landing after a space switch binds home rather
            // than stranding a foreign terminal in the tree on screen.
            let is_split = id.starts_with("split-");
            let home = if is_split {
                split_home(state, &id)
            } else {
                None
            };
            // A person split this tab: that is worth keeping.
            let written = home.and_then(|(space, at)| {
                let layout = tree_at(state, space, at)?;
                layout.bind_placeholder(terminal).then(|| layout.to_pane())
            });
            let bound = written.is_some();
            state.split_writes.extend(written.flatten());
            if !is_split {
                // The caller subscribes: a new terminal that the client is not
                // watching draws the terminal it came from, which is what the
                // first version did. The screen it came from is kept as last seen.
                if state.focused != 0
                    && let Some(screen) = state.snapshot.take()
                {
                    state.last_seen.insert(state.focused, screen);
                }
                state.forget_screen();
            }
            // An unbound split stays where the keyboard is: yanking the focus
            // to a terminal in another space would collapse that space's tree
            // onto one pane. So does one bound into a parked tree: the person
            // has moved to another tab since.
            let on_screen =
                bound && matches!(home, Some((space, None)) if state.space == Some(space));
            if state.elsewhere.as_deref() == Some(id.as_str()) {
                state.elsewhere = None;
                state.show_when_listed =
                    Some((terminal, std::time::Instant::now() + SHOW_WAIT, false));
            } else if on_screen || !is_split {
                // A tab the person opened, and the keyboard goes to it: a focused tab that
                // went away no longer takes it back.
                forget_return(state);
                state.pending_focus = Some(terminal);
            }
            // An agent tab's shell is up: queue its command line. The shell holds
            // the line as typeahead, so it lands even before its first prompt. A name
            // given to it waits for its conversation.
            if let Some((line, name)) = take_agent_command(state, &id) {
                if let Some(name) = name {
                    state.session_titles.insert(terminal, name);
                }
                state.agent_typing.push((terminal, line));
            }
            *dirty = true;
        }
        ControlResponse::TerminalClosed { terminal, .. } => {
            state.session_titles.remove(&terminal);
            // One answer, one next ask: a space being closed sends its tabs one at a
            // time, so the close of the space cannot be reordered ahead of them.
            if let Some(closing) = state.closing.clone()
                && closing.sent > 0
                && closing.asked.get(closing.sent - 1) == Some(&terminal)
            {
                if let Some(closing) = state.closing.as_mut() {
                    closing.pending.retain(|(_, t)| *t != terminal);
                    // A retry that worked ends the trouble: the tab is closed, and asking
                    // for it again would be asking the daemon to refuse.
                    if closing.stuck == Some(terminal) {
                        closing.stuck = None;
                        closing.stuck_tries = 0;
                    }
                }
                close_one(state, request);
            }
            // Out of every tree, so no space keeps a pane nobody can focus.
            // Trees left with no leaves and no pending split go away, and the
            // unsubscription of a tile nobody watches piggybacks on the next
            // sync: the daemon already ended the terminal itself.
            prune_trees(state, |t| t == terminal);
            state.snapshots.remove(&terminal);
            state.last_seen.remove(&terminal);
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
                state.forget_screen();
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
                    state.message = Some(format!(
                        "this space is empty, {} c starts a terminal",
                        crate::keys::prefix_name()
                    ));
                }
            }
            *dirty = true;
        }
        ControlResponse::Failed { id, reason, .. } => {
            if state.probing.as_deref() == Some(id.as_str()) {
                state.probing = None;
            }
            if state.installing.as_deref() == Some(id.as_str()) {
                state.installing = None;
            }
            // A space that was refused is not coming: it no longer counts toward the next
            // numbered name.
            if id.starts_with("space-") {
                state.creating = state.creating.saturating_sub(1);
            }
            // An agent tab that never opened types nothing: its line goes with it.
            // The refusal itself is shown below, like any other.
            take_agent_command(state, &id);
            if state.elsewhere.as_deref() == Some(id.as_str()) {
                state.elsewhere = None;
            }
            // A daemon older than this client, refusing the colours it was told. It has
            // nothing to do with them anyway: said once, silently, and never again.
            if id.is_empty() && reason.contains("`HostColors`") {
                state.host_refused = true;
                return;
            }
            // The same for the news this interface tells: an older daemon has no rows to
            // put it on.
            if id.is_empty() && reason.contains("`News`") {
                state.news_refused = true;
                return;
            }
            // A split whose spawn failed rolls its tree entry back where it
            // was asked for: one failure must not clear another space's
            // split still on its way, and a rollback on screen only would
            // leak the origin's placeholder forever.
            if id.starts_with("split-")
                && let Some((space, at)) = split_home(state, &id)
                && let Some(layout) = tree_at(state, space, at)
                && layout.drop_placeholder()
            {
                // Recorded as it stands now: a detach made while this split
                // was on its way could not be (a pending tree has no whole
                // shape to write), and the disk would still hold that pane.
                if let Some(tree) = layout.to_pane() {
                    state.split_writes.push(tree);
                }
                // A parked tree the rollback left with one pane is a tab again.
                prune_trees(state, |_| false);
            }
            // A tree that did not reach the disk is still on screen here, and
            // the next split or detach writes it again. The refusal is shown
            // below: a split that will not survive a restart is news. An
            // older daemon cannot read the request at all, and cannot say
            // whose refusal it is either, so an empty id counts as one.
            if id.starts_with("tree-") || id.is_empty() {
                state.split_saving = state.split_saving.saturating_sub(1);
            }
            // A move refused leaves the tab where it was, and the pane it was to go
            // beside without it.
            if state
                .drop_after_move
                .as_ref()
                .is_some_and(|(asked, ..)| *asked == id)
            {
                state.drop_after_move = None;
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
        ControlResponse::SplitSaved { .. } => {
            state.split_saving = state.split_saving.saturating_sub(1);
        }

        // No qoro on the machine being connected: a list offering to put it there. Opened only
        // as the answer to the question still standing, and only over nothing else: an answer
        // can take seconds, and a list that opened over a box the person has moved on to
        // would take their keys.
        ControlResponse::RemoteMissing {
            id,
            target,
            platform,
            root,
        } => {
            open_answer_list(
                state,
                &id,
                MenuKind::Install {
                    target,
                    platform,
                    root,
                },
            );
            *dirty = true;
        }
        // Said whatever the person is doing now, because it happened; the connection follows
        // only while they are still waiting for it.
        ControlResponse::RemoteInstalled {
            id,
            target,
            version,
        } => {
            state.message = Some(format!("installed qoro {version} on {target}"));
            if state.installing.as_deref() == Some(id.as_str()) {
                state.installing = None;
                state.reprobe = Some(target);
            }
            *dirty = true;
        }
        // A remote that will move onto a newer qoro once nothing there is working, and what
        // it waits on, or that it has moved: the mux words it, and says it once. On the
        // hint row, which stays until the person does something: the notice row is
        // cleared by the next render of the tab in front of them, which a working agent
        // sends many times a second, so a line said there was painted once and never read.
        ControlResponse::RestartArmed {
            waiting_on: Some(line),
            ..
        } => {
            state.hint = Some(line);
            *dirty = true;
        }
        ControlResponse::RemoteAdded { name, .. } => {
            state.connecting = None;
            state.message = Some(format!("connecting to {name}"));
            state.joining = Some(name);
            *dirty = true;
        }

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

/// A pointer position as a cell of the focused tile, pulled back to its nearest edge when
/// the pointer has left it. Only for a gesture whose press already went to the program:
/// a release with no press, or a drag begun on the sidebar, is never sent.
fn clamped_to_focused(state: &State, row: u16, col: u16) -> (usize, usize) {
    let tile = focused_rect(state);
    let first_col = screen::left_for(state.cols, state.collapsed, state.sidebar_width) + 1;
    let x = (col as usize)
        .saturating_sub(first_col)
        .saturating_sub(tile.x);
    let y = (row as usize)
        .saturating_sub(screen::TABS + 1)
        .saturating_sub(tile.y);
    (
        y.min(tile.rows.saturating_sub(1)),
        x.min(tile.cols.saturating_sub(1)),
    )
}

/// Where the input goes on after the `ESC [` sequence at `i`, or `None` when the read ends
/// inside it and the rest is still coming.
///
/// A control byte before the final one ends it unfinished and is read as itself: held on,
/// an `ESC [` that never finished swallowed Enter, esc and the detach after it.
fn skip_sequence(buf: &[u8], i: usize) -> Option<usize> {
    let at = buf[i + 2..]
        .iter()
        .position(|b| (0x40..=0x7e).contains(b) || *b < 0x20)?;
    Some(if buf[i + 2 + at] >= 0x40 {
        i + 2 + at + 1
    } else {
        i + 2 + at
    })
}

/// Typing into the keys overlay's search.
///
/// Printable text is the query and backspace rubs it out. A lone esc ends the search, and a
/// key after it in the same read closes the overlay, as a key does with no search open.
/// Enter closes it. A click closes it and the wheel or a drag does nothing. Any other
/// sequence is skipped whole and any other byte dropped. The prefix, `ctrl-q`, and a key
/// after an esc are handed back from there on, for the decoder to close the overlay
/// with, so `ctrl-a d` here closes the list and types nothing, as it always did.
fn keys_search(state: &mut State, bytes: &[u8]) -> Option<Vec<u8>> {
    let prefix = state.config.prefix;
    let mut buf = std::mem::take(&mut state.palette_pending);
    buf.extend_from_slice(bytes);
    let close = |state: &mut State| {
        state.reading = None;
        state.keys_query = None;
    };
    let mut i = 0;
    while i < buf.len() {
        let Some(query) = state.keys_query.as_mut() else {
            return Some(buf[i..].to_vec());
        };
        match buf[i] {
            0x1b => match buf.get(i + 1) {
                Some(b'[') if buf.get(i + 2) == Some(&b'<') => match crate::keys::report(&buf[i..])
                {
                    crate::keys::Report::Whole(used, action) => {
                        if matches!(action, Some(Action::Click { .. })) {
                            close(state);
                            return None;
                        }
                        i += used;
                    }
                    crate::keys::Report::Broken(used) => i += used,
                    crate::keys::Report::Partial => {
                        state.palette_pending = buf[i..].to_vec();
                        return None;
                    }
                },
                // An arrow, a function key: skipped whole, split ones held, the way the
                // palette skips them.
                Some(b'[') => match skip_sequence(&buf, i) {
                    Some(next) => i = next,
                    None => {
                        state.palette_pending = buf[i..].to_vec();
                        return None;
                    }
                },
                // Esc on its own, or with a key after it: the search is over.
                _ => {
                    state.keys_query = None;
                    i += 1;
                }
            },
            b'\r' | b'\n' => {
                close(state);
                return None;
            }
            0x7f | 0x08 => {
                query.pop();
                i += 1;
            }
            b if b == prefix || b == 0x11 => {
                state.keys_query = None;
                return Some(buf[i..].to_vec());
            }
            // The keys and what they do are ASCII, so nothing else could match.
            0x20..=0x7e => {
                query.push(buf[i] as char);
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

/// Whether a read closes an overlay that is only being read: a key or a click does. The
/// release of the click that chose it from the menu, or the pointer jittering with the
/// button still down, arrives after the overlay is up, and closing on those closed it
/// again before anyone saw it.
fn closes_reading(actions: &[Action]) -> bool {
    actions.iter().any(|action| {
        !matches!(
            action,
            Action::Release { .. } | Action::Drag { .. } | Action::Wheel { .. }
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Leaving restores the host: modes off, the cursor forced visible, the
    /// attributes reset, and only then out of the alternate screen. A program
    /// that hid the cursor must not keep it hidden.
    #[test]
    fn leaving_shows_the_cursor_resets_and_leaves_last() {
        assert_eq!(
            exit_trailer(),
            b"\x1b[?2031l\x1b[?1004l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?25h\x1b[0m\x1b[?1049l"
                .to_vec(),
            "the exit sequence changed shape"
        );
    }

    /// The mouse is enabled where it reports usefully and nowhere else:
    /// Terminal.app clicks are inert, and a pipe or console answers nothing.
    /// The keyboard runs everything either way.
    #[test]
    fn the_mouse_stays_off_where_it_reports_nothing_useful() {
        let on = |term: Option<&str>, program: Option<&str>| {
            mouse_on_for(term.map(str::to_string), program.map(str::to_string))
        };
        assert!(on(Some("xterm-256color"), None));
        assert!(on(Some("xterm-256color"), Some("iTerm.app")));
        assert!(on(Some("wezterm"), Some("WezTerm")));
        assert!(!on(Some("xterm-256color"), Some("Apple_Terminal")));
        assert!(!on(Some("dumb"), None));
        assert!(!on(Some("linux"), None));
    }

    /// A finished row says the first line of the answer as a person reads it: the heading
    /// and emphasis marks are the page's, not the words.
    /// A drag or release past the tile's edge reaches the program at that edge.
    #[test]
    fn a_pointer_outside_the_focused_tile_is_pulled_back_to_its_edge() {
        let s = state();
        let tile = focused_rect(&s);
        assert!(tile.rows > 0 && tile.cols > 0);
        assert_eq!(
            clamped_to_focused(&s, u16::MAX, u16::MAX),
            (tile.rows - 1, tile.cols - 1)
        );
        assert_eq!(clamped_to_focused(&s, 1, 1), (0, 0));
    }

    #[test]
    fn the_rest_of_the_choosing_click_does_not_close_what_it_opened() {
        let mut keys = Keys::default();
        assert!(!closes_reading(&keys.feed(b"\x1b[<0;5;5m")), "the release");
        assert!(
            !closes_reading(&keys.feed(b"\x1b[<32;6;5M")),
            "a jitter, button down"
        );
        assert!(!closes_reading(&keys.feed(b"\x1b[<0;5")), "half a release");
        assert!(!closes_reading(&keys.feed(b";5m")), "and its other half");
        assert!(closes_reading(&keys.feed(b"\x1b[<0;5;5M")), "a new click");
        assert!(closes_reading(&keys.feed(b"q")), "a key");
    }

    use qoro_runtime::control::WorkspaceRow;

    fn space(id: i64, name: &str) -> WorkspaceRow {
        WorkspaceRow {
            keep_awake: false,
            id,
            name: name.to_string(),
            terminals: 1,
            asleep: false,
            sleeping: Vec::new(),
            closable: true,
            ink: None,
            link: None,
        }
    }

    fn agent(terminal: u32, workspace: i64, program: &str, state: &str) -> AgentRow {
        AgentRow {
            keep_awake: false,
            background: 0,
            news: false,
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
            activity: None,
            pgid: None,
            discards: None,
        }
    }

    /// Two spaces in a tall window, so nothing scrolls: app holds agent 1, a shell 3 the
    /// column does not show, and agent 2, in that order; webshop holds agent 4.
    fn drag_state() -> State {
        let mut s = state();
        s.lines = 50;
        let mut shell = agent(3, 1, "zsh", "idle");
        shell.agent = false;
        s.rows = vec![
            agent(1, 1, "codex", "blocked"),
            shell,
            agent(2, 1, "claude", "working"),
            agent(4, 2, "claude", "idle"),
        ];
        s
    }

    /// The column row (zero based, from the top) whose hit is `hit`, its first or its last.
    fn row_of(s: &State, hit: sidebar::Hit, last: bool) -> usize {
        let rows = s.column_rows_now();
        let mut at = rows
            .iter()
            .enumerate()
            .filter(|(_, (_, h))| *h == Some(hit));
        let found = if last { at.next_back() } else { at.next() };
        found.map(|(at, _)| at).expect("drawn")
    }

    /// Where a carried row lands, read off the unshifted column: a name row is before
    /// that tab, the row under it after it (before the hidden shell that follows), a rule
    /// before the space's first tab, a space's blank foot its end; the padding under the
    /// last space and the carried row's own place are no move at all.
    #[test]
    fn a_carried_row_lands_where_the_column_says() {
        use sidebar::Hit;
        let s = drag_state();
        let screen = |at: usize| (at + screen::SIDEBAR_TOP) as u16;
        let drop = |t: u32, at: usize| s.drop_target(Source::Agent(t), screen(at), 3);
        let name1 = row_of(&s, Hit::Terminal(1), false);
        let under1 = row_of(&s, Hit::Terminal(1), true);
        let rule1 = row_of(&s, Hit::Space(1), false);
        let foot1 = row_of(&s, Hit::Terminal(2), true) + 1;
        let foot2 = row_of(&s, Hit::Terminal(4), true) + 1;
        let slot = |place, rule| {
            Some(RowDrop::Slot {
                space: 1,
                place,
                rule,
            })
        };
        assert_eq!(drop(4, name1), slot(Place::Before(1), name1));
        assert_eq!(drop(4, under1), slot(Place::Before(3), under1 + 1));
        assert_eq!(drop(4, rule1), slot(Place::Before(1), rule1 + 1));
        assert_eq!(drop(4, foot1), slot(Place::End, foot1));
        assert_eq!(drop(4, foot2 + 1), None, "the window's padding is no place");
        // In its own space: its own name row, and the row after the tab before it, are
        // where it already is.
        let name2 = row_of(&s, Hit::Terminal(2), false);
        assert_eq!(drop(2, name2), Some(RowDrop::Here));
        assert_eq!(drop(2, foot1), Some(RowDrop::Here));
        assert_eq!(drop(2, name1), slot(Place::Before(1), name1));
        // The hint names what a person can see, never the hidden shell.
        let hint = drop_hint(&s, Source::Agent(4), drop(4, under1));
        assert!(
            hint.starts_with("move ") && hint.contains(", below "),
            "{hint}"
        );
        let end = drop_hint(&s, Source::Agent(4), drop(4, foot1));
        assert!(end.ends_with("to the end of app"), "{end}");
        // Its own place, named by itself, is where it already is.
        assert_eq!(drop(1, name1), Some(RowDrop::Here));
    }

    /// A carried space lands before the space whose rule is under the pointer, or after the
    /// space whose rows are, its rule on that space's blank foot; its own place, and the
    /// place the list would give it anyway, are no move. Over a pane it has no place.
    #[test]
    fn a_carried_space_lands_before_or_after_a_space() {
        use sidebar::Hit;
        let s = drag_state();
        let screen = |at: usize| (at + screen::SIDEBAR_TOP) as u16;
        let drop = |space: i64, at: usize| {
            s.drop_target(Source::Space { space, fold: true }, screen(at), 3)
        };
        let rule1 = row_of(&s, Hit::Space(1), false);
        let rule2 = row_of(&s, Hit::Space(2), false);
        let row4 = row_of(&s, Hit::Terminal(4), false);
        let foot2 = row_of(&s, Hit::Terminal(4), true) + 1;
        assert_eq!(
            drop(2, rule1),
            Some(RowDrop::SpaceSlot {
                before: Some(1),
                rule: rule1
            })
        );
        assert_eq!(drop(2, rule2), Some(RowDrop::Here), "its own rule");
        assert_eq!(
            drop(2, row_of(&s, Hit::Terminal(1), false)),
            Some(RowDrop::Here)
        );
        assert_eq!(
            drop(1, row4),
            Some(RowDrop::SpaceSlot {
                before: None,
                rule: foot2
            })
        );
        assert_eq!(
            s.drop_target(
                Source::Space {
                    space: 1,
                    fold: true
                },
                screen(rule1),
                60
            ),
            None,
            "a space has no place over a pane"
        );
        assert_eq!(space_hint(&s, 2, drop(2, rule1)), "move webshop above app");
        assert_eq!(space_hint(&s, 1, drop(1, row4)), "move app to the end");
        // Carried, its rule and every row under it dim, and its chip names it.
        let mut s = s;
        s.row_press = Some(RowPress {
            source: Source::Space {
                space: 1,
                fold: true,
            },
            at: (1, 1),
            now: (screen(row4), 3),
            name: false,
            moved: true,
            dead: false,
        });
        let drag = s.drag_paint(s.paint_now).expect("carried");
        assert_eq!(drag.space, Some(1));
        assert_eq!(drag.carried, vec![1, 3, 2]);
        assert_eq!(drag.chip.2, "app (2)");
        assert_eq!(drag.rule, Some((foot2, 1)));
    }

    /// A space carries across machines: a remote's space lands above a local one, and a
    /// local one below the remote's, wherever the list has them.
    #[test]
    fn a_carried_space_crosses_machines() {
        use sidebar::Hit;
        let mut s = drag_state();
        let far = (1i64 << 40) | 3;
        s.spaces.push(space(far, "devbox:api"));
        s.rows.push(agent(9, far, "claude", "idle"));
        let screen = |at: usize| (at + screen::SIDEBAR_TOP) as u16;
        let drop = |s: &State, space: i64, at: usize| {
            s.drop_target(Source::Space { space, fold: true }, screen(at), 3)
        };
        let rule1 = row_of(&s, Hit::Space(1), false);
        assert_eq!(
            drop(&s, far, rule1),
            Some(RowDrop::SpaceSlot {
                before: Some(1),
                rule: rule1
            })
        );
        let row9 = row_of(&s, Hit::Terminal(9), false);
        let foot = row_of(&s, Hit::Terminal(9), true) + 1;
        assert_eq!(
            drop(&s, 1, row9),
            Some(RowDrop::SpaceSlot {
                before: None,
                rule: foot
            })
        );
    }

    /// The focused terminal's directory starts a new tab only on its own machine.
    #[test]
    fn a_directory_stays_on_its_machine() {
        let mut s = drag_state();
        s.focused = 1;
        s.rows[0].cwd = Some(std::env::temp_dir().display().to_string());
        assert!(
            focused_cwd_in(&s, 2).is_some(),
            "another space, same machine"
        );
        assert_eq!(
            focused_cwd_in(&s, (1i64 << 40) | 3),
            None,
            "another machine"
        );
    }

    /// A pane of a split, let go in its own space, leaves the split for a tab of its own
    /// there, even where it already stands; its split's first row still moves the split.
    #[test]
    fn a_pane_let_go_in_its_own_space_leaves_its_split() {
        use sidebar::Hit;
        let mut s = drag_state();
        let area = pane_area(&s);
        let mut layout = Layout::single(1);
        assert!(layout.split_placeholder(1, layout::Dir::V, area));
        assert!(layout.bind_placeholder(2));
        s.layouts.insert(1, layout);
        s.space = Some(1);
        let screen = |at: usize| (at + screen::SIDEBAR_TOP) as u16;
        let drop = |s: &State, t: u32, at: usize| s.drop_target(Source::Agent(t), screen(at), 3);
        assert!(s.leaves_split(2) && !s.leaves_split(1));
        let name1 = row_of(&s, Hit::Terminal(1), false);
        assert_eq!(
            drop(&s, 2, name1),
            Some(RowDrop::Slot {
                space: 1,
                place: Place::Before(1),
                rule: name1
            })
        );
        // On its own rows: out of the split, after it, where a reorder would see no move.
        let low = row_of(&s, Hit::Terminal(2), true);
        assert!(matches!(
            drop(&s, 2, low),
            Some(RowDrop::Slot { space: 1, .. })
        ));
        let hint = drop_hint(&s, Source::Agent(2), drop(&s, 2, name1));
        assert!(hint.contains("out of its split"), "{hint}");
        // Its answer takes it out of this client's tree too, and keeps the keyboard.
        let focused = s.focused;
        let (mut request, mut dirty) = (0, false);
        apply(
            &mut s,
            ControlResponse::TerminalMoved {
                id: format!("{LEAVE}1"),
                terminal: 2,
                workspace: 1,
            },
            &mut request,
            &mut dirty,
        );
        assert!(!s.is_pane(2));
        assert_eq!(s.focused, focused);
        // The keyboard on the pane that leaves follows it to its own tab.
        let mut s = drag_state();
        let mut layout = Layout::single(1);
        assert!(layout.split_placeholder(1, layout::Dir::V, pane_area(&s)));
        assert!(layout.bind_placeholder(2));
        s.layouts.insert(1, layout);
        s.space = Some(1);
        s.focused = 2;
        apply(
            &mut s,
            ControlResponse::TerminalMoved {
                id: format!("{LEAVE}2"),
                terminal: 2,
                workspace: 1,
            },
            &mut request,
            &mut dirty,
        );
        assert_eq!(s.focused, 2);
        assert!(
            s.layouts.get(&1).is_some_and(|l| l.leaves().contains(&2)),
            "the tab on screen is the one the keyboard is on"
        );
    }

    /// A pane's name on its top edge is a handle for it, and the tab bar a place to let it
    /// go: past the last tab, the pane leaves its split for a tab of its own at the end.
    #[test]
    fn a_panes_name_carries_it_to_the_tab_bar() {
        let mut s = drag_state();
        s.cols = 160;
        let area = pane_area(&s);
        let mut layout = Layout::single(1);
        assert!(layout.split_placeholder(1, layout::Dir::V, area));
        assert!(layout.bind_placeholder(2));
        s.layouts.insert(1, layout);
        s.space = Some(1);
        let (_, rect) = s.layouts[&1]
            .tile_rects(area)
            .into_iter()
            .find(|(t, _)| *t == 2)
            .expect("tiled");
        let left = screen::left_for(s.cols, s.collapsed, s.sidebar_width);
        let row = (screen::TABS + rect.y) as u16;
        let col = (left + 1 + rect.x + 2) as u16;
        assert_eq!(s.title_at_screen(row, col), Some(2), "on its name");
        assert_eq!(s.title_at_screen(row + 1, col), None, "inside the pane");
        assert_eq!(
            s.title_at_screen(row, (left + 1 + rect.x + rect.cols - 1) as u16),
            None,
            "the edge past its name is the edge"
        );
        let end = (left + 1 + area.cols - 1) as u16;
        let place = RowDrop::Bar {
            space: 1,
            place: Place::End,
        };
        let bar = Some(place);
        assert_eq!(s.drop_target(Source::Pane(2), 1, end), bar);
        let hint = drop_hint(&s, Source::Pane(2), s.drop_target(Source::Pane(2), 1, end));
        assert!(hint.contains("out of its split, a tab"), "{hint}");
        // The split's first pane, held by its name, leaves too: it is asked to, by name.
        assert_eq!(s.drop_target(Source::Pane(1), 1, end), bar);
        let mut request = 0;
        let ask = drop_row(&mut s, 1, place, &mut request, true);
        assert!(
            matches!(&ask, Some(ControlRequest::MoveTerminal { id, terminal: 1, leave_split: true, .. })
                if id.starts_with(LEAVE)),
            "{ask:?}"
        );
        // Its row in the column still moves the whole split.
        let ask = drop_row(&mut s, 1, place, &mut request, false);
        assert!(
            matches!(
                &ask,
                Some(ControlRequest::MoveTerminal {
                    leave_split: false,
                    ..
                })
            ),
            "{ask:?}"
        );
    }

    /// A pane stacked under another has its name on the divider between them, which stays
    /// the divider: no handle there.
    #[test]
    fn a_name_on_a_divider_stays_the_divider() {
        let mut s = drag_state();
        s.cols = 160;
        s.lines = 50;
        let area = pane_area(&s);
        let mut layout = Layout::single(1);
        assert!(layout.split_placeholder(1, layout::Dir::H, area));
        assert!(layout.bind_placeholder(2));
        s.layouts.insert(1, layout);
        s.space = Some(1);
        let rects = s.layouts[&1].tile_rects(area);
        let left = screen::left_for(s.cols, s.collapsed, s.sidebar_width);
        let at = |t: u32| {
            let (_, r) = rects.iter().find(|(x, _)| *x == t).expect("tiled");
            ((screen::TABS + r.y) as u16, (left + 1 + r.x + 2) as u16)
        };
        let (row, col) = at(1);
        assert_eq!(s.title_at_screen(row, col), Some(1), "the top pane's name");
        let (row, col) = at(2);
        assert_eq!(s.title_at_screen(row, col), None, "on the divider");
    }

    /// The priority list has no order to point into: any row is its space, at the end.
    #[test]
    fn the_priority_list_drops_into_a_space_not_a_place() {
        let mut s = drag_state();
        s.view = sidebar::View::Priority;
        let rows = s.column_rows_now();
        let at = rows
            .iter()
            .position(|(_, h)| *h == Some(sidebar::Hit::Terminal(1)))
            .expect("drawn");
        let row = (at + screen::SIDEBAR_TOP) as u16;
        assert_eq!(
            s.drop_target(Source::Agent(4), row, 3),
            Some(RowDrop::Space(1))
        );
    }

    /// A carried row paints: its rows dim, its chip follows the pointer, and the place it
    /// would land opens its rule, in another space or its own.
    #[test]
    fn a_carried_row_is_drawn_while_it_moves() {
        use sidebar::Hit;
        let mut s = drag_state();
        let name1 = row_of(&s, Hit::Terminal(1), false);
        let pointer = ((name1 + screen::SIDEBAR_TOP) as u16, 4);
        s.row_press = Some(RowPress {
            source: Source::Agent(4),
            at: (1, 1),
            now: pointer,
            name: true,
            moved: true,
            dead: false,
        });
        let drag = s.drag_paint(s.paint_now).expect("carried");
        assert_eq!(drag.carried, vec![4]);
        assert_eq!(drag.rule, Some((name1, 1)));
        assert_eq!(drag.pointer, pointer);
        s.row_press = Some(RowPress {
            source: Source::Agent(2),
            ..s.row_press.unwrap()
        });
        // A reorder in its own space is promised its place too.
        assert_eq!(
            s.drag_paint(s.paint_now).expect("carried").rule,
            Some((name1, 1))
        );
        // Not moved yet: a press is still a click.
        s.row_press = Some(RowPress {
            moved: false,
            ..s.row_press.unwrap()
        });
        assert!(s.drag_paint(s.paint_now).is_none());
    }

    /// A divider drag arms only on the divider: the open column actually
    /// drawn, no overlay, exactly the rule column inside the painted rows.
    /// The foot row arms too, so the press never falls through to the fold
    /// mark the divider also crosses.
    #[test]
    fn a_divider_drag_arms_only_on_the_divider() {
        // The divider is the column after the default width, one based.
        let d = sidebar::WIDTH as u16 + 1;
        let mut s = state();
        s.palette = None;
        // Rows 1 through lines - STATUS.
        assert!(s.divider_press(d, 5));
        assert!(s.divider_press(d, 22));
        assert_eq!(s.sidebar_drag, Some(sidebar::WIDTH));

        // Pane, titles, and outside the painted rows: nothing arms.
        let mut s = state();
        assert!(!s.divider_press(d + 1, 5));
        assert!(!s.divider_press(d - 1, 5));
        assert!(!s.divider_press(d, 0));
        assert!(!s.divider_press(d, 23));
        assert_eq!(s.sidebar_drag, None);

        // Folded, overlaid, or too narrow to draw the column: nothing arms.
        let mut s = state();
        s.collapsed = true;
        assert!(!s.divider_press(5, 5));
        let mut s = state();
        s.menu = Some(0);
        assert!(!s.divider_press(d, 5));
        let mut s = state();
        s.cols = 10;
        assert!(!s.divider_press(d, 5));
    }

    /// Motions clamp to the sidebar range, latch past the pane, ignore
    /// shift, and report whether anything moved: equal widths repaint and
    /// reflow nothing.
    #[test]
    fn divider_motions_clamp_latch_and_report_movement() {
        // The divider is the column after the default width, one based.
        let d = sidebar::WIDTH as u16 + 1;
        let mut s = state();
        s.palette = None;
        assert!(s.divider_press(d, 5));
        // Pull left past the end: clamps, and says so.
        assert!(s.divider_motion(5, false));
        assert_eq!(s.sidebar_width, sidebar::SIDEBAR_MIN);
        // Stuck at the clamp: same width reports nothing.
        assert!(!s.divider_motion(1, false));
        assert_eq!(s.sidebar_width, sidebar::SIDEBAR_MIN);
        // Past the pane edge the drag stays latched.
        assert!(s.divider_motion(70, false));
        assert_eq!(s.sidebar_width, sidebar::sidebar_max(s.cols));
        // Shift motions are ignored while the drag stays armed.
        assert!(!s.divider_motion(20, true));
        assert_eq!(s.sidebar_width, sidebar::sidebar_max(s.cols));
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
        // The divider is the column after the default width, one based.
        let d = sidebar::WIDTH as u16 + 1;
        let mut s = state();
        s.palette = None;
        assert!(!s.finish_drag());
        assert!(s.divider_press(d, 5));
        assert!(!s.finish_drag(), "a motionless press moved nothing");
        assert!(s.divider_press(d, 5));
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

    /// A space close sends its tabs one at a time, and only asks the daemon to close the
    /// space once there is nothing left in it.
    ///
    /// The burst is what the shape suggests and it is wrong twice over: a close is answered
    /// before the store row closes, so the daemon still counts the tab, and a request whose
    /// space lock is busy runs off the connection's read loop, so a burst can arrive out of
    /// order. This is the property that keeps the space from being asked to close over tabs
    /// that are on their way out.
    #[test]
    fn a_space_close_asks_for_one_tab_at_a_time_and_waits_for_the_index() {
        let mut s = state();
        s.rows = vec![
            agent(1, 1, "claude", "working"),
            agent(2, 1, "codex", "idle"),
        ];
        s.spaces = vec![space(1, "qoro")];
        let mut request = 0u64;
        s.armed = Some(1);
        armed_key(&mut s, 1, b"y", &mut request);
        assert_eq!(s.armed, None, "the question is answered");
        // One tab, not two, and no space yet.
        assert_eq!(s.closing.as_ref().map(|c| c.sent), Some(1));
        assert_eq!(s.modes.outbox.len(), 1);
        assert!(matches!(
            s.modes.outbox[0],
            ControlRequest::CloseTerminal { terminal: 1, .. }
        ));

        // The first tab answers: the second is asked, and the space is not.
        let mut dirty = false;
        apply(
            &mut s,
            ControlResponse::TerminalClosed {
                id: "close-1".into(),
                terminal: 1,
            },
            &mut request,
            &mut dirty,
        );
        assert_eq!(s.closing.as_ref().map(|c| c.sent), Some(2));
        assert_eq!(s.modes.outbox.len(), 2);
        assert!(matches!(
            s.modes.outbox[1],
            ControlRequest::CloseTerminal { terminal: 2, .. }
        ));

        // The second answers and the index has not caught up: still no space request.
        apply(
            &mut s,
            ControlResponse::TerminalClosed {
                id: "close-2".into(),
                terminal: 2,
            },
            &mut request,
            &mut dirty,
        );
        assert_eq!(
            s.modes.outbox.len(),
            2,
            "asked the space before the index agreed"
        );
        assert!(!space_is_empty(&s, 1), "the index still lists a tab");

        // The index agrees, and only then does the space go.
        s.rows.clear();
        assert!(space_is_empty(&s, 1), "the index says the space is empty");
        apply(
            &mut s,
            ControlResponse::Agents {
                id: "index".into(),
                agents: Vec::new(),
                splits: Vec::new(),
            },
            &mut request,
            &mut dirty,
        );
        assert!(
            s.modes
                .outbox
                .iter()
                .any(|r| matches!(r, ControlRequest::CloseWorkspace { workspace: 1, .. })),
            "{:?}",
            s.modes.outbox
        );
    }

    /// A second close does not overwrite the first. The `x` is on every rule while a close
    /// is in flight, so asking about another space is one click away, and clobbering the
    /// state would leave the first space's tabs half closed with nothing chasing them.
    #[test]
    fn a_second_close_does_not_overwrite_the_first() {
        let mut s = state();
        s.rows = vec![agent(1, 1, "claude", "working")];
        s.spaces = vec![space(1, "qoro")];
        let mut request = 0u64;
        start_close(&mut s, 1, &mut request);
        let asked = s.modes.outbox.len();
        assert_eq!(asked, 1);
        start_close(&mut s, 1, &mut request);
        assert_eq!(s.modes.outbox.len(), asked, "the same tab asked twice");
        assert_eq!(s.closing.as_ref().map(|c| c.space), Some(1));
    }

    /// A space closed by somebody else does not clear our close. The answer arrives for
    /// any space that went, including one another client or the mux reconciled away, and
    /// clearing ours would strand the tabs still on their way out of it.
    #[test]
    fn a_close_of_somebody_elses_space_leaves_ours_alone() {
        let mut s = state();
        s.rows = vec![agent(1, 1, "claude", "working")];
        s.spaces = vec![space(1, "qoro"), space(2, "other")];
        let mut request = 0u64;
        start_close(&mut s, 1, &mut request);
        let mut dirty = false;
        apply(
            &mut s,
            ControlResponse::WorkspaceClosed {
                id: "mux".into(),
                workspace: 2,
            },
            &mut 0,
            &mut dirty,
        );
        assert_eq!(
            s.closing.as_ref().map(|c| c.space),
            Some(1),
            "our close was dropped"
        );
    }

    /// A tab that will not take the close is asked again, because the daemon's only
    /// refusal is a command channel that was full at that instant, and a close of a space
    /// must not stall behind one of them.
    #[test]
    fn a_tab_that_refuses_the_close_is_asked_again() {
        let mut s = state();
        s.rows = vec![agent(1, 1, "claude", "working")];
        s.spaces = vec![space(1, "qoro")];
        let mut request = 0u64;
        start_close(&mut s, 1, &mut request);
        let id = match &s.modes.outbox[0] {
            ControlRequest::CloseTerminal { id, terminal } => {
                assert_eq!(*terminal, 1);
                id.clone()
            }
            other => panic!("{other:?}"),
        };
        let mut dirty = false;
        apply(
            &mut s,
            ControlResponse::Failed {
                id: id.clone(),
                reason: "terminal 1 did not accept the close: full".into(),
            },
            &mut request,
            &mut dirty,
        );
        assert_eq!(
            s.closing.as_ref().and_then(|c| c.stuck),
            Some(1),
            "the tab gave up"
        );
        // Asked again, for the same tab.
        assert_eq!(s.modes.outbox.len(), 2, "{:?}", s.modes.outbox);
        assert!(matches!(
            s.modes.outbox[1],
            ControlRequest::CloseTerminal { terminal: 1, .. }
        ));
        // And a refused tab close of one tab on its own keeps the notice row it always had:
        // a hint is wiped by the next keystroke.
        let mut solo = state();
        solo.closing = None;
        let mut dirty = false;
        apply(
            &mut solo,
            ControlResponse::Failed {
                id,
                reason: "terminal 9 did not accept the close: full".into(),
            },
            &mut 0,
            &mut dirty,
        );
        // The refusal rides on both rows: the notice is what a person reads, and the hint
        // is what survives a full pane's own render wiping the notice.
        assert!(
            solo.hint
                .as_deref()
                .unwrap_or_default()
                .contains("did not accept")
        );
        assert!(
            solo.message
                .as_deref()
                .unwrap_or_default()
                .contains("did not accept"),
            "{:?}",
            solo.message
        );
    }

    /// The space is asked to close once, however many indexes say it is empty. The gate
    /// runs on every index, and a second ask for a space the first one deleted comes back
    /// as a refusal that would be reported against a close that worked.
    #[test]
    fn the_space_is_asked_to_close_exactly_once() {
        let mut s = state();
        s.rows = vec![agent(1, 1, "claude", "working")];
        s.spaces = vec![space(1, "qoro")];
        let mut request = 0u64;
        start_close(&mut s, 1, &mut request);
        let mut dirty = false;
        apply(
            &mut s,
            ControlResponse::TerminalClosed {
                id: "close-1".into(),
                terminal: 1,
            },
            &mut request,
            &mut dirty,
        );
        s.rows.clear();
        for _ in 0..3 {
            apply(
                &mut s,
                ControlResponse::Agents {
                    id: "index".into(),
                    agents: Vec::new(),
                    splits: Vec::new(),
                },
                &mut request,
                &mut dirty,
            );
        }
        let asked = s
            .modes
            .outbox
            .iter()
            .filter(|r| matches!(r, ControlRequest::CloseWorkspace { .. }))
            .count();
        assert_eq!(asked, 1, "{:?}", s.modes.outbox);
    }

    /// A space with nothing in it is asked to close at once: there is nothing to wait for.
    #[test]
    fn an_empty_space_closes_in_one_step() {
        let mut s = state();
        s.rows.clear();
        s.spaces = vec![space(1, "dub")];
        let mut request = 0u64;
        start_close(&mut s, 1, &mut request);
        assert_eq!(s.modes.outbox.len(), 1);
        assert!(
            matches!(
                s.modes.outbox[0],
                ControlRequest::CloseWorkspace { workspace: 1, .. }
            ),
            "{:?}",
            s.modes.outbox
        );
    }

    /// The space list is what empties a sleeping space, so the gate runs there too. On the
    /// index alone the two answers arrive in the order the loop asks for them, the index
    /// first, and nothing asks again: the space waits for the give-up timer and the timer
    /// blames a tab that never refused anything.
    #[test]
    fn a_sleeping_space_is_asked_to_close_when_its_tabs_are_gone() {
        let mut s = state();
        s.rows.clear();
        s.spaces = vec![asleep(1, "qoro", &[4, 5, 6])];
        let mut request = 0u64;
        start_close(&mut s, 1, &mut request);
        let mut dirty = false;
        let mut sent = 0u64;
        for terminal in [4u32, 5, 6] {
            sent += 1;
            apply(
                &mut s,
                ControlResponse::TerminalClosed {
                    id: format!("close-{sent}"),
                    terminal,
                },
                &mut request,
                &mut dirty,
            );
        }
        // The index cannot see a sleeping space's tabs, so it says nothing and no space
        // close goes out on it.
        apply(
            &mut s,
            ControlResponse::Agents {
                id: "index".into(),
                agents: Vec::new(),
                splits: Vec::new(),
            },
            &mut request,
            &mut dirty,
        );
        assert!(
            !s.modes
                .outbox
                .iter()
                .any(|r| matches!(r, ControlRequest::CloseWorkspace { .. })),
            "the index has no news about a sleeping space: {:?}",
            s.modes.outbox
        );
        // The space list is the news.
        apply(
            &mut s,
            ControlResponse::Workspaces {
                id: "spaces".into(),
                workspaces: vec![asleep(1, "qoro", &[])],
            },
            &mut request,
            &mut dirty,
        );
        assert!(
            s.modes
                .outbox
                .iter()
                .any(|r| matches!(r, ControlRequest::CloseWorkspace { workspace: 1, .. })),
            "the space list emptied it and nothing asked: {:?}",
            s.modes.outbox
        );
        assert_eq!(
            s.closing.as_ref().and_then(|c| c.space_asked_id.clone()),
            s.modes.outbox.iter().find_map(|r| match r {
                ControlRequest::CloseWorkspace { id, .. } => Some(id.clone()),
                _ => None,
            }),
            "the id it went out under is kept, to tell our refusal from ctrl-a W's"
        );
    }

    /// `ctrl-a W` asks for a space close directly and its refusal is a notice row, the way
    /// it has always been. It never sets `closing`, so an arm that assumed it did would
    /// swallow every refusal the daemon has ever given: the default space, and a sleeping
    /// one whose tabs the client's own count cannot see.
    #[test]
    fn a_refused_ctrl_a_w_is_the_notice_row_it_has_always_been() {
        for name in ["the default space", "a space with three tabs in it"] {
            let mut s = state();
            let mut dirty = false;
            let mut request = 0u64;
            apply(
                &mut s,
                ControlResponse::Failed {
                    id: "close-space-9".into(),
                    reason: name.into(),
                },
                &mut request,
                &mut dirty,
            );
            assert_eq!(s.message.as_deref(), Some(name), "{name}");
            // On both rows: the notice row is what it has always been, and the hint row
            // is where it survives a pane's own render.
            assert_eq!(s.hint.as_deref(), Some(name), "{name}");
            assert!(s.closing.is_none(), "{name}");
        }
    }

    /// `ctrl-a W` and this client's space close share an id prefix, so a refusal is told
    /// apart by the id it went out under. Somebody else's refusal is a notice row and does
    /// not re-arm our gate, which would send ours a second time and have its refusal
    /// reported against a close that already worked.
    #[test]
    fn a_refused_ctrl_a_w_does_not_re_ask_our_space() {
        let mut s = state();
        s.rows = vec![agent(1, 1, "claude", "working")];
        s.spaces = vec![space(1, "qoro")];
        let mut request = 0u64;
        start_close(&mut s, 1, &mut request);
        apply(
            &mut s,
            ControlResponse::TerminalClosed {
                id: "close-1".into(),
                terminal: 1,
            },
            &mut request,
            &mut false,
        );
        s.rows.clear();
        apply(
            &mut s,
            ControlResponse::Agents {
                id: "index".into(),
                agents: Vec::new(),
                splits: Vec::new(),
            },
            &mut request,
            &mut false,
        );
        let ours = s
            .modes
            .outbox
            .iter()
            .find_map(|r| match r {
                ControlRequest::CloseWorkspace { id, .. } => Some(id.clone()),
                _ => None,
            })
            .expect("the space close went out");
        let mut dirty = false;
        apply(
            &mut s,
            ControlResponse::Failed {
                id: "close-space-77".into(),
                reason: "workspace 2 is this session's default".into(),
            },
            &mut request,
            &mut dirty,
        );
        assert_eq!(
            s.message.as_deref(),
            Some("workspace 2 is this session's default"),
            "a refusal that is not ours is the notice row"
        );
        assert!(
            s.closing
                .as_ref()
                .is_some_and(|c| c.space_asked_id.as_deref() == Some(ours.as_str())),
            "our gate was re-armed by somebody else's refusal: {:?}",
            s.closing.as_ref().map(|c| &c.space_asked_id)
        );
        // And ours still re-arms, because the tabs that caused the refusal are on their
        // way out and the next index will find the space empty.
        let mut dirty = false;
        apply(
            &mut s,
            ControlResponse::Failed {
                id: ours.clone(),
                reason: "workspace 1 still holds 1 tab".into(),
            },
            &mut request,
            &mut dirty,
        );
        assert!(s.closing.as_ref().is_some_and(|c| !c.space_asked));
        // Nothing of ours is left to wait for, so the daemon's own words are the whole
        // message rather than our count appended to them.
        assert_eq!(s.hint.as_deref(), Some("workspace 1 still holds 1 tab"));
    }

    /// A sleeping space's tabs are the store's, not the index's, so both the question and
    /// the gate count them. A close that reported zero would be closing over tabs nobody
    /// had closed.
    #[test]
    fn a_sleeping_space_is_counted_by_its_store_rows_and_not_by_the_index() {
        let mut s = state();
        s.rows.clear();
        s.spaces = vec![asleep(1, "qoro", &[4, 5, 6])];
        assert!(!space_is_empty(&s, 1), "a sleeping space is not empty");
        assert_eq!(tabs_left(&s, 1), 3, "the index has none of them");
        let mut request = 0u64;
        start_close(&mut s, 1, &mut request);
        // Its sleeping tabs are asked for by number, and the space waits for them.
        assert_eq!(
            s.closing.as_ref().map(|c| c.asked.as_slice()),
            Some([4, 5, 6].as_slice())
        );
        assert!(matches!(
            s.modes.outbox[0],
            ControlRequest::CloseTerminal { terminal: 4, .. }
        ));
        assert!(
            !s.modes
                .outbox
                .iter()
                .any(|r| matches!(r, ControlRequest::CloseWorkspace { .. })),
            "the space was asked to close over three tabs"
        );
    }

    /// A tab that refuses is asked a bounded number of times, and then the chain leaves it
    /// behind rather than asking it for ever.
    ///
    /// The counter is the whole point: a reset on every refusal makes the budget
    /// unreachable, the tab is asked until the give-up timer, and the tabs behind it are
    /// never reached at all.
    #[test]
    fn a_tab_that_always_refuses_is_left_behind_after_a_few_asks() {
        let mut s = state();
        s.rows = vec![
            agent(1, 1, "claude", "working"),
            agent(2, 1, "claude", "working"),
            agent(3, 1, "claude", "working"),
        ];
        s.spaces = vec![space(1, "qoro")];
        let mut request = 0u64;
        assert!(start_close(&mut s, 1, &mut request), "the close started");
        let mut asked: Vec<u32> = Vec::new();
        let mut dirty = false;
        for round in 0..8 {
            let id = match s.modes.outbox.last() {
                Some(ControlRequest::CloseTerminal { id, terminal }) => {
                    asked.push(*terminal);
                    id.clone()
                }
                other => panic!("round {round}: {other:?}"),
            };
            // The first tab answers, so the chain moves on to the second, which never does.
            if asked[round] == 1 {
                apply(
                    &mut s,
                    ControlResponse::TerminalClosed {
                        id: id.clone(),
                        terminal: 1,
                    },
                    &mut request,
                    &mut dirty,
                );
            } else {
                apply(
                    &mut s,
                    ControlResponse::Failed {
                        id: id.clone(),
                        reason: format!("terminal {} did not accept the close: full", asked[round]),
                    },
                    &mut request,
                    &mut dirty,
                );
            }
        }
        assert_eq!(
            asked,
            vec![1, 2, 2, 2, 2, 3, 3, 3],
            "the stuck tab was not left behind: {asked:?}"
        );
        assert!(
            !s.modes
                .outbox
                .iter()
                .any(|r| matches!(r, ControlRequest::CloseWorkspace { .. })),
            "the space was asked to close over a tab that is still there"
        );
    }

    /// A retry that works ends the trouble: the tab is closed, and asking for it again
    /// would be asking the daemon to refuse.
    #[test]
    fn a_retry_that_works_is_not_asked_again() {
        let mut s = state();
        s.rows = vec![
            agent(1, 1, "claude", "working"),
            agent(2, 1, "codex", "idle"),
        ];
        s.spaces = vec![space(1, "qoro")];
        let mut request = 0u64;
        start_close(&mut s, 1, &mut request);
        let mut dirty = false;
        // The first tab refuses once and then takes the close.
        let id = match &s.modes.outbox[0] {
            ControlRequest::CloseTerminal { id, .. } => id.clone(),
            other => panic!("{other:?}"),
        };
        apply(
            &mut s,
            ControlResponse::Failed {
                id: id.clone(),
                reason: "terminal 1 did not accept the close: full".into(),
            },
            &mut request,
            &mut dirty,
        );
        let retried = match &s.modes.outbox[1] {
            ControlRequest::CloseTerminal { id, terminal } => {
                assert_eq!(*terminal, 1, "the retry was for another tab");
                id.clone()
            }
            other => panic!("{other:?}"),
        };
        apply(
            &mut s,
            ControlResponse::TerminalClosed {
                id: retried,
                terminal: 1,
            },
            &mut request,
            &mut dirty,
        );
        // The next ask is the second tab, never the first again.
        let next = s.modes.outbox.last().cloned();
        assert!(
            matches!(
                next,
                Some(ControlRequest::CloseTerminal { terminal: 2, .. })
            ),
            "{next:?}"
        );
        assert_eq!(s.closing.as_ref().and_then(|c| c.stuck), None);
    }

    /// A read that carries a key and a report in it is not handed to the question whole.
    /// The key is a key and the report is a click, which is what both of them are, and
    /// treating the read as one or the other loses one of them.
    #[test]
    fn a_read_is_only_the_mouse_when_it_is_nothing_but_the_mouse() {
        assert!(is_mouse(b"\x1b[<0;28;5M"), "a bare press");
        assert!(
            is_mouse(b"\x1b[<0;28;5m\x1b[<0;26;5m"),
            "a click, two reports"
        );
        assert!(!is_mouse(b"y"), "a key");
        assert!(!is_mouse(b"y\x1b[<0;28;5M"), "a key and a report");
        assert!(!is_mouse(b"\x1b[<0;28;5My"), "a report and a key");
        assert!(!is_mouse(b"\x1b[A"), "an arrow key, which is also a CSI");
    }

    /// The question names what will refuse, because the daemon's own close checks nothing
    /// and a count alone would not say which tab is still working.
    /// The count on the question and the number of requests that follow it are one number.
    /// A tab the index and the store both know is one tab: counted twice on the question
    /// and once to the daemon, the person reads "4 tabs" and watches three close.
    #[test]
    fn the_question_counts_the_tabs_the_close_will_ask_for() {
        let mut s = state();
        s.rows = vec![agent(4, 1, "claude", "idle")];
        // A space that fell asleep between this client's last index and the click: the
        // store still lists tab 4 and the index still lists it too.
        s.spaces = vec![asleep(1, "qoro", &[4, 5])];
        arm_close(&mut s, 1);
        let asked = s.message.clone().unwrap_or_default();
        assert!(asked.contains("2 tabs"), "{asked:?}");
        let mut request = 0u64;
        assert!(
            start_close(&mut s, 1, &mut request),
            "the close did not begin"
        );
        assert_eq!(
            s.closing.as_ref().map(|c| c.asked.as_slice()),
            Some([4, 5].as_slice()),
            "the daemon is asked for a different number than the question named"
        );
    }

    /// A question is drawn on a rule, and a rule exists only in the tree. Everything that
    /// takes the tree away leaves a question nothing can see and nothing can answer, with
    /// the keyboard still held: pressing `y` then closes a space over a row that is not on
    /// screen any more. So every click that changes the view puts the question down.
    #[test]
    fn a_click_that_changes_the_view_puts_the_question_down() {
        let mut s = state();
        s.rows = vec![agent(1, 1, "claude", "idle")];
        s.spaces = vec![space(1, "qoro")];
        arm_close(&mut s, 1);
        assert_eq!(s.armed, Some(1), "the question never went up");
        // The row and the columns the paint actually drew, read out of the column the
        // click maps onto, so a control that moves moves the test with it.
        let column = s.column(s.paint_now, 0);
        let rows = sidebar::rows_folded(
            &column,
            screen::column_rows(s.lines as usize),
            sidebar::current_avatar().as_ref(),
        );
        let title = rows
            .iter()
            .position(|(_, hit)| *hit == Some(sidebar::Hit::Title))
            .expect("the title row")
            + screen::SIDEBAR_TOP;
        let on = |what| {
            (1..=crate::sidebar::WIDTH)
                .find(|c| {
                    sidebar::act(sidebar::Hit::Title, *c, &column, false).is_some_and(|a| {
                        matches!(
                            (what, a),
                            ("view", sidebar::Act::View) | ("new", sidebar::Act::NewSpace)
                        )
                    })
                })
                .unwrap_or_else(|| panic!("no column on the title row is {what}"))
        };
        let (title, view_col, new_col) = (title as u16, on("view") as u16, on("new") as u16);
        for (what, row, col) in [
            ("the order word", title, view_col),
            ("the plus", title, new_col),
            ("a rule's own row", title + 1, 3),
        ] {
            let hit = s.clicked(row, col);
            assert!(
                hit.is_some(),
                "{what}: the click at {row},{col} hit nothing, so the test proves nothing"
            );
            let about = format!("{hit:?}", hit = hit);
            s.armed = Some(1);
            let asked = put_question_down(&mut s, hit);
            assert_eq!(asked, Some(1), "{what}: there was no question to put down");
            assert_eq!(s.armed, None, "{what}: {about} left the question asking");
            assert_eq!(s.message.as_deref(), Some("kept qoro"), "{what}: {about}");
        }
        // And the three clicks that are about the question keep it, because the arm
        // behind them is what puts it back up.
        for keep in [
            Some(Clicked::Close(1)),
            Some(Clicked::ConfirmClose(1)),
            Some(Clicked::CancelClose),
        ] {
            let about = format!("{keep:?}", keep = keep);
            s.armed = Some(1);
            s.message = None;
            assert_eq!(put_question_down(&mut s, keep), Some(1));
            assert!(
                s.message.is_none(),
                "{about} is about the question and must not say a space was kept"
            );
        }
    }

    /// One tab spending the whole budget does not spend the next tab's. The daemon's only
    /// refusal is a command channel that was full at that instant, which a retry fixes, so
    /// a tab behind a given-up one is entitled to its own retries rather than being
    /// abandoned on its first refusal with an allowance of zero.
    #[test]
    fn a_tab_behind_a_given_up_one_gets_its_own_retries() {
        let mut s = state();
        s.rows = vec![agent(1, 1, "claude", "idle"), agent(2, 1, "codex", "idle")];
        s.spaces = vec![space(1, "qoro")];
        let mut request = 0u64;
        start_close(&mut s, 1, &mut request);
        let mut dirty = false;
        let asked = |s: &State, at: usize| match &s.modes.outbox[at] {
            ControlRequest::CloseTerminal { id, terminal } => (id.clone(), *terminal),
            other => panic!("{other:?}"),
        };
        // Tab 1 refuses until its budget is spent and the chain leaves it behind.
        let mut last = None;
        for _ in 0..=STUCK_TRIES {
            let (id, terminal) = asked(&s, s.modes.outbox.len() - 1);
            assert_eq!(terminal, 1, "the chain left tab 1 too early");
            last = Some(id.clone());
            apply(
                &mut s,
                ControlResponse::Failed {
                    id,
                    reason: "terminal 1 did not accept the close: full".into(),
                },
                &mut request,
                &mut dirty,
            );
        }
        let at = s.modes.outbox.len() - 1;
        let (id, terminal) = asked(&s, at);
        assert_eq!(terminal, 2, "the chain never reached the tab behind it");
        assert_ne!(Some(&id), last.as_ref(), "tab 2 was asked under tab 1's id");
        // And it gets a full budget of its own, not the one tab 1 spent.
        let mut refuses = 0;
        for _ in 0..=STUCK_TRIES {
            let (id, terminal) = asked(&s, s.modes.outbox.len() - 1);
            assert_eq!(
                terminal, 2,
                "tab 2 was given up on after {refuses} refusals"
            );
            refuses += 1;
            apply(
                &mut s,
                ControlResponse::Failed {
                    id: id.clone(),
                    reason: "terminal 2 did not accept the close: full".into(),
                },
                &mut request,
                &mut dirty,
            );
        }
        assert_eq!(refuses, STUCK_TRIES + 1);
    }

    /// A report can arrive in two reads. The half that begins one belongs to the mouse,
    /// and the decoder is holding the rest of it, so a rule mid question must not read
    /// the half as an answer: that would close a space over a click still on its way.
    #[test]
    fn half_a_mouse_report_is_not_an_answer() {
        let mut s = state();
        s.rows = vec![agent(1, 1, "claude", "idle")];
        s.spaces = vec![space(1, "qoro")];
        arm_close(&mut s, 1);
        assert_eq!(s.armed, Some(1));
        assert!(
            is_mouse(b"\x1b[<0;28;5M\x1b[<0;28;5m"),
            "the whole pair is a mouse read"
        );
        assert!(!is_mouse(b"\x1b[<0;28;5"), "half of one is not");
        assert!(
            is_report_head(b"\x1b[<0;28;5"),
            "and the armed arm has to be able to tell the difference"
        );
        assert!(
            !is_report_head(b"y"),
            "a keypress is not the head of a report"
        );
        // Which is the whole point: the gate leaves the half to the decoder, which is
        // holding the tail.
        let mut keys = Keys::with_prefix(s.config.prefix);
        assert!(keys.feed(b"\x1b[<0;28;5").is_empty());
        assert!(
            keys.mouse_pending(),
            "the decoder did not hold the half, so the tail would be read as keys"
        );
        assert!(
            keys.feed(b";5M")
                .iter()
                .any(|a| matches!(a, Action::Click { .. }))
        );
        assert!(!keys.mouse_pending());
    }

    /// The give-up timer is armed where the close is created, by a click and by a key, and
    /// never on a later answer: a daemon that takes the request and says nothing is exactly
    /// the case the timer exists for, and it would never answer. `start_close` refuses a
    /// second close, so a close whose flag the next close wipes would wait out the timer
    /// with nobody watching.
    /// The give-up timer is armed where the close is created, by a click and by a key, and
    /// never on a later answer: a daemon that takes the request and says nothing is exactly
    /// the case the timer exists for, and it would never answer. `start_close` refuses a
    /// second close, so a close whose flag the next close wipes would wait out the timer
    /// with nobody watching.
    #[test]
    fn a_close_arms_the_give_up_timer_where_it_is_created() {
        let mut s = state();
        s.rows = vec![agent(1, 1, "claude", "idle")];
        s.spaces = vec![space(1, "qoro")];
        arm_close(&mut s, 1);
        let mut request = 0u64;
        let prefix = s.config.prefix;
        let outcome = mode_input(&mut s, b"y", prefix, &mut request);
        assert!(matches!(outcome, ModeOutcome::Stay));
        assert!(
            s.close_begun,
            "the key path armed no timer, and a close nobody answers never ends"
        );
        // The click path needs no flag: it is in scope where the close begins.
        let mut s = state();
        s.rows = vec![agent(1, 1, "claude", "idle")];
        s.spaces = vec![space(1, "qoro")];
        arm_close(&mut s, 1);
        let mut request = 0u64;
        assert!(
            start_close(&mut s, 1, &mut request),
            "the close did not begin"
        );
        assert!(s.closing.is_some());
    }

    #[test]
    fn a_close_from_the_key_arms_the_give_up_timer() {
        let mut s = state();
        s.rows = vec![agent(1, 1, "claude", "idle")];
        s.spaces = vec![space(1, "qoro")];
        arm_close(&mut s, 1);
        let mut request = 0u64;
        let prefix = s.config.prefix;
        let outcome = mode_input(&mut s, b"y", prefix, &mut request);
        assert!(matches!(outcome, ModeOutcome::Stay));
        assert!(
            s.close_begun,
            "the key path armed no timer, and a close nobody answers never ends"
        );
        // And the click path does it without the flag at all, because it is in scope.
        let mut s = state();
        s.rows = vec![agent(1, 1, "claude", "idle")];
        s.spaces = vec![space(1, "qoro")];
        arm_close(&mut s, 1);
        let mut request = 0u64;
        assert!(
            start_close(&mut s, 1, &mut request),
            "the close did not begin"
        );
    }

    #[test]
    fn the_close_question_names_the_tabs_that_are_working() {
        let mut s = state();
        s.rows = vec![
            agent(1, 1, "claude", "working"),
            agent(2, 1, "codex", "idle"),
        ];
        s.spaces = vec![space(1, "qoro")];
        arm_close(&mut s, 1);
        assert_eq!(s.armed, Some(1));
        let asked = s.message.clone().unwrap_or_default();
        assert!(asked.contains("close qoro?"), "{asked:?}");
        assert!(asked.contains("2 tabs"), "{asked:?}");
        assert!(asked.contains("claude in tab 1 is working"), "{asked:?}");
    }

    /// Any other key takes the question down rather than being typed at whatever is behind.
    #[test]
    fn any_key_answers_a_space_question() {
        let mut s = state();
        s.spaces = vec![space(1, "qoro")];
        let mut request = 0u64;
        s.armed = Some(1);
        armed_key(&mut s, 1, b"n", &mut request);
        assert_eq!(s.armed, None);
        assert_eq!(s.message.as_deref(), Some("kept qoro"));
        assert!(s.closing.is_none(), "nothing was sent");
    }

    fn state() -> State {
        State {
            close_begun: false,
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
            last: LastFocus::default(),
            show_when_listed: None,
            elsewhere: None,
            waking: None,
            dim: 0,
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
            inbox: sidebar::Inbox::default(),
            inbox_drag: None,

            shut: sidebar::Shut::new(),

            in_column: false,

            prefix_pending: false,

            answer: None,

            usage: sidebar::Usage::new(),

            usage_seen: std::collections::BTreeMap::new(),
            seen_done: sidebar::Seen::new(),
            unseen_finished: sidebar::Unseen::new(),
            tick: 0,
            paint_now: 0,
            sidebar_width: sidebar::WIDTH,
            sidebar_drag: None,
            row_press: None,
            drop_after_move: None,
            sidebar_sized: sidebar::WIDTH,
            palette: Some(Palette::open()),
            palette_pending: Vec::new(),
            keys_query: None,
            menu_kind: MenuKind::Main,
            connecting: None,
            probing: None,
            installing: None,
            reprobe: None,
            joining: None,
            modes: Modes::default(),
            selection: None,
            forwarded_press: false,
            anchor: None,
            behind: Behind::default(),
            outer_focus: None,
            host_colors: [None, None],
            host_light: None,
            icon_reply: crate::icons::Reply::Silent,
            icon_font: false,
            host_sent: None,
            host_refused: false,
            news_sent: None,
            news_refused: false,
            notes: Vec::new(),
            notes_due: None,
            route: crate::notify::Route::Nowhere(crate::notify::Why::NoProgram),
            nowhere: false,
            config: crate::config::Config::default(),
            snapshots: std::collections::BTreeMap::new(),
            last_seen: std::collections::BTreeMap::new(),
            previewing: false,
            images: std::collections::BTreeMap::new(),
            graphics: crate::graphics::GraphicsCap::off(),
            shown_images: std::collections::BTreeMap::new(),
            img_size: (0, 0),
            subs: std::collections::BTreeSet::new(),
            sized: std::collections::BTreeMap::new(),
            layouts: std::collections::BTreeMap::new(),
            parked: std::collections::BTreeMap::new(),
            split_writes: Vec::new(),
            split_saving: 0,
            resync: false,
            split_origin: std::collections::BTreeMap::new(),
            config_stamp: None,
            pending_agents: std::collections::BTreeMap::new(),
            session_titles: std::collections::BTreeMap::new(),
            agent_typing: Vec::new(),
            closing: None,
            armed: None,
            drawer: None,
            list_scroll: None,
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

    /// An agent tab takes the command's first word as its name, so it reads as
    /// the agent until renamed, and remembers the whole line to type.
    #[test]
    fn an_agent_tab_takes_the_commands_first_word_as_its_name() {
        let mut s = state();
        let ask = start_agent_terminal(&mut s, 7, "claude", None, None);
        match ask {
            ControlRequest::CreateTerminal { id, name, .. } => {
                assert_eq!(id, "agent-7");
                assert_eq!(name.as_deref(), Some("claude"));
            }
            other => panic!("an agent tab is a create: {other:?}"),
        }
        assert_eq!(
            s.pending_agents.get("agent-7").map(|(c, _)| c.as_str()),
            Some("claude")
        );
    }

    /// Flags stay on the typed line, off the tab name: the tab says the agent,
    /// the shell gets the whole command.
    #[test]
    fn an_agent_command_with_flags_keeps_the_whole_line() {
        let mut s = state();
        let ask = start_agent_terminal(&mut s, 7, "my-agent --resume", None, None);
        match ask {
            ControlRequest::CreateTerminal { name, .. } => {
                assert_eq!(name.as_deref(), Some("my-agent"));
            }
            other => panic!("an agent tab is a create: {other:?}"),
        }
        assert_eq!(
            s.pending_agents.get("agent-7").map(|(c, _)| c.as_str()),
            Some("my-agent --resume")
        );
    }

    /// A space's `+` starts its tab in that space and that space's directory, even when
    /// another space is on screen: `agent` lists the agents, `terminal` opens a shell, and
    /// the last agent row asks for any command, still for that space.
    #[test]
    fn a_spaces_plus_starts_a_terminal_or_an_agent_in_that_space() {
        let dir = std::env::temp_dir().display().to_string();
        let mut s = state();
        s.rows
            .iter_mut()
            .find(|r| r.terminal == 1)
            .expect("space 1 row")
            .cwd = Some(dir.clone());
        let mut request = 0;

        open_new(&mut s, 1);
        match apply_menu_choice(&mut s, 1, &mut request) {
            MenuSend::Control(ControlRequest::CreateTerminal {
                workspace,
                cwd,
                name,
                ..
            }) => {
                assert_eq!(workspace, Some(1));
                assert_eq!(cwd.as_deref(), Some(dir.as_str()));
                assert_eq!(name, None);
            }
            _ => panic!("terminal is a create"),
        }
        // Space 1 is not on screen, so the answer waits for the index to show the tab.
        let id = s.elsewhere.clone().expect("a create elsewhere is followed");
        let mut dirty = false;
        apply(
            &mut s,
            ControlResponse::TerminalCreated { id, terminal: 9 },
            &mut request,
            &mut dirty,
        );
        assert_eq!(s.elsewhere, None);
        assert_eq!(s.pending_focus, None);
        assert_eq!(s.show_when_listed.map(|(t, _, _)| t), Some(9));

        open_new(&mut s, 1);
        assert!(matches!(
            apply_menu_choice(&mut s, 0, &mut request),
            MenuSend::Nothing
        ));
        assert!(matches!(s.menu_kind, MenuKind::Agents { space: 1, .. }));
        assert_eq!(s.menu, Some(0));

        let agents = vec![crate::config::Launcher {
            name: "zed".to_string(),
            command: "claude --resume".to_string(),
        }];
        s.menu_kind = MenuKind::Agents {
            space: 1,
            name: "app".to_string(),
            agents: agents.clone(),
        };
        assert_eq!(menu_title(&s.menu_kind), " new agent in app");
        assert_eq!(
            menu_rows(&s.menu_kind),
            vec![
                ("zed".to_string(), "claude --resume".to_string()),
                ("other command".to_string(), String::new()),
            ]
        );
        // The agent is named first, then starts there.
        assert!(matches!(
            apply_menu_choice(&mut s, 0, &mut request),
            MenuSend::Nothing
        ));
        let named = s.prompt.take().expect("the agent's name box");
        assert!(named.what == Naming::AgentName(Some(1)));
        match answer_prompt(&mut s, named, None, &mut request).as_slice() {
            [
                ControlRequest::CreateTerminal {
                    id, workspace, cwd, ..
                },
            ] => {
                assert_eq!(*workspace, Some(1));
                assert_eq!(cwd.as_deref(), Some(dir.as_str()));
                assert_eq!(
                    s.pending_agents.get(id).map(|(c, _)| c.as_str()),
                    Some("claude --resume")
                );
            }
            _ => panic!("an agent is a create"),
        }

        s.menu_kind = MenuKind::Agents {
            space: 1,
            name: "app".to_string(),
            agents,
        };
        assert!(matches!(
            apply_menu_choice(&mut s, 1, &mut request),
            MenuSend::Nothing
        ));
        assert!(matches!(
            s.prompt.as_ref().map(|p| p.what),
            Some(Naming::Agent(Some(1)))
        ));
    }

    /// A `+` offers the person's own agents first, then the known agents on PATH that
    /// none of them already starts, and nothing that is not there.
    #[test]
    fn a_plus_offers_own_agents_then_the_known_ones_on_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        for name in ["claude", "codex", "not-an-agent"] {
            std::fs::write(dir.path().join(name), "").expect("write");
        }
        let own = vec![crate::config::Launcher {
            name: "yolo".to_string(),
            command: "claude --dangerously-skip-permissions".to_string(),
        }];
        let names: Vec<String> = agents_on(own, dir.path().as_os_str())
            .into_iter()
            .map(|l| l.name)
            .collect();
        assert_eq!(names, ["yolo", "codex"]);
    }

    /// The column drags out to half the window, and never below its narrowest.
    #[test]
    fn the_column_drags_to_half_the_window() {
        assert_eq!(sidebar::sidebar_max(200), 100);
        assert_eq!(sidebar::sidebar_max(20), sidebar::SIDEBAR_MIN);
    }

    /// The `TerminalCreated` answer for an agent queues its line with enter,
    /// and an answer for any other tab queues nothing.
    #[test]
    fn a_terminal_answer_for_an_agent_queues_its_line() {
        let mut s = state();
        s.pending_agents
            .insert("agent-7".to_string(), ("claude".to_string(), None));
        assert_eq!(
            take_agent_command(&mut s, "agent-7"),
            Some((b"claude\r".to_vec(), None))
        );
        assert!(s.pending_agents.is_empty(), "consumed, not kept");
        assert_eq!(take_agent_command(&mut s, "new-8"), None);
        s.pending_agents
            .insert("agent-9".to_string(), ("codex".to_string(), None));
        take_agent_command(&mut s, "agent-9");
        assert!(s.pending_agents.is_empty(), "a refusal drops the line");
    }

    /// Typing an agent row's text in the palette and pressing enter confirms an
    /// agent start rather than a move.
    #[test]
    fn picking_a_filtered_agent_row_confirms_an_agent_start() {
        let mut state = state();
        assert!(matches!(
            palette_key(&mut state, b"new claude terminal"),
            PaletteOutcome::Updated
        ));
        assert!(matches!(
            palette_key(&mut state, b"\r"),
            PaletteOutcome::Confirm(palette::Item::NewAgent(cmd)) if cmd == "claude"
        ));
        assert_eq!(state.palette, None, "confirming leaves the palette");
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

    /// A longer key sequence is skipped whole: only its first three bytes were, and
    /// `Delete` typed `~`, ctrl-up `;5A`, into the query.
    #[test]
    fn a_long_key_sequence_types_nothing_into_the_query() {
        let mut state = state();
        palette_key(&mut state, b"ma");
        palette_key(&mut state, b"\x1b[3~\x1b[1;5A\x1b[<0;5;5m");
        assert_eq!(state.palette.as_ref().map(|p| p.query.as_str()), Some("ma"));
        // An `ESC [` that never finishes does not hold the enter behind it.
        assert!(matches!(
            palette_key(&mut state, b"\x1b[\r"),
            PaletteOutcome::Confirm(_)
        ));
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

    /// The rows shown as news go to the daemon when the set moves, never twice the same,
    /// never another machine's, and an older daemon's refusal ends the telling.
    #[test]
    fn news_is_told_when_it_moves_and_only_this_machines() {
        let mut state = state();
        let mut request = 0;
        let remote = 1i64 << 40;
        state.rows = vec![
            agent(2, 1, "claude", "idle"),
            agent(3, 1, "claude", "idle"),
            agent(9, remote, "claude", "idle"),
        ];
        state.focused = 3;
        let told = |r: Option<ControlRequest>| match r {
            Some(ControlRequest::News { terminals, .. }) => terminals,
            other => panic!("nothing told: {other:?}"),
        };
        assert_eq!(
            told(news_request(&mut state, &mut request)),
            Vec::<u32>::new()
        );
        assert!(
            news_request(&mut state, &mut request).is_none(),
            "the same set twice"
        );
        // Finished while the keyboard was elsewhere, here and on another machine.
        for t in [2, 9] {
            state.unseen_finished.insert((t, 0));
        }
        assert_eq!(told(news_request(&mut state, &mut request)), vec![2]);
        // Looked at: news no more.
        state.focused = 2;
        assert_eq!(
            told(news_request(&mut state, &mut request)),
            Vec::<u32>::new()
        );
        state.focused = 3;
        apply(
            &mut state,
            ControlResponse::Failed {
                id: String::new(),
                reason: "unknown variant `News`".into(),
            },
            &mut 0,
            &mut false,
        );
        assert!(
            news_request(&mut state, &mut request).is_none(),
            "an old daemon is not asked again"
        );
    }

    /// The colours go to the daemon once both are known and again only when they change;
    /// an older daemon's refusal is swallowed and ends the telling for the session.
    #[test]
    fn host_colours_are_told_once_and_an_old_daemons_refusal_is_swallowed() {
        use crate::input::Report;
        let mut state = state();
        let mut request = 0;
        note_report(
            &mut state,
            Report::Color {
                background: true,
                rgb: [255, 255, 255],
            },
        );
        assert!(
            host_colors_request(&mut state, &mut request).is_none(),
            "half known"
        );
        note_report(
            &mut state,
            Report::Color {
                background: false,
                rgb: [0, 0, 0],
            },
        );
        assert!(matches!(
            host_colors_request(&mut state, &mut request),
            Some(ControlRequest::HostColors {
                fg: [0, 0, 0],
                bg: [255, 255, 255],
                ..
            })
        ));
        assert!(
            host_colors_request(&mut state, &mut request).is_none(),
            "unchanged"
        );
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::Failed {
                id: String::new(),
                reason: old_daemon_refusal(),
            },
            &mut 0,
            &mut dirty,
        );
        assert_eq!(state.message, None, "the refusal is not news to a person");
        state.host_colors[1] = Some([0, 0, 0]);
        assert!(
            host_colors_request(&mut state, &mut request).is_none(),
            "never again"
        );
        assert!(
            note_report(&mut state, Report::Focus(true)),
            "focus-in asks again"
        );
        assert!(!note_report(&mut state, Report::Scheme { light: true }));
        assert!(
            note_report(&mut state, Report::Scheme { light: false }),
            "a switch asks again"
        );
    }

    /// What a daemon from before `HostColors` answers it with, word for word.
    fn old_daemon_refusal() -> String {
        "this daemon is older than the client that called it: it does not know the method \
         that was asked for (unknown variant `HostColors`, expected one of `CreateTerminal`, \
         `ListFocusEvents` at line 1 column 18). stop the session and open it again to pick \
         up the new daemon; what is open comes back."
            .to_string()
    }

    /// A key whose sequence ends in `~` (Delete, the page keys, F5 and up) is read whole
    /// and dropped, so the keys after it still answer the box.
    #[test]
    fn a_tilde_key_in_the_naming_box_never_holds_what_follows() {
        let mut state = state();
        state.prompt = Some(Prompt::new(Naming::RenameTerminal(1), String::new()));
        assert!(matches!(
            prompt_key(&mut state, b"a\x1b[3~b"),
            PromptOutcome::Typing
        ));
        assert_eq!(state.prompt.as_ref().map(|p| p.text.as_str()), Some("ab"));
        assert!(matches!(
            prompt_key(&mut state, b"\x1b[6~"),
            PromptOutcome::Typing
        ));
        match prompt_key(&mut state, b"\r") {
            PromptOutcome::Confirm(_, Some(name), _) => assert_eq!(name, "ab"),
            _ => panic!("enter after a page key did not answer the box"),
        }
    }

    /// A rename opened over its name: enter with nothing typed keeps it and passes on what
    /// came after, ctrl-u starts the answer over, ctrl-r gives the default back, and a
    /// press on a button is its key.
    #[test]
    fn the_naming_box_keeps_clears_resets_and_takes_clicks() {
        let open = |state: &mut State| {
            state.prompt = Some(Prompt::new(
                Naming::RenameTerminal(1),
                "build logs".to_string(),
            ));
        };
        let mut state = state();
        open(&mut state);
        match prompt_key(&mut state, b"\rx") {
            PromptOutcome::Passthrough(rest) => assert_eq!(rest, b"x"),
            _ => panic!("an empty enter did not keep the name"),
        }
        assert!(state.prompt.is_none());

        open(&mut state);
        assert!(matches!(
            prompt_key(&mut state, b"abc\x15d"),
            PromptOutcome::Typing
        ));
        assert_eq!(state.prompt.as_ref().map(|p| p.text.as_str()), Some("d"));
        match prompt_key(&mut state, b"\r") {
            PromptOutcome::Confirm(p, Some(name), _) if p.what == Naming::RenameTerminal(1) => {
                assert_eq!(name, "d");
            }
            _ => panic!("a typed name was not confirmed"),
        }

        open(&mut state);
        assert!(matches!(
            prompt_key(&mut state, b"\x12"),
            PromptOutcome::Confirm(p, None, _) if p.what == Naming::RenameTerminal(1)
        ));
        // A space has no default name, so ctrl-r is nothing there.
        state.prompt = Some(Prompt::new(Naming::SpaceSettings(1), "work".to_string()));
        assert!(matches!(
            prompt_key(&mut state, b"\x12"),
            PromptOutcome::Typing
        ));

        (state.cols, state.lines) = (100, 24);
        open(&mut state);
        prompt_key(&mut state, b"ab");
        let reset =
            crate::screen::prompt_button(100, 24, state.prompt.as_ref().expect("open"), 0x12)
                .expect("a reset button");
        let click = format!("\x1b[<0;{};{}M", reset.1 + 1, reset.0 + 1);
        assert!(matches!(
            prompt_key(&mut state, click.as_bytes()),
            PromptOutcome::Confirm(p, None, _) if p.what == Naming::RenameTerminal(1)
        ));
    }

    /// The pick in the settings box of space 1, if it is open.
    fn pick(state: &State) -> Option<Option<u8>> {
        state.prompt.as_ref()?.ink.map(|pick| pick.now)
    }

    /// The settings box opens from the rule's `⋯`, from `ctrl-a R` and from the menu, over
    /// the space's name and the colour it has.
    #[test]
    fn the_settings_box_opens_from_the_rule_the_key_and_the_menu() {
        let mut s = state();
        (s.cols, s.lines) = (100, 30);
        s.spaces[0].ink = Some(4);
        // Settings is in the rule's drawer: out, it answers where it is painted.
        s.drawer = Some(Drawer {
            space: 1,
            shown: crate::sidebar::DRAWER,
            open: true,
        });
        let space = (1..s.lines)
            .flat_map(|row| (1..=crate::sidebar::WIDTH as u16).map(move |col| (row, col)))
            .find_map(|(row, col)| match s.clicked(row, col) {
                Some(Clicked::Settings(space)) => Some(space),
                _ => None,
            })
            .expect("a settings chip on a rule");
        assert_eq!(space, 1, "the first rule's chip is the first space's");
        s.prompt = Some(space_settings(&s, space));
        let open = s.prompt.as_ref().expect("open");
        assert!(open.what == Naming::SpaceSettings(1));
        assert_eq!(open.was, "app");
        assert_eq!(pick(&s), Some(Some(4)));
        assert_eq!(open.machine, None);

        assert_eq!(
            Keys::with_prefix(0x01).feed(b"\x01R"),
            vec![Action::SpaceSettings]
        );

        s.prompt = None;
        let row = MENU
            .iter()
            .position(|(name, ..)| *name == "space settings")
            .expect("a menu row");
        assert_eq!(MENU[row].1, "R");
        assert!(matches!(
            apply_menu_choice(&mut s, row, &mut 0),
            MenuSend::Nothing
        ));
        assert!(
            s.prompt
                .as_ref()
                .is_some_and(|p| p.what == Naming::SpaceSettings(2))
        );
    }

    /// A remote space's box names its machine and how it is reached.
    #[test]
    fn a_remote_spaces_settings_name_its_machine() {
        let mut s = state();
        s.spaces[1].name = "devbox:api".to_string();
        s.spaces[1].link = Some(qoro_runtime::control::LinkState::Parked);
        let open = space_settings(&s, 2);
        assert_eq!(
            open.machine,
            Some((
                "devbox".to_string(),
                qoro_runtime::control::LinkState::Parked
            ))
        );
    }

    /// Left and right walk the colours, 1 to 6 then auto and round, in both spellings and
    /// split across reads; a bare esc still cancels.
    #[test]
    fn the_arrows_walk_the_colours_round_through_auto() {
        let mut s = state();
        (s.cols, s.lines) = (100, 30);
        s.prompt = Some(space_settings(&s, 1));
        assert_eq!(pick(&s), Some(None));
        prompt_key(&mut s, b"\x1b[C");
        assert_eq!(pick(&s), Some(Some(0)));
        prompt_key(&mut s, b"\x1bOC\x1bOC");
        assert_eq!(pick(&s), Some(Some(2)));
        prompt_key(&mut s, b"\x1b[D\x1bOD\x1b[D");
        assert_eq!(pick(&s), Some(None), "left from 1 is auto");
        prompt_key(&mut s, b"\x1b[D");
        assert_eq!(pick(&s), Some(Some(5)), "left from auto is 6");
        assert!(matches!(
            prompt_key(&mut s, b"\x1bO"),
            PromptOutcome::Typing
        ));
        assert!(s.prompt.is_some(), "half an arrow closed the box");
        prompt_key(&mut s, b"C");
        assert_eq!(
            pick(&s),
            Some(None),
            "the held half and its tail are one arrow"
        );
        assert_eq!(s.prompt.as_ref().map(|p| p.text.as_str()), Some(""));
        assert!(matches!(prompt_key(&mut s, b"\x1b"), PromptOutcome::Closed));
        assert!(s.prompt.is_none());
    }

    /// A click on a swatch picks it, and on `auto` gives the id's colour back.
    #[test]
    fn a_swatch_click_picks_the_colour() {
        let mut s = state();
        (s.cols, s.lines) = (100, 30);
        s.prompt = Some(space_settings(&s, 1));
        for want in [Some(3), None] {
            let (row, col) =
                crate::screen::prompt_swatch(100, 30, s.prompt.as_ref().expect("open"), want)
                    .expect("swatches");
            let click = format!("\x1b[<0;{};{}M", col + 2, row + 1);
            assert!(matches!(
                prompt_key(&mut s, click.as_bytes()),
                PromptOutcome::Typing
            ));
            assert_eq!(pick(&s), Some(want));
        }
    }

    /// Enter with only the colour changed sends the colour and nothing else; with only the
    /// name changed, the rename; with nothing changed, nothing at all.
    #[test]
    fn saving_the_settings_sends_what_changed() {
        let mut s = state();
        (s.cols, s.lines) = (100, 30);
        let mut request = 0;
        s.prompt = Some(space_settings(&s, 1));
        prompt_key(&mut s, b"\x1b[C\x1b[C");
        let PromptOutcome::Confirm(prompt, name, _) = prompt_key(&mut s, b"\r") else {
            panic!("a colour picked over an empty name was not saved");
        };
        let asks = answer_prompt(&mut s, *prompt, name, &mut request);
        assert!(
            matches!(
                asks.as_slice(),
                [ControlRequest::SetWorkspaceInk {
                    workspace: 1,
                    ink: Some(1),
                    ..
                }]
            ),
            "{asks:?}"
        );

        s.prompt = Some(space_settings(&s, 1));
        prompt_key(&mut s, b"api");
        let PromptOutcome::Confirm(prompt, name, _) = prompt_key(&mut s, b"\r") else {
            panic!("a name was not saved");
        };
        let asks = answer_prompt(&mut s, *prompt, name, &mut request);
        assert!(
            matches!(asks.as_slice(), [ControlRequest::RenameWorkspace { workspace: 1, name, .. }] if name == "api"),
            "{asks:?}"
        );

        s.prompt = Some(space_settings(&s, 1));
        prompt_key(&mut s, b"\x1b[C\x1b[D");
        assert!(matches!(
            prompt_key(&mut s, b"\rx"),
            PromptOutcome::Passthrough(rest) if rest == b"x"
        ));
    }

    /// In a window too small for the colours the box is the plain rename, and the arrows
    /// pick nothing nobody can see.
    #[test]
    fn a_cramped_settings_box_picks_no_colour() {
        let mut s = state();
        (s.cols, s.lines) = (34, 30);
        s.prompt = Some(space_settings(&s, 1));
        prompt_key(&mut s, b"\x1b[C");
        assert_eq!(pick(&s), Some(None));
    }

    /// The menu sleeps and wakes the space on screen, for a terminal with no mouse.
    #[test]
    fn the_menu_sleeps_and_wakes_the_space_on_screen() {
        let mut s = state();
        let at = |name: &str| MENU.iter().position(|(n, ..)| *n == name).expect("a row");
        apply_menu_choice(&mut s, at("sleep space"), &mut 0);
        apply_menu_choice(&mut s, at("wake space"), &mut 0);
        assert!(
            matches!(
                s.modes.outbox.as_slice(),
                [
                    ControlRequest::SleepWorkspace {
                        workspace: 2,
                        terminal: None,
                        ..
                    },
                    ControlRequest::WakeWorkspace {
                        workspace: Some(2),
                        terminal: None,
                        ..
                    }
                ]
            ),
            "{:?}",
            s.modes.outbox
        );
    }

    /// Every agent start asks its name: empty keeps the command's first word, a name goes
    /// on the tab, and esc leaves no command behind.
    #[test]
    fn an_agent_is_named_before_it_starts() {
        let mut s = state();
        let mut request = 0;
        s.prompt = Some(Prompt::new(Naming::Agent(Some(1)), ""));
        prompt_key(&mut s, b"claude --resume");
        let PromptOutcome::Confirm(prompt, name, _) = prompt_key(&mut s, b"\r") else {
            panic!("the command was not taken");
        };
        assert!(answer_prompt(&mut s, *prompt, name, &mut request).is_empty());
        let open = s.prompt.as_ref().expect("the name box");
        assert!(open.what == Naming::AgentName(Some(1)));
        assert_eq!(open.command.as_deref(), Some("claude --resume"));

        let PromptOutcome::Confirm(prompt, name, _) = prompt_key(&mut s, b"\r") else {
            panic!("an empty name did not start the agent");
        };
        let asks = answer_prompt(&mut s, *prompt, name, &mut request);
        let [
            ControlRequest::CreateTerminal {
                id,
                name,
                workspace,
                ..
            },
        ] = asks.as_slice()
        else {
            panic!("{asks:?}");
        };
        assert_eq!(name.as_deref(), Some("claude"));
        assert_eq!(*workspace, Some(1));
        assert_eq!(
            s.pending_agents.get(id).map(|(c, _)| c.as_str()),
            Some("claude --resume")
        );
        assert_eq!(
            s.pending_agents.get(id).and_then(|(_, n)| n.as_deref()),
            None,
            "a default name is not owed to anyone"
        );

        s.prompt = Some(agent_name(None, "codex".to_string()));
        prompt_key(&mut s, b"review");
        let PromptOutcome::Confirm(prompt, name, _) = prompt_key(&mut s, b"\r") else {
            panic!("a name did not start the agent");
        };
        let asks = answer_prompt(&mut s, *prompt, name, &mut request);
        let [ControlRequest::CreateTerminal { id, name, .. }] = asks.as_slice() else {
            panic!("{asks:?}");
        };
        assert_eq!(name.as_deref(), Some("review"));
        assert_eq!(
            s.pending_agents.get(id).and_then(|(_, n)| n.as_deref()),
            Some("review")
        );

        s.prompt = Some(agent_name(None, "codex".to_string()));
        assert!(matches!(prompt_key(&mut s, b"\x1b"), PromptOutcome::Closed));
        assert!(s.prompt.is_none());
    }

    /// A name given at start goes on the agent's conversation once, the first time the
    /// index shows it in one; never again, and never for a tab that closed first.
    #[test]
    fn a_given_name_goes_on_the_conversation_once() {
        let mut s = state();
        let mut request = 10;
        let mut dirty = false;
        start_agent_terminal(&mut s, 3, "claude", Some(1), Some("api".to_string()));
        start_agent_terminal(&mut s, 4, "codex", Some(1), Some("gone".to_string()));
        for (id, terminal) in [("agent-3", 7), ("agent-4", 8)] {
            apply(
                &mut s,
                ControlResponse::TerminalCreated {
                    id: id.into(),
                    terminal,
                },
                &mut request,
                &mut dirty,
            );
        }
        assert!(s.pending_agents.is_empty());
        apply(
            &mut s,
            ControlResponse::TerminalClosed {
                id: String::new(),
                terminal: 8,
            },
            &mut request,
            &mut dirty,
        );
        let index = |session: Option<&str>| {
            let mut rows = vec![agent(7, 1, "claude", "idle"), agent(8, 1, "codex", "idle")];
            for row in &mut rows {
                row.agent_session = session.map(str::to_string);
            }
            ControlResponse::Agents {
                id: "index-1".into(),
                agents: rows,
                splits: Vec::new(),
            }
        };
        s.modes.outbox.clear();
        apply(&mut s, index(None), &mut request, &mut dirty);
        assert!(
            s.modes.outbox.is_empty(),
            "named before the agent said its session"
        );
        for _ in 0..2 {
            apply(&mut s, index(Some("abc")), &mut request, &mut dirty);
        }
        let titles: Vec<_> = s
            .modes
            .outbox
            .iter()
            .filter_map(|ask| match ask {
                ControlRequest::RenameSession {
                    id,
                    terminal,
                    title,
                } => Some((id.starts_with("title-"), *terminal, title.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(titles, [(true, 7, Some("api".to_string()))]);
        // A refusal of it is not said: nobody asked.
        s.hint = None;
        s.message = None;
        apply(
            &mut s,
            ControlResponse::Failed {
                id: "title-11".into(),
                reason: "unknown variant `RenameSession`".into(),
            },
            &mut request,
            &mut dirty,
        );
        assert_eq!((s.hint.as_deref(), s.message.as_deref()), (None, None));
    }

    /// With another prefix, every overlay hands that one back and treats ctrl-a as the
    /// person's own key, and a paste holding a ctrl-a reaches the pane untouched.
    #[test]
    fn a_chosen_prefix_is_the_one_every_overlay_hands_back() {
        let mut state = state();
        state.config.prefix = 0x02;
        match palette_key(&mut state, b"\x02d") {
            PaletteOutcome::Passthrough(rest) => assert_eq!(rest, b"\x02d"),
            other => panic!("the palette kept the prefix: {other:?}"),
        }
        state.palette = Some(Palette::open());
        assert!(matches!(
            palette_key(&mut state, b"\x02 "),
            PaletteOutcome::Closed
        ));
        state.menu = Some(0);
        match menu_key(&mut state, 0, b"\x02c") {
            MenuOutcome::Passthrough(rest) => assert_eq!(rest, b"\x02c"),
            _ => panic!("the menu kept the prefix"),
        }
        state.prompt = Some(Prompt::new(Naming::Terminal, ""));
        match prompt_key(&mut state, b"\x02d") {
            PromptOutcome::Passthrough(rest) => assert_eq!(rest, b"\x02d"),
            _ => panic!("the naming box kept the prefix"),
        }
        state.menu = Some(0);
        assert!(
            !matches!(
                menu_key(&mut state, 0, b"\x01"),
                MenuOutcome::Passthrough(_)
            ),
            "ctrl-a is an ordinary key to the menu now"
        );
        let paste = b"line one\x01\nline two\n".to_vec();
        let mut keys = Keys::with_prefix(0x02);
        assert_eq!(keys.feed(&paste), vec![Action::Input(paste.clone())]);
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

    #[test]
    fn the_keys_search_types_ends_on_esc_and_hands_a_prefix_back() {
        let mut state = state();
        state.palette = None;
        state.reading = Some(Reading::Keybinds);
        state.keys_query = Some(String::new());
        assert_eq!(keys_search(&mut state, b"new"), None);
        assert_eq!(state.keys_query.as_deref(), Some("new"));
        assert_eq!(keys_search(&mut state, b"\x7f"), None);
        assert_eq!(state.keys_query.as_deref(), Some("ne"));
        // An arrow is skipped whole and the wheel does nothing.
        assert_eq!(keys_search(&mut state, b"\x1b[A\x1b[<64;5;5M"), None);
        assert_eq!(state.keys_query.as_deref(), Some("ne"));
        // Two escs in one read: the search ends, and the second is a key for the decoder,
        // which closes the list as any key does.
        assert_eq!(keys_search(&mut state, b"\x1b\x1b"), Some(b"\x1b".to_vec()));
        assert_eq!(state.keys_query, None);
        assert!(state.reading.is_some());
        // The prefix is handed back whole, typing nothing into the search.
        state.keys_query = Some("x".to_string());
        assert_eq!(keys_search(&mut state, b"\x01d"), Some(b"\x01d".to_vec()));
        assert_eq!(state.keys_query, None);
        // An unfinished `ESC [` ends at the next control byte, which is read as itself:
        // the enter after it still closes the list.
        state.keys_query = Some(String::new());
        assert_eq!(keys_search(&mut state, b"\x1b[\r"), None);
        assert!(state.reading.is_none());
        state.reading = Some(Reading::Keybinds);
        // A click closes the list; enter does too.
        state.keys_query = Some(String::new());
        assert_eq!(keys_search(&mut state, b"\x1b[<0;5;5M"), None);
        assert!(state.reading.is_none());
        state.reading = Some(Reading::Keybinds);
        state.keys_query = Some("z".to_string());
        assert_eq!(keys_search(&mut state, b"\r"), None);
        assert!(state.reading.is_none() && state.keys_query.is_none());
    }

    #[test]
    fn tab_walks_the_palette_filters_and_a_pasted_tab_does_not() {
        let mut state = state();
        let filter = |state: &State| state.palette.as_ref().map(|p| p.filter);
        palette_key(&mut state, b"\t");
        assert_eq!(filter(&state), Some(palette::Filter::Tabs));
        // Shift-tab walks back, past `all` round to the last.
        palette_key(&mut state, b"\x1b[Z");
        palette_key(&mut state, b"\x1b[Z");
        assert_eq!(filter(&state), Some(palette::Filter::Actions));
        // A tab inside a paste is not the key: the filter stays and the text is typed.
        palette_key(&mut state, b"ab\tc");
        assert_eq!(filter(&state), Some(palette::Filter::Actions));
        assert_eq!(
            state.palette.as_ref().map(|p| p.query.as_str()),
            Some("abc")
        );
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
            close_begun: false,
            rows: Vec::new(),
            focused: 1,
            snapshot: None,
            snapshots: std::collections::BTreeMap::new(),
            last_seen: std::collections::BTreeMap::new(),
            previewing: false,
            images: std::collections::BTreeMap::new(),
            graphics: crate::graphics::GraphicsCap::off(),
            shown_images: std::collections::BTreeMap::new(),
            img_size: (0, 0),
            subs: std::collections::BTreeSet::new(),
            sized: std::collections::BTreeMap::new(),
            layouts: std::collections::BTreeMap::new(),
            parked: std::collections::BTreeMap::new(),
            split_writes: Vec::new(),
            split_saving: 0,
            resync: false,
            split_origin: std::collections::BTreeMap::new(),
            config_stamp: None,
            pending_agents: std::collections::BTreeMap::new(),
            session_titles: std::collections::BTreeMap::new(),
            agent_typing: Vec::new(),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            pending_focus: None,
            last: LastFocus::default(),
            show_when_listed: None,
            elsewhere: None,
            waking: None,
            dim: 0,
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
            inbox: sidebar::Inbox::default(),
            inbox_drag: None,

            shut: sidebar::Shut::new(),

            in_column: false,

            prefix_pending: false,

            answer: None,

            usage: sidebar::Usage::new(),

            usage_seen: std::collections::BTreeMap::new(),
            seen_done: sidebar::Seen::new(),
            unseen_finished: sidebar::Unseen::new(),
            tick: 0,
            paint_now: 0,
            sidebar_width: sidebar::WIDTH,
            sidebar_drag: None,
            row_press: None,
            drop_after_move: None,
            sidebar_sized: sidebar::WIDTH,
            palette: None,
            palette_pending: Vec::new(),
            keys_query: None,
            menu_kind: MenuKind::Main,
            connecting: None,
            probing: None,
            installing: None,
            reprobe: None,
            joining: None,
            modes: Modes::default(),
            selection: None,
            forwarded_press: false,
            anchor: None,
            behind: Behind::default(),
            outer_focus: None,
            host_colors: [None, None],
            host_light: None,
            icon_reply: crate::icons::Reply::Silent,
            icon_font: false,
            host_sent: None,
            host_refused: false,
            news_sent: None,
            news_refused: false,
            notes: Vec::new(),
            notes_due: None,
            route: crate::notify::Route::Nowhere(crate::notify::Why::NoProgram),
            nowhere: false,
            config: crate::config::Config::default(),
            closing: None,
            armed: None,
            drawer: None,
            list_scroll: None,
        }
    }

    fn snapshot_with_behind(behind: u64) -> SnapshotWire {
        SnapshotWire {
            cols: 80,
            rows: 24,
            cursor: (0, 0),
            lines: Vec::new(),
            styles: Vec::new(),
            links: Vec::new(),
            modes: Default::default(),
            behind,
        }
    }

    fn tiled_state() -> State {
        // Space 1 with two terminals side by side, 1 focused, both watched.
        let mut state = State {
            armed: None,
            drawer: None,
            list_scroll: None,
            closing: None,
            close_begun: false,
            rows: vec![agent(1, 1, "zsh", "idle"), agent(2, 1, "claude", "working")],
            focused: 1,
            snapshot: Some(snapshot_with_behind(0)),
            snapshots: std::collections::BTreeMap::from([(2, snapshot_with_behind(0))]),
            last_seen: std::collections::BTreeMap::new(),
            previewing: false,
            images: std::collections::BTreeMap::new(),
            graphics: crate::graphics::GraphicsCap::off(),
            shown_images: std::collections::BTreeMap::new(),
            img_size: (0, 0),
            subs: std::collections::BTreeSet::from([1, 2]),
            sized: std::collections::BTreeMap::new(),
            layouts: std::collections::BTreeMap::new(),
            parked: std::collections::BTreeMap::new(),
            split_writes: Vec::new(),
            split_saving: 0,
            resync: false,
            split_origin: std::collections::BTreeMap::new(),
            config_stamp: None,
            pending_agents: std::collections::BTreeMap::new(),
            session_titles: std::collections::BTreeMap::new(),
            agent_typing: Vec::new(),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            pending_focus: None,
            last: LastFocus::default(),
            show_when_listed: None,
            elsewhere: None,
            waking: None,
            dim: 0,
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
            inbox: sidebar::Inbox::default(),
            inbox_drag: None,

            shut: sidebar::Shut::new(),

            in_column: false,

            prefix_pending: false,

            answer: None,

            usage: sidebar::Usage::new(),

            usage_seen: std::collections::BTreeMap::new(),
            seen_done: sidebar::Seen::new(),
            unseen_finished: sidebar::Unseen::new(),
            tick: 0,
            paint_now: 0,
            sidebar_width: sidebar::WIDTH,
            sidebar_drag: None,
            row_press: None,
            drop_after_move: None,
            sidebar_sized: sidebar::WIDTH,
            palette: None,
            palette_pending: Vec::new(),
            keys_query: None,
            menu_kind: MenuKind::Main,
            connecting: None,
            probing: None,
            installing: None,
            reprobe: None,
            joining: None,
            modes: Modes::default(),
            selection: None,
            forwarded_press: false,
            anchor: None,
            behind: Behind::default(),
            outer_focus: None,
            host_colors: [None, None],
            host_light: None,
            icon_reply: crate::icons::Reply::Silent,
            icon_font: false,
            host_sent: None,
            host_refused: false,
            news_sent: None,
            news_refused: false,
            notes: Vec::new(),
            notes_due: None,
            route: crate::notify::Route::Nowhere(crate::notify::Why::NoProgram),
            nowhere: false,
            config: crate::config::Config::default(),
        };
        let area = pane_area(&state);
        let mut layout = Layout::single(1);
        assert!(layout.split_placeholder(1, layout::Dir::V, area));
        assert!(layout.bind_placeholder(2));
        layout.focus_terminal(1);
        state.layouts.insert(1, layout);
        state
    }

    /// A reorder's answer leaves the tab where it is: its split stays whole and the keyboard
    /// stays on it, even when an index already said the same space. An answer to a move
    /// that was not a reorder still takes the tab out of its split.
    #[test]
    fn a_reorders_answer_keeps_the_split_and_the_keyboard() {
        let mut state = tiled_state();
        let before = state.layouts.get(&1).cloned();
        let focused = state.focused;
        let (mut request, mut dirty) = (0, false);
        let moved = |id: &str, terminal: u32| ControlResponse::TerminalMoved {
            id: id.into(),
            terminal,
            workspace: 1,
        };
        // The split's first pane moved the whole split.
        apply(&mut state, moved("reorder-1", 1), &mut request, &mut dirty);
        assert!(dirty);
        assert_eq!(state.message.as_deref(), Some("reordered"));
        assert_eq!(
            state.layouts.get(&1).cloned(),
            before,
            "the split stayed whole"
        );
        assert_eq!(state.focused, focused);
        // Not asked as a reorder: the old path, which takes it out of its split.
        apply(&mut state, moved("move-2", 2), &mut request, &mut dirty);
        assert!(
            !state
                .layouts
                .get(&1)
                .is_some_and(|l| l.leaves().contains(&2)),
            "a moved tab leaves its split"
        );
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
    fn a_tab_switch_parks_the_split_and_the_way_back_brings_it_back() {
        let mut state = tiled_state();
        state.rows.push(agent(3, 1, "sh", "idle"));
        let mut seqs = std::collections::BTreeMap::new();
        let mut control_seq = 0;
        focus(&mut state, 3, &mut control_seq, &mut seqs).unwrap();
        assert_eq!(state.layouts.get(&1).map(|l| l.leaves()), Some(vec![3]));
        assert_eq!(state.subs, std::collections::BTreeSet::from([3]));
        // The split's pane is still a pane, not a tab and not lost.
        let tabs: Vec<u32> = state.tabs().iter().map(|r| r.terminal).collect();
        assert_eq!(tabs, vec![1, 3]);
        focus(&mut state, 1, &mut control_seq, &mut seqs).unwrap();
        assert_eq!(state.layouts.get(&1).map(|l| l.leaves()), Some(vec![1, 2]));
        assert_eq!(state.subs, std::collections::BTreeSet::from([1, 2]));
        assert!(
            state.split_writes.is_empty(),
            "moving between tabs writes nothing"
        );
    }

    /// A tab come back to draws the screen it had while the fresh one crosses the link,
    /// rather than a blank pane, and only draws it: every decision still sees no screen,
    /// so the subscription that brings the live one goes out as before.
    #[test]
    fn a_tab_come_back_to_draws_its_last_screen_until_the_live_one_lands() {
        let mut state = tiled_state();
        state.rows.push(agent(3, 1, "sh", "idle"));
        let mut seqs = std::collections::BTreeMap::new();
        let mut control_seq = 0;
        focus(&mut state, 3, &mut control_seq, &mut seqs).unwrap();
        assert!(
            state.snapshots.is_empty(),
            "a hidden pane's screen stayed live"
        );
        assert!(state.last_seen.contains_key(&1) && state.last_seen.contains_key(&2));
        // Never seen here: nothing to stand in.
        assert!(state.frame().snapshot.is_none());

        focus(&mut state, 1, &mut control_seq, &mut seqs).unwrap();
        assert!(state.snapshot.is_none(), "the old screen reached the logic");
        assert_eq!(state.subs, std::collections::BTreeSet::from([1, 2]));
        let frame = state.frame();
        assert!(frame.snapshot.is_some(), "the pane came back blank");
        assert!(
            frame.tiles.iter().all(|t| t.snapshot.is_some()),
            "a tile of the split came back blank"
        );
    }

    /// A pane emptied for something new (here a new space, with the keyboard still on the
    /// old terminal) shows nothing, never the screen it held.
    #[test]
    fn a_pane_emptied_for_a_new_space_never_shows_its_old_screen() {
        let mut state = tiled_state();
        state.rows.push(agent(3, 1, "sh", "idle"));
        let mut seqs = std::collections::BTreeMap::new();
        let mut control_seq = 0;
        focus(&mut state, 3, &mut control_seq, &mut seqs).unwrap();
        focus(&mut state, 1, &mut control_seq, &mut seqs).unwrap();
        assert!(state.frame().snapshot.is_some());
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::WorkspaceCreated {
                id: "x".to_string(),
                workspace: 9,
            },
            &mut 0,
            &mut dirty,
        );
        assert!(state.frame().snapshot.is_none(), "the old screen stood in");
    }

    /// Leaving for a space with nothing to focus keeps every screen as last seen.
    #[test]
    fn unfocusing_keeps_the_screens_as_last_seen() {
        let mut state = tiled_state();
        unfocus(&mut state, "nothing here", &mut 0).unwrap();
        assert!(state.last_seen.contains_key(&1) && state.last_seen.contains_key(&2));
        assert!(state.snapshots.is_empty());
    }

    /// A terminal no longer listed takes its last screen with it, as a remote's do when
    /// that remote drops.
    #[test]
    fn a_terminal_gone_from_the_list_leaves_no_last_screen() {
        let mut state = tiled_state();
        state.rows.push(agent(3, 1, "sh", "idle"));
        let mut seqs = std::collections::BTreeMap::new();
        focus(&mut state, 3, &mut 0, &mut seqs).unwrap();
        assert!(state.last_seen.contains_key(&2));
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::Agents {
                id: "x".to_string(),
                agents: vec![agent(1, 1, "zsh", "idle"), agent(3, 1, "sh", "idle")],
                splits: Vec::new(),
            },
            &mut 0,
            &mut dirty,
        );
        assert!(!state.last_seen.contains_key(&2));
        assert!(state.last_seen.contains_key(&1));
    }

    /// A terminal that goes takes its last screen with it.
    #[test]
    fn a_closed_terminal_leaves_no_last_screen() {
        let mut state = tiled_state();
        state.rows.push(agent(3, 1, "sh", "idle"));
        let mut seqs = std::collections::BTreeMap::new();
        focus(&mut state, 3, &mut 0, &mut seqs).unwrap();
        assert!(state.last_seen.contains_key(&2));
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::TerminalClosed {
                id: "x".to_string(),
                terminal: 2,
            },
            &mut 0,
            &mut dirty,
        );
        assert!(!state.last_seen.contains_key(&2));
    }

    #[test]
    fn a_pane_picked_from_the_sidebar_brings_its_whole_tab() {
        let mut state = tiled_state();
        state.rows.push(agent(3, 1, "sh", "idle"));
        let mut seqs = std::collections::BTreeMap::new();
        let mut control_seq = 0;
        focus(&mut state, 3, &mut control_seq, &mut seqs).unwrap();
        focus(&mut state, 2, &mut control_seq, &mut seqs).unwrap();
        assert_eq!(state.layouts.get(&1).map(|l| l.leaves()), Some(vec![1, 2]));
        assert_eq!(state.focused, 2);
    }

    #[test]
    fn closing_the_tabs_own_terminal_leaves_the_rest_as_one_tab() {
        let mut state = tiled_state();
        let area = pane_area(&state);
        let layout = state.layouts.get_mut(&1).unwrap();
        assert!(layout.split_placeholder(2, layout::Dir::H, area));
        assert!(layout.bind_placeholder(3));
        state.rows.push(agent(3, 1, "sh", "idle"));
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::TerminalClosed {
                id: "x".to_string(),
                terminal: 1,
            },
            &mut 0,
            &mut dirty,
        );
        state.rows.retain(|r| r.terminal != 1);
        let tabs: Vec<u32> = state.tabs().iter().map(|r| r.terminal).collect();
        assert_eq!(tabs, vec![2], "the next pane is the tab now");
        assert!(state.is_pane(3));
    }

    #[test]
    fn a_split_answered_after_a_tab_switch_binds_into_the_parked_tree() {
        let mut state = tiled_state();
        state.rows.push(agent(3, 1, "sh", "idle"));
        let area = pane_area(&state);
        assert!(
            state
                .layouts
                .get_mut(&1)
                .unwrap()
                .split_placeholder(2, layout::Dir::H, area)
        );
        state.split_origin.insert("split-4".to_string(), (1, 2));
        let mut seqs = std::collections::BTreeMap::new();
        let mut control_seq = 0;
        focus(&mut state, 3, &mut control_seq, &mut seqs).unwrap();
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::TerminalCreated {
                id: "split-4".to_string(),
                terminal: 4,
            },
            &mut 0,
            &mut dirty,
        );
        let parked: Vec<Vec<u32>> = state.parked[&1].iter().map(|l| l.leaves()).collect();
        assert_eq!(parked, vec![vec![1, 2, 4]]);
        assert!(state.is_pane(4));
        assert_eq!(
            state.split_writes.len(),
            1,
            "the new split is recorded once"
        );
        assert_eq!(state.split_writes[0].leaves(), vec![1, 2, 4]);
        assert_eq!(
            state.pending_focus, None,
            "the keyboard stays on the tab it moved to"
        );
    }

    #[test]
    fn two_tabs_splitting_at_once_each_get_their_own_pane() {
        let mut state = tiled_state();
        state.rows.push(agent(3, 1, "sh", "idle"));
        let area = pane_area(&state);
        assert!(
            state
                .layouts
                .get_mut(&1)
                .unwrap()
                .split_placeholder(2, layout::Dir::H, area)
        );
        state.split_origin.insert("split-4".to_string(), (1, 2));
        let mut seqs = std::collections::BTreeMap::new();
        let mut control_seq = 0;
        focus(&mut state, 3, &mut control_seq, &mut seqs).unwrap();
        assert!(
            state
                .layouts
                .get_mut(&1)
                .unwrap()
                .split_placeholder(3, layout::Dir::V, area)
        );
        state.split_origin.insert("split-5".to_string(), (1, 3));
        let mut dirty = false;
        // The first tab's answer arrives while the second tab is on screen.
        apply(
            &mut state,
            ControlResponse::TerminalCreated {
                id: "split-4".to_string(),
                terminal: 4,
            },
            &mut 0,
            &mut dirty,
        );
        apply(
            &mut state,
            ControlResponse::TerminalCreated {
                id: "split-5".to_string(),
                terminal: 5,
            },
            &mut 0,
            &mut dirty,
        );
        assert_eq!(state.layouts[&1].leaves(), vec![3, 5]);
        let parked: Vec<Vec<u32>> = state.parked[&1].iter().map(|l| l.leaves()).collect();
        assert_eq!(parked, vec![vec![1, 2, 4]]);
    }

    #[test]
    fn a_space_answered_after_the_index_still_brings_the_split_on_screen() {
        let mut state = fresh_client_with_one_pane_on_screen();
        state.layouts.clear();
        state.subs.clear();
        state.space = None;
        let mut dirty = false;
        apply(
            &mut state,
            index_with(vec![three_panes()]),
            &mut 0,
            &mut dirty,
        );
        assert!(state.layouts.is_empty(), "no space yet, no tree on screen");
        apply(
            &mut state,
            ControlResponse::Workspaces {
                id: "spaces".to_string(),
                workspaces: vec![space(1, "app")],
            },
            &mut 0,
            &mut dirty,
        );
        assert_eq!(
            state.layouts.get(&1).map(|l| l.leaves()),
            Some(vec![1, 2, 3])
        );
        assert!(state.resync, "the other panes are owed their subscriptions");
        assert!(state.adopted_resize, "and the focused one its smaller size");
    }

    #[test]
    fn a_bound_split_is_recorded_once() {
        let mut state = tiled_state();
        let area = pane_area(&state);
        state
            .layouts
            .get_mut(&1)
            .unwrap()
            .split_placeholder(2, layout::Dir::H, area);
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::TerminalCreated {
                id: "split-5".to_string(),
                terminal: 5,
            },
            &mut 0,
            &mut dirty,
        );
        let written: Vec<Vec<u32>> = state.split_writes.iter().map(|t| t.leaves()).collect();
        assert_eq!(written, vec![vec![1, 2, 5]]);
    }

    /// A restart: the daemon's index names three panes of one tab, and a fresh
    /// client that has only seen one of them on screen takes the whole tab.
    fn fresh_client_with_one_pane_on_screen() -> State {
        let mut state = tiled_state();
        state.layouts.clear();
        state.layouts.insert(1, Layout::single(1));
        state.subs = std::collections::BTreeSet::from([1]);
        state
    }

    fn three_panes() -> qoro_runtime::control::Pane {
        use qoro_runtime::control::{Pane, SplitDir};
        let leaf = |terminal| Pane::Leaf { terminal };
        Pane::Split {
            dir: SplitDir::V,
            permille: 500,
            first: Box::new(leaf(1)),
            second: Box::new(Pane::Split {
                dir: SplitDir::H,
                permille: 500,
                first: Box::new(leaf(2)),
                second: Box::new(leaf(3)),
            }),
        }
    }

    fn index_with(splits: Vec<qoro_runtime::control::Pane>) -> ControlResponse {
        ControlResponse::Agents {
            id: "index".to_string(),
            agents: vec![
                agent(1, 1, "claude", "idle"),
                agent(2, 1, "claude", "idle"),
                agent(3, 1, "claude", "idle"),
            ],
            splits,
        }
    }

    #[test]
    fn a_split_in_the_index_comes_back_as_one_tab() {
        let mut state = fresh_client_with_one_pane_on_screen();
        let mut dirty = false;
        apply(
            &mut state,
            index_with(vec![three_panes()]),
            &mut 0,
            &mut dirty,
        );
        assert_eq!(
            state.layouts.get(&1).map(|l| l.leaves()),
            Some(vec![1, 2, 3])
        );
        let tabs: Vec<u32> = state.tabs().iter().map(|r| r.terminal).collect();
        assert_eq!(tabs, vec![1], "one tab, not three");
        assert!(
            state.resync,
            "the other panes still need their subscriptions"
        );
        assert!(
            state.split_writes.is_empty(),
            "adopting writes nothing back"
        );
        let mut control_seq = 0;
        assert!(sync_subs(&mut state, &mut control_seq).unwrap().is_some());
        assert_eq!(state.subs, std::collections::BTreeSet::from([1, 2, 3]));
    }

    #[test]
    fn a_split_for_a_tab_not_on_screen_is_parked() {
        let mut state = fresh_client_with_one_pane_on_screen();
        state.rows.push(agent(9, 1, "sh", "idle"));
        state.layouts.insert(1, Layout::single(9));
        state.focused = 9;
        let mut dirty = false;
        let mut index = index_with(vec![three_panes()]);
        if let ControlResponse::Agents { agents, .. } = &mut index {
            agents.push(agent(9, 1, "sh", "idle"));
        }
        apply(&mut state, index, &mut 0, &mut dirty);
        assert_eq!(state.layouts.get(&1).map(|l| l.leaves()), Some(vec![9]));
        let tabs: Vec<u32> = state.tabs().iter().map(|r| r.terminal).collect();
        assert_eq!(tabs, vec![1, 9]);
    }

    #[test]
    fn a_split_this_client_holds_is_not_replaced_by_the_index() {
        let mut state = tiled_state();
        state.rows.push(agent(3, 1, "sh", "idle"));
        let mut dirty = false;
        apply(
            &mut state,
            index_with(vec![three_panes()]),
            &mut 0,
            &mut dirty,
        );
        assert_eq!(state.layouts.get(&1).map(|l| l.leaves()), Some(vec![1, 2]));
        assert!(!state.is_pane(3), "the local tree wins; 3 stays a tab");
    }

    #[test]
    fn nothing_is_adopted_while_a_recording_is_on_its_way() {
        let mut state = fresh_client_with_one_pane_on_screen();
        state.split_saving = 1;
        let mut dirty = false;
        apply(
            &mut state,
            index_with(vec![three_panes()]),
            &mut 0,
            &mut dirty,
        );
        assert_eq!(state.layouts.get(&1).map(|l| l.leaves()), Some(vec![1]));
        apply(
            &mut state,
            ControlResponse::SplitSaved {
                id: "tree-1".to_string(),
            },
            &mut 0,
            &mut dirty,
        );
        assert_eq!(state.split_saving, 0);
    }

    #[test]
    fn a_parked_tree_left_with_one_pane_is_a_tab_again() {
        let mut state = tiled_state();
        state.rows.push(agent(3, 1, "sh", "idle"));
        let mut seqs = std::collections::BTreeMap::new();
        let mut control_seq = 0;
        focus(&mut state, 3, &mut control_seq, &mut seqs).unwrap();
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::TerminalClosed {
                id: "x".to_string(),
                terminal: 1,
            },
            &mut 0,
            &mut dirty,
        );
        state.rows.retain(|r| r.terminal != 1);
        assert!(state.parked[&1].is_empty());
        let tabs: Vec<u32> = state.tabs().iter().map(|r| r.terminal).collect();
        assert_eq!(tabs, vec![2, 3]);
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
            &mut 0,
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
            &mut 0,
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
        // The pane's corner is the frame around the split, and belongs to no pane.
        assert_eq!(
            state.tile_at_screen((screen::TABS + 1) as u16, first_col as u16),
            None
        );
        // One cell in from it is terminal 1's top-left cell.
        let hit = state.tile_at_screen((screen::TABS + 2) as u16, (first_col + 1) as u16);
        assert_eq!(hit.map(|h| h.0), Some(1));
        // The divider column between the two tiles belongs to no pane.
        let rects = state.layouts.get(&1).unwrap().tile_rects(pane_area(&state));
        let left_rect = rects.iter().find(|(t, _)| *t == 1).unwrap().1;
        let divider_col = (first_col + left_rect.x + left_rect.cols) as u16;
        assert_eq!(
            state.tile_at_screen((screen::TABS + 2) as u16, divider_col),
            None
        );
        // And the focused tile answers tile-local coordinates.
        assert_eq!(
            state.in_pane((screen::TABS + 2) as u16, (first_col + 1) as u16),
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

    /// A visible terminal with no screen here is subscribed again although it already is,
    /// which the daemon answers with its live screen: back in a space whose copy was dropped,
    /// the pane is not left blank until the terminal prints.
    #[test]
    fn a_visible_terminal_with_no_screen_is_asked_for_one_again() {
        let mut state = tiled_state();
        let mut control_seq = 0;
        assert!(sync_subs(&mut state, &mut control_seq).unwrap().is_none());
        state.forget_screen();
        let out = sync_subs(&mut state, &mut control_seq)
            .unwrap()
            .expect("a subscribe");
        assert!(String::from_utf8_lossy(&out).contains("show-tiles"));
        assert!(!String::from_utf8_lossy(&out).contains("hide-tiles"));
        assert_eq!(state.subs, std::collections::BTreeSet::from([1, 2]));
    }

    /// A new space starts bare, and the old space's subscriptions are dropped by the next
    /// sync, with an unsubscribe that reaches the daemon. Forgotten locally instead, the
    /// daemon kept them, and coming back subscribed to a terminal it still watched, which
    /// asks for no screen: the first space came back blank.
    #[test]
    fn a_new_space_starts_bare_and_its_first_sync_unsubscribes_the_old_one() {
        let mut state = tiled_state();
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::WorkspaceCreated {
                id: "w".to_string(),
                workspace: 2,
            },
            &mut 0,
            &mut dirty,
        );
        assert!(!state.layouts.contains_key(&2));
        assert!(state.snapshots.is_empty());
        assert_eq!(state.wants_terminal_in, vec![2]);
        state.focused = 0;
        let mut control_seq = 0;
        let out = sync_subs(&mut state, &mut control_seq)
            .unwrap()
            .expect("an unsubscribe");
        assert!(
            String::from_utf8_lossy(&out).contains("hide-tiles"),
            "the old space's terminals were not unsubscribed"
        );
        assert!(state.subs.is_empty());
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
            &mut 0,
            &mut dirty,
        );
        assert!(state.layouts.get(&1).unwrap().contains(3));
        assert!(!state.layouts.get(&2).unwrap().contains(3));
        assert!(state.is_pane(3), "bound splits hide from the tab bar");
        state.rows.push(agent(3, 1, "zsh", "idle"));
        assert!(!state.tabs().iter().any(|r| r.terminal == 3));
        assert!(state.rows.iter().any(|r| r.terminal == 3));
        // The split belongs to its tree's first leaf, and with the keyboard in it that
        // tab is the one the bar marks.
        let tab = state.layouts.get(&1).unwrap().leaves()[0];
        assert_eq!(state.panes().get(&3), Some(&tab));
        state.focused = 3;
        assert_eq!(state.focused_tab(), tab);
        state.focused = tab;
        assert_eq!(state.focused_tab(), tab);
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
            &mut 0,
            &mut dirty,
        );
        assert!(!state.layouts.get(&1).unwrap().contains(3));
        assert!(!state.is_pane(3));
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
        state.split_origin.insert("split-9".to_string(), (1, 1));
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
            &mut 0,
            &mut dirty,
        );
        assert!(state.layouts.get(&1).unwrap().contains(3));
        assert!(!state.layouts.get(&2).unwrap().contains(3));
        assert!(state.is_pane(3));
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
        state.split_origin.insert("split-9".to_string(), (1, 1));
        state.space = Some(2);
        state.layouts.insert(2, Layout::single(9));
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::Failed {
                id: "split-9".to_string(),
                reason: "no".to_string(),
            },
            &mut 0,
            &mut dirty,
        );
        assert!(!state.layouts.get(&1).unwrap().has_pending());
        assert!(state.layouts.get(&2).unwrap().contains(9));
        assert!(!state.split_origin.contains_key("split-9"));
    }

    /// Only the `+` corner of the spaces title makes a space: the word
    /// itself is a no-op, so a click aimed at the title never makes a space
    /// nobody asked for. The title sits under the inbox.
    #[test]
    fn only_the_plus_corner_of_the_spaces_title_makes_a_space() {
        let mut state = bare_state();
        state.cols = 100;
        state.lines = 30;
        state.spaces = vec![qoro_runtime::control::WorkspaceRow {
            keep_awake: false,
            closable: true,
            id: 1,
            name: "default".to_string(),
            terminals: 0,
            asleep: false,
            sleeping: Vec::new(),
            ink: None,
            link: None,
        }];
        state.space = Some(1);
        assert!(matches!(
            state.clicked(
                (crate::screen::SIDEBAR_TOP + crate::sidebar::TITLE_ROW) as u16,
                crate::sidebar::WIDTH as u16
            ),
            Some(Clicked::NewSpace)
        ));
        assert!(
            state
                .clicked(
                    (crate::screen::SIDEBAR_TOP + crate::sidebar::TITLE_ROW) as u16,
                    5
                )
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
        assert_eq!(state.menu, Some(12));
        // Keyless rows answer to their first letter, the first such row from the top:
        // `s` is still settings, ahead of sleep space.
        assert!(matches!(menu_key(&mut state, 7, b"s"), MenuOutcome::Moved));
        assert_eq!(state.menu, Some(13));
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
                "space settings",
                "close tab",
                "split side by side",
                "split stacked",
                "zoom pane",
                "copy mode",
                "resize panes",
                "equalize panes",
                "toggle view",
                "keybinds",
                "settings",
                "sleep space",
                "wake space",
                "reload manifests",
                "connect a machine",
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
            .position(|(n, _, _)| *n == "toggle view")
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
        let main = MenuKind::Main;
        let Some((left, top, w, _)) =
            crate::screen::menu_rect(cols, lines, &menu_rows(&main), &menu_title(&main))
        else {
            panic!("no menu rect on 80x24");
        };
        // The third row's middle cell.
        let row = (top + crate::screen::BOX_FIRST_ROW + 2 + 1) as u16;
        let col = (left + w / 2 + 1) as u16;
        let press = format!("\x1b[<0;{col};{row}M").into_bytes();
        assert_eq!(menu_click(&press, cols, lines, &main), Some(Some(2)));
        // Far away from the centred box is outside.
        assert_eq!(menu_click(b"\x1b[<0;1;1M", cols, lines, &main), Some(None));
        // A release is not a choice.
        let release = format!("\x1b[<0;{col};{row}m").into_bytes();
        assert_eq!(menu_click(&release, cols, lines, &main), None);
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

    fn index(state: &mut State, agents: Vec<AgentRow>) {
        let mut dirty = false;
        apply(
            state,
            ControlResponse::Agents {
                id: "index".to_string(),
                agents,
                splits: Vec::new(),
            },
            &mut 0,
            &mut dirty,
        );
    }

    /// Every held note, as if its second were up.
    fn age_notes(state: &mut State) {
        let past = std::time::Instant::now() - crate::notify::DELAY;
        for note in &mut state.notes {
            note.due = past;
        }
        state.notes_due = Some(past);
    }

    /// A note that arrived after another waits its own full second: it is not said with
    /// the first one early, and the timer stays armed for it.
    #[test]
    fn a_later_note_waits_its_own_second() {
        let mut state = bare_state();
        state.focused = 1;
        state.route = crate::notify::Route::Escape {
            escape: crate::notify::Escape::Osc9,
            tmux: false,
        };
        let working = |t| agent(t, 1, "claude", "working");
        index(&mut state, vec![working(1), working(2), working(3)]);
        index(
            &mut state,
            vec![working(1), agent(2, 1, "codex", "blocked"), working(3)],
        );
        age_notes(&mut state);
        index(
            &mut state,
            vec![
                working(1),
                agent(2, 1, "codex", "blocked"),
                agent(3, 1, "aider", "failed"),
            ],
        );
        let mut out = Vec::new();
        fire_notes(&mut state, &mut out).expect("fire");
        assert_eq!(out, b"\x1b]9;codex is blocked\x1b\\".to_vec());
        assert_eq!(state.notes.len(), 1, "the later one is still held");
        assert!(
            state.notes_due.is_some(),
            "and the timer stays armed for it"
        );
    }

    /// Changes while the person is not looking are held, said together once due, and
    /// the bell for them stays quiet when the desktop will say it instead.
    #[test]
    fn changes_away_from_the_keyboard_are_held_and_said_as_one() {
        let mut state = bare_state();
        state.focused = 1;
        state.route = crate::notify::Route::Escape {
            escape: crate::notify::Escape::Osc9,
            tmux: false,
        };
        let working = |t| agent(t, 1, "claude", "working");
        index(&mut state, vec![working(1), working(2), working(3)]);
        index(
            &mut state,
            vec![
                working(1),
                agent(2, 1, "codex", "blocked"),
                agent(3, 1, "aider", "idle"),
            ],
        );
        assert_eq!(state.notes.len(), 2, "{:?}", state.notes);
        assert!(
            state.notes_due.is_some(),
            "a timer is armed while something is held"
        );
        assert!(!state.ring, "the desktop says it, so the bell does not");
        let mut out = Vec::new();
        fire_notes(&mut state, &mut out).expect("fire");
        assert!(out.is_empty(), "said before its second was up: {out:?}");
        assert_eq!(state.notes.len(), 2, "still held");
        age_notes(&mut state);
        fire_notes(&mut state, &mut out).expect("fire");
        assert_eq!(
            out,
            b"\x1b]9;codex is blocked, aider finished\x1b\\".to_vec()
        );
        assert!(
            state.notes.is_empty() && state.notes_due.is_none(),
            "nothing left armed"
        );
    }

    /// A change undone inside the second says nothing, and the one under the keyboard
    /// says nothing while the window has the focus.
    #[test]
    fn a_blip_and_a_change_being_watched_say_nothing() {
        let mut state = bare_state();
        state.focused = 1;
        state.route = crate::notify::Route::Escape {
            escape: crate::notify::Escape::Osc9,
            tmux: false,
        };
        index(
            &mut state,
            vec![agent(1, 1, "a", "working"), agent(2, 1, "b", "working")],
        );
        index(
            &mut state,
            vec![agent(1, 1, "a", "idle"), agent(2, 1, "b", "blocked")],
        );
        assert_eq!(state.notes.len(), 1, "the focused one is being watched");
        index(
            &mut state,
            vec![agent(1, 1, "a", "idle"), agent(2, 1, "b", "working")],
        );
        let mut out = Vec::new();
        age_notes(&mut state);
        fire_notes(&mut state, &mut out).expect("fire");
        assert!(out.is_empty(), "a blip was said: {out:?}");
        // The window lost the focus: the focused one finishing is news after all.
        state.outer_focus = Some(false);
        index(
            &mut state,
            vec![agent(1, 1, "a", "working"), agent(2, 1, "b", "working")],
        );
        index(
            &mut state,
            vec![agent(1, 1, "a", "idle"), agent(2, 1, "b", "working")],
        );
        assert_eq!(state.notes.len(), 1);
    }

    /// A session that opens and settles (a new tab, a restore, `/clear`) did no turn: no
    /// note, no bell, no `done` row. A real turn settling still is all three.
    #[test]
    fn an_agent_that_only_started_is_not_news() {
        let mut state = bare_state();
        state.focused = 1;
        state.route = crate::notify::Route::Escape {
            escape: crate::notify::Escape::Osc9,
            tmux: false,
        };
        index(
            &mut state,
            vec![agent(1, 1, "a", "idle"), agent(2, 1, "b", "idle")],
        );
        index(
            &mut state,
            vec![agent(1, 1, "a", "idle"), agent(2, 1, "b", "starting")],
        );
        index(
            &mut state,
            vec![agent(1, 1, "a", "idle"), agent(2, 1, "b", "idle")],
        );
        assert!(
            state.notes.is_empty(),
            "a start was said: {:?}",
            state.notes.len()
        );
        assert!(!state.ring);
        assert!(state.unseen_finished.is_empty(), "a start reads as done");
        index(
            &mut state,
            vec![agent(1, 1, "a", "idle"), agent(2, 1, "b", "working")],
        );
        index(
            &mut state,
            vec![agent(1, 1, "a", "idle"), agent(2, 1, "b", "idle")],
        );
        assert_eq!(state.notes.len(), 1, "a finished turn is news");
        assert!(!state.unseen_finished.is_empty());
    }

    /// With nowhere to send a note the bell rings exactly as before, and the hint says why
    /// once.
    #[test]
    fn with_nowhere_to_send_it_the_bell_rings_and_the_hint_says_why_once() {
        let mut state = bare_state();
        state.focused = 1;
        state.nowhere = true;
        index(
            &mut state,
            vec![agent(1, 1, "a", "working"), agent(2, 1, "b", "working")],
        );
        index(
            &mut state,
            vec![agent(1, 1, "a", "working"), agent(2, 1, "b", "blocked")],
        );
        assert!(state.ring);
        assert!(
            state
                .hint
                .as_deref()
                .is_some_and(|h| h.contains("[notify] then desktop = false"))
        );
        assert!(!state.nowhere, "said once");
        state.config.desktop = false;
        state.ring = false;
        state.notes.clear();
        index(
            &mut state,
            vec![agent(1, 1, "a", "working"), agent(2, 1, "b", "working")],
        );
        index(
            &mut state,
            vec![agent(1, 1, "a", "working"), agent(2, 1, "b", "failed")],
        );
        assert!(
            state.ring && state.notes.is_empty(),
            "notifications off: only the bell"
        );
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
                splits: Vec::new(),
                agents: vec![agent(7, 5, "zsh", "working")],
            },
            &mut 0,
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
                splits: Vec::new(),
                agents: rows,
            },
            &mut 0,
            &mut dirty,
        );
        assert_eq!(
            state.pending_focus,
            Some(1),
            "the keyboard comes back to its space: {:?}",
            state.pending_focus
        );
    }

    /// Coming back lands on the terminal the person left, in its space, once the first
    /// index names it; one that is gone falls back to the first tab, and the file then
    /// keeps whatever the keyboard moves to, never a remote number.
    #[test]
    fn a_new_interface_comes_back_to_the_terminal_left() {
        let dir = tempfile::tempdir().expect("a directory");
        let file = dir.path().join("interface-focus");
        let index = |agents| ControlResponse::Agents {
            id: "index".to_string(),
            splits: Vec::new(),
            agents,
        };
        let rows = || {
            vec![
                agent(1, 1, "zsh", "idle"),
                agent(4, 2, "claude", "idle"),
                agent(9, 2, "zsh", "idle"),
            ]
        };
        std::fs::write(&file, "9\n").expect("write");
        let mut state = bare_state();
        state.focused = 0;
        state.adopted = false;
        state.last = LastFocus::read(Some(file.clone()));
        let mut dirty = false;
        let mut request = 0u64;
        apply(&mut state, index(rows()), &mut request, &mut dirty);
        assert_eq!(state.space, Some(2), "the space it was in");
        assert_eq!(state.pending_focus, Some(9), "the terminal it was on");
        assert_eq!(state.last.resume, None, "once");

        std::fs::write(&file, "77\n").expect("write");
        let mut state = bare_state();
        state.focused = 0;
        state.adopted = false;
        state.last = LastFocus::read(Some(file.clone()));
        apply(&mut state, index(rows()), &mut request, &mut dirty);
        assert_eq!(state.focused, 1, "gone: the first tab, as before");
        assert_eq!(state.pending_focus, None);

        state.last.keep(4);
        assert_eq!(std::fs::read_to_string(&file).expect("read"), "4\n");
        state.last.keep(crate::mux::REMOTE_TERMINAL | 3);
        assert_eq!(std::fs::read_to_string(&file).expect("read"), "4\n");
    }

    /// `qoro focus` for a terminal not listed yet: waited for, shown once an index names
    /// it, and not jumped to when it arrives long after the ask.
    #[test]
    fn a_focus_ahead_of_the_index_lands_when_the_index_catches_up() {
        let index = |agents| ControlResponse::Agents {
            id: "index".to_string(),
            splits: Vec::new(),
            agents,
        };
        let mut state = bare_state();
        state.space = Some(1);
        state.adopted = true;
        state.focused = 1;
        state.rows = vec![agent(1, 1, "zsh", "working")];
        state.show_when_listed = Some((9, std::time::Instant::now() + SHOW_WAIT, false));
        let mut dirty = false;
        // An index from before the new terminal: still waiting.
        apply(
            &mut state,
            index(vec![agent(1, 1, "zsh", "working")]),
            &mut 0,
            &mut dirty,
        );
        assert_eq!(state.pending_focus, None);
        assert!(state.show_when_listed.is_some());
        apply(
            &mut state,
            index(vec![
                agent(1, 1, "zsh", "working"),
                agent(9, 1, "claude", "idle"),
            ]),
            &mut 0,
            &mut dirty,
        );
        assert_eq!(state.pending_focus, Some(9));
        assert_eq!(state.show_when_listed, None);

        // Asked too long ago: forgotten, not shown.
        let mut state = bare_state();
        state.space = Some(1);
        state.adopted = true;
        state.focused = 1;
        state.show_when_listed = Some((
            9,
            std::time::Instant::now() - std::time::Duration::from_secs(6),
            false,
        ));
        apply(
            &mut state,
            index(vec![
                agent(1, 1, "zsh", "working"),
                agent(9, 1, "claude", "idle"),
            ]),
            &mut 0,
            &mut dirty,
        );
        assert_eq!(state.pending_focus, None);
        assert_eq!(state.show_when_listed, None);
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
                    splits: Vec::new(),
                    agents: vec![agent(1, 1, "zsh", state_word)],
                },
                &mut 0,
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
            &mut 0,
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
                splits: Vec::new(),
                agents: vec![agent(1, 1, "claude", "working")],
            },
            &mut 0,
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
        state.lines = (3 + screen::TABS + screen::STATUS) as u16;
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
    fn a_mouse_report_reaches_the_mouse_handling_whole_with_the_list_open() {
        let mut state = bare_state();
        state.focus = true;
        match focus_key(&mut state, b"\x1b[<64;12;5M") {
            FocusOutcome::Passthrough(rest) => assert_eq!(rest, b"\x1b[<64;12;5M"),
            other => panic!("the report is handed on intact: {other:?}"),
        }
        assert!(state.focus, "a wheel over the list is not a close");
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
            &mut 0,
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
            &mut 0,
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
            &mut 0,
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
            &mut 0,
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
                splits: Vec::new(),
                agents: vec![],
            },
            &mut 0,
            &mut dirty,
        );
        assert!(!state.done, "an offline remote ended the interface");
        state.spaces = vec![space(1, "space 1")];
        apply(
            &mut state,
            ControlResponse::Agents {
                id: "index".into(),
                splits: Vec::new(),
                agents: vec![],
            },
            &mut 0,
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
                splits: Vec::new(),
                agents: vec![
                    agent(1, 1, "zsh", "idle"),
                    agent(remote, R1 | 1, "claude", "idle"),
                ],
            },
            &mut 0,
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
                splits: Vec::new(),
                agents: vec![agent(remote, R1 | 1, "claude", "sleeping")],
            },
            &mut 0,
            &mut dirty,
        );
        assert_eq!(state.pending_focus, None);
    }

    /// A remote daemon moving onto a newer qoro ends its tabs and brings them back with the
    /// same numbers. The keyboard goes back to the tab it was on, not to the first tab of
    /// the space, and an interface showing only that remote (`qoro --remote`) stays open
    /// across the gap rather than ending with nothing listed.
    #[test]
    fn the_focused_tab_takes_the_keyboard_back_after_its_remote_restarts() {
        let first = crate::mux::REMOTE_TERMINAL | 1 << 24 | 2;
        let second = crate::mux::REMOTE_TERMINAL | 1 << 24 | 3;
        let index = |agents| ControlResponse::Agents {
            id: "mux".into(),
            splits: Vec::new(),
            agents,
        };
        let both = || {
            vec![
                agent(first, R1 | 1, "zsh", "idle"),
                agent(second, R1 | 1, "claude", "idle"),
            ]
        };
        let mut state = state();
        state.adopted = true;
        state.space = Some(R1 | 1);
        state.spaces = vec![space(R1 | 1, "devbox:work")];
        state.rows = both();
        state.focused = second;
        let mut dirty = false;

        // The link drops while the daemon there execs: its space is listed offline and its
        // tabs are gone from the index.
        state.spaces = vec![space(R1 | 1, "devbox:work (offline)")];
        apply(&mut state, index(Vec::new()), &mut 0, &mut dirty);
        assert_eq!(state.focused, 0);
        assert_eq!(state.show_when_listed.map(|(t, _, _)| t), Some(second));
        assert!(
            !state.done,
            "the interface ended while its remote restarted"
        );

        // Back, same numbers: the second tab, though the first is the space's first.
        state.spaces = vec![space(R1 | 1, "devbox:work")];
        apply(&mut state, index(both()), &mut 0, &mut dirty);
        assert_eq!(state.pending_focus, Some(second));
        assert_eq!(state.show_when_listed, None);

        // One gone for good is not waited for past its time.
        state.focused = second;
        state.pending_focus = None;
        apply(
            &mut state,
            index(vec![agent(first, R1 | 1, "zsh", "idle")]),
            &mut 0,
            &mut dirty,
        );
        let asked = std::time::Instant::now();
        let (_, until, returning) = state.show_when_listed.expect("waited for");
        assert!(returning);
        assert!(
            until > asked + FOCUS_RETURN - std::time::Duration::from_secs(1)
                && until <= asked + FOCUS_RETURN,
            "waited for {:?}, not about {FOCUS_RETURN:?}",
            until.saturating_duration_since(asked)
        );
        // The same wait, its time up: forgotten without a trace, not jumped to.
        state.show_when_listed = Some((second, until - FOCUS_RETURN, returning));
        state.pending_focus = None;
        apply(&mut state, index(both()), &mut 0, &mut dirty);
        assert_eq!(
            state.pending_focus,
            Some(first),
            "a stale wait took the keyboard"
        );
        assert_eq!(state.show_when_listed, None);

        // A tab of the person's own that went first (they closed it) does not hold the
        // place: the one the restart takes is waited for instead.
        let mut state = self::state();
        state.adopted = true;
        state.space = Some(R1 | 1);
        state.rows = both();
        state.focused = second;
        state.show_when_listed = Some((77, std::time::Instant::now() + FOCUS_RETURN, true));
        apply(&mut state, index(Vec::new()), &mut 0, &mut dirty);
        assert_eq!(state.show_when_listed.map(|(t, _, _)| t), Some(second));
    }

    /// A person who moved the keyboard themselves while a remote restarted keeps it where
    /// they put it: the tab coming back does not take it, whether they picked a tab or a
    /// space.
    #[test]
    fn a_tab_coming_back_does_not_take_the_keyboard_from_where_the_person_went() {
        let remote = crate::mux::REMOTE_TERMINAL | 1 << 24 | 3;
        let index = |agents| ControlResponse::Agents {
            id: "mux".into(),
            splits: Vec::new(),
            agents,
        };
        let both = || {
            vec![
                agent(1, 1, "zsh", "idle"),
                agent(remote, R1 | 1, "claude", "idle"),
            ]
        };
        for by_space in [false, true] {
            let mut state = state();
            state.adopted = true;
            state.space = Some(R1 | 1);
            state.spaces = vec![space(1, "space 1"), space(R1 | 1, "devbox:work")];
            state.rows = both();
            state.focused = remote;
            let (mut dirty, mut control_seq, mut seqs) = (false, 0, Default::default());
            apply(
                &mut state,
                index(vec![agent(1, 1, "zsh", "idle")]),
                &mut 0,
                &mut dirty,
            );
            assert_eq!(state.show_when_listed.map(|(t, _, _)| t), Some(remote));

            if by_space {
                enter_space(&mut state, 1, &mut control_seq, &mut seqs).unwrap();
            } else {
                focus(&mut state, 1, &mut control_seq, &mut seqs).unwrap();
            }
            assert_eq!(state.focused, 1);
            assert_eq!(state.show_when_listed, None);

            state.pending_focus = None;
            apply(&mut state, index(both()), &mut 0, &mut dirty);
            assert_eq!(
                state.pending_focus, None,
                "the tab coming back took the keyboard"
            );
            assert_eq!(state.focused, 1);
        }

        // A machine the person connected meanwhile: its first tab, not the one coming back,
        // though both arrive in the same index.
        let mut state = state();
        state.adopted = true;
        state.space = Some(R1 | 1);
        state.rows = both();
        state.focused = remote;
        let mut dirty = false;
        apply(
            &mut state,
            index(vec![agent(1, 1, "zsh", "idle")]),
            &mut 0,
            &mut dirty,
        );
        state.joining = Some("box".into());
        state.spaces = vec![
            space(1, "space 1"),
            space(R1 | 1, "devbox:work"),
            space(2 << 40 | 1, "box:work"),
        ];
        state.pending_focus = None;
        let mut joined = both();
        joined.push(agent(7, 2 << 40 | 1, "zsh", "idle"));
        apply(&mut state, index(joined), &mut 0, &mut dirty);
        assert_eq!(
            state.pending_focus,
            Some(7),
            "the tab coming back took the keyboard"
        );
        assert_eq!(state.show_when_listed, None);

        // A deferred focus the interface picked on its own is not the person's choice: the
        // wait stays, and a terminal asked for by name is never dropped by a move.
        let mut state = self::state();
        state.rows = both();
        state.show_when_listed = Some((9, std::time::Instant::now() + SHOW_WAIT, false));
        focus(&mut state, 1, &mut 0, &mut Default::default()).unwrap();
        assert_eq!(state.show_when_listed.map(|(t, _, _)| t), Some(9));
    }

    /// The answer to the ask that a remote move to a newer qoro is one line on the bar.
    #[test]
    fn a_remote_waiting_to_move_says_why_once() {
        let mut state = state();
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::RestartArmed {
                id: crate::mux::UPDATE_NOTICE.into(),
                waiting_on: Some("devbox moves to the new qoro once nothing there is working; now waiting on tab 3".into()),
            },
            &mut 0,
            &mut dirty,
        );
        assert!(
            state.hint.as_deref().is_some_and(|m| m.contains("tab 3")),
            "{:?}",
            state.hint
        );
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
            &mut 0,
            &mut dirty,
        );
        assert_eq!(state.creating, 0);
    }

    /// The `+` offers a space here or another machine, and each opens its own box.
    #[test]
    fn the_plus_offers_a_space_or_a_machine() {
        let mut state = bare_state();
        let mut request = 0;
        state.menu_kind = MenuKind::Plus;
        assert!(matches!(
            apply_menu_choice(&mut state, 0, &mut request),
            MenuSend::Nothing
        ));
        assert!(
            state
                .prompt
                .as_ref()
                .is_some_and(|p| p.what == Naming::Space)
        );
        state.prompt = None;
        state.menu_kind = MenuKind::Plus;
        apply_menu_choice(&mut state, 1, &mut request);
        assert!(
            state
                .prompt
                .as_ref()
                .is_some_and(|p| p.what == Naming::Remote)
        );
        // The choice spends the list: the next menu opened is the main one again.
        assert_eq!(state.menu_kind, MenuKind::Main);
    }

    /// An answer to a question no longer standing, or one that lands on an open box, never
    /// opens a list over what the person is doing.
    #[test]
    fn a_late_session_list_does_not_take_the_keys() {
        let mut state = bare_state();
        let mut dirty = false;
        let answer = |id: &str| ControlResponse::RemoteMissing {
            id: id.into(),
            target: "h".into(),
            platform: "linux x86_64".into(),
            root: false,
        };
        state.probing = Some("probe-2".into());
        apply(&mut state, answer("probe-1"), &mut 0, &mut dirty);
        assert_eq!(state.menu, None, "a stale answer opened the list");
        state.prompt = Some(Prompt::new(Naming::Space, ""));
        apply(&mut state, answer("probe-2"), &mut 0, &mut dirty);
        assert_eq!(state.menu, None, "the list opened over a box");
        assert!(state.message.is_some());
    }

    /// A machine connected from here moves the screen to its first tab when that arrives,
    /// and not before.
    #[test]
    fn a_connected_machine_is_joined_when_its_first_tab_arrives() {
        let mut state = bare_state();
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::RemoteAdded {
                id: "connect-1".into(),
                name: "devbox".into(),
            },
            &mut 0,
            &mut dirty,
        );
        let there = (1i64 << 40) | 1;
        let tab = crate::mux::REMOTE_TERMINAL | 1 << 24 | 1;
        state.spaces = vec![space(1, "space 1"), space(there, "devbox:default")];
        apply(
            &mut state,
            ControlResponse::Agents {
                id: "mux".into(),
                agents: vec![agent(1, 1, "zsh", "idle"), agent(tab, there, "zsh", "idle")],
                splits: Vec::new(),
            },
            &mut 0,
            &mut dirty,
        );
        assert_eq!(state.space, Some(there));
        assert_eq!(state.pending_focus, Some(tab));
        assert_eq!(state.joining, None);
    }

    #[test]
    fn resize_mode_moves_on_its_keys_and_hands_everything_else_back() {
        let mut state = tiled_state();
        enter_resize(&mut state);
        assert!(state.modes.resizing);
        let before = focused_rect(&state).cols;
        assert_eq!(resize_key(&mut state, b"lll"), ModeOutcome::Stay);
        assert!(focused_rect(&state).cols > before);
        let mut seqs = std::collections::BTreeMap::new();
        assert!(
            size_moved_tile(&mut state, &mut seqs).unwrap().is_some(),
            "the paint owes the tiles a resize"
        );
        assert_eq!(resize_key(&mut state, b"\x1b[D="), ModeOutcome::Stay);
        // Leaving keys leave and are not typed; what follows them is.
        assert_eq!(resize_key(&mut state, b"q"), ModeOutcome::Stay);
        assert!(!state.modes.resizing);
        state.modes.resizing = true;
        assert_eq!(
            resize_key(&mut state, b"\x1bls"),
            ModeOutcome::Passthrough(b"ls".to_vec())
        );
        // The prefix leaves and is passed on whole, so ctrl-a d still detaches.
        state.modes.resizing = true;
        assert_eq!(
            resize_key(&mut state, b"\x01d"),
            ModeOutcome::Passthrough(b"\x01d".to_vec())
        );
        assert!(!state.modes.resizing);
        // Typing is someone who has finished resizing.
        state.modes.resizing = true;
        assert_eq!(
            resize_key(&mut state, b"ls -la"),
            ModeOutcome::Passthrough(b"s -la".to_vec())
        );
        // One pane has nothing to resize, and says so.
        let mut single = bare_state();
        enter_resize(&mut single);
        assert!(!single.modes.resizing);
        assert!(single.message.is_some());
    }

    /// Both tiles either side of a moved divider are sized, once per painted frame however
    /// many drag motions arrived before it.
    #[test]
    fn a_divider_drag_sizes_both_tiles_once_per_frame() {
        let mut state = tiled_state();
        let rect = focused_rect(&state);
        let left = screen::left_for(state.cols, state.collapsed, state.sidebar_width);
        let divider_col = (left + 1 + rect.x + rect.cols) as u16;
        let row = (screen::TABS + 3) as u16;
        assert!(state.split_press(divider_col, row));
        assert!(!state.divider_press(divider_col, row), "not the sidebar's");
        assert!(state.split_motion(divider_col - 5, row));
        assert!(state.split_motion(divider_col - 9, row));
        assert_eq!(focused_rect(&state).cols, rect.cols - 9);
        let mut seqs = std::collections::BTreeMap::new();
        assert!(size_moved_tile(&mut state, &mut seqs).unwrap().is_some());
        assert!(size_moved_tile(&mut state, &mut seqs).unwrap().is_none());
        assert_eq!(sent(&seqs), vec![(1, 1), (2, 1)], "each tile once");
        let other = layout_rect(&state, 2);
        assert_eq!(state.sized[&1], (rect.cols as u16 - 9, rect.rows as u16));
        assert_eq!(state.sized[&2], (other.cols as u16, other.rows as u16));
        // A press inside a pane is not a divider.
        state.modes.split_drag = None;
        assert!(!state.split_press((left + 3) as u16, row));
    }

    /// How many resizes each terminal was sent, from the sequence numbers they took.
    fn sent(seqs: &std::collections::BTreeMap<u32, u32>) -> Vec<(u32, u32)> {
        seqs.iter().map(|(t, n)| (*t, *n)).collect()
    }

    fn layout_rect(state: &State, terminal: u32) -> layout::Rect {
        state.layouts[&1]
            .tile_rects(pane_area(state))
            .into_iter()
            .find(|(t, _)| *t == terminal)
            .map(|(_, r)| r)
            .expect("a tile")
    }

    /// Every tile is sized to its own rectangle, the unfocused one included, and asking
    /// again with nothing changed sends nothing, the focused one included: an equal size
    /// is the daemon's one-row-short nudge, and a shell reading its size then is wrong.
    #[test]
    fn every_tile_is_sized_and_an_unchanged_one_is_not_sent_again() {
        let mut state = tiled_state();
        let mut seqs = std::collections::BTreeMap::new();
        assert!(size_terminals(&mut state, &mut seqs).unwrap().is_some());
        assert_eq!(sent(&seqs), vec![(1, 1), (2, 1)]);
        let right = layout_rect(&state, 2);
        assert_eq!(state.sized[&2], (right.cols as u16, right.rows as u16));
        assert!(right.cols < pane_area(&state).cols, "a tile, not the pane");
        assert!(size_terminals(&mut state, &mut seqs).unwrap().is_none());
        assert_eq!(sent(&seqs), vec![(1, 1), (2, 1)], "nothing sent twice");
        // Moving the focus to a tile already at its size sends it nothing: an equal size
        // would be the daemon's one-row-short repaint nudge.
        let mut control_seq = 0;
        focus(&mut state, 2, &mut control_seq, &mut seqs).unwrap();
        assert_eq!(
            sent(&seqs),
            vec![(1, 1), (2, 1)],
            "no resize on a focus move"
        );
    }

    /// A tab not on screen, parked or in another space, is sized to the whole pane, the
    /// way it will be drawn when it comes back; a sleeping row has no process to size.
    #[test]
    fn a_tab_off_screen_is_sized_to_the_pane_and_a_sleeping_row_is_not() {
        let mut state = tiled_state();
        state.rows.push(agent(3, 1, "zsh", "idle"));
        state.rows.push(agent(4, 2, "codex", "working"));
        state.rows.push(agent(5, 2, "claude", "sleeping"));
        let mut seqs = std::collections::BTreeMap::new();
        size_terminals(&mut state, &mut seqs).unwrap();
        let area = pane_area(&state);
        let whole = (area.cols as u16, area.rows as u16);
        assert_eq!(state.sized.get(&3), Some(&whole));
        assert_eq!(state.sized.get(&4), Some(&whole));
        assert_eq!(state.sized.get(&5), None);
        assert!(!seqs.contains_key(&5));
        // Gone from the index: forgotten, so it is sized again if it comes back.
        state.rows.retain(|r| r.terminal != 3);
        size_terminals(&mut state, &mut seqs).unwrap();
        assert_eq!(state.sized.get(&3), None);
    }

    /// While the panel is dragged only what is on screen follows; the tabs off screen are
    /// sized once the drag lets go, not on every frame of it.
    #[test]
    fn a_drag_sizes_the_tabs_off_screen_only_once_it_lets_go() {
        let mut state = tiled_state();
        state.rows.push(agent(3, 1, "zsh", "idle"));
        let mut seqs = std::collections::BTreeMap::new();
        size_terminals(&mut state, &mut seqs).unwrap();
        let before = state.sized[&3];
        state.sidebar_drag = Some(state.sidebar_width);
        state.sidebar_width += 6;
        size_terminals(&mut state, &mut seqs).unwrap();
        assert_eq!(
            state.sized[&3], before,
            "an off-screen tab followed the drag"
        );
        assert_eq!(seqs[&3], 1);
        assert_ne!(seqs[&1], 1, "the tiles on screen follow it");
        state.sidebar_drag = None;
        size_terminals(&mut state, &mut seqs).unwrap();
        assert_eq!(seqs[&3], 2, "sized once the drag let go");
        assert_ne!(state.sized[&3], before);
    }

    /// Zoom sizes the zoomed pane to the whole area and leaves the pane it hides at the
    /// size it had; unzooming gives each its tile back.
    #[test]
    fn zoom_sizes_the_zoomed_pane_and_leaves_the_hidden_one_alone() {
        let mut state = tiled_state();
        let mut seqs = std::collections::BTreeMap::new();
        size_terminals(&mut state, &mut seqs).unwrap();
        let tile = state.sized[&2];
        assert!(state.layouts.get_mut(&1).unwrap().toggle_zoom());
        size_terminals(&mut state, &mut seqs).unwrap();
        let area = pane_area(&state);
        assert_eq!(state.sized[&1], (area.cols as u16, area.rows as u16));
        assert_eq!(state.sized[&2], tile, "kept while hidden");
        assert_eq!(seqs[&2], 1, "the hidden pane was not sent anything");
        assert!(state.layouts.get_mut(&1).unwrap().toggle_zoom());
        size_terminals(&mut state, &mut seqs).unwrap();
        assert_eq!(seqs[&2], 1, "back at the size it kept");
        assert_eq!(seqs[&1], 3, "the focused one sized back to its tile");
    }

    #[test]
    fn closing_a_busy_agent_asks_first_and_only_a_capital_y_closes() {
        let mut state = tiled_state();
        let mut request = 0;
        // Terminal 1 is an idle shell: closed at once.
        close_focused(&mut state, &mut request);
        assert!(matches!(
            state.modes.outbox.pop(),
            Some(ControlRequest::CloseTerminal { terminal: 1, .. })
        ));
        // Terminal 2 is a working claude: asked about.
        state.focused = 2;
        close_focused(&mut state, &mut request);
        assert!(state.modes.outbox.is_empty());
        assert_eq!(
            state.mode_line(),
            Some("close claude (working)? Y closes it, any other key keeps it")
        );
        // A lowercase y is the answer a blocked agent waits for, not this one's.
        assert_eq!(
            confirm_key(&mut state, b"y", &mut request),
            ModeOutcome::Stay
        );
        assert!(state.modes.outbox.is_empty(), "no is no");
        assert!(state.modes.confirm_close.is_none());
        assert_eq!(state.message.as_deref(), Some("kept claude"));
        close_focused(&mut state, &mut request);
        assert_eq!(
            confirm_key(&mut state, b"Y", &mut request),
            ModeOutcome::Stay
        );
        assert!(matches!(
            state.modes.outbox.pop(),
            Some(ControlRequest::CloseTerminal { terminal: 2, .. })
        ));
    }

    /// The drawer slides out a place a frame and back the same way; another rule's toggle
    /// opens that one instead, and without motion it is where it is going at once.
    #[test]
    fn a_rules_drawer_slides_out_and_back_a_place_a_frame() {
        let mut s = state();
        assert!(!s.drawer_moving());
        s.toggle_drawer(1);
        assert_eq!(s.drawer_view(), Some((1, 0)));
        for shown in 1..=crate::sidebar::DRAWER {
            assert!(s.drawer_moving());
            s.step_drawer();
            assert_eq!(s.drawer_view(), Some((1, shown)));
        }
        assert!(!s.drawer_moving(), "all out, the clock may stop");
        s.toggle_drawer(1);
        for shown in (1..crate::sidebar::DRAWER).rev() {
            s.step_drawer();
            assert_eq!(s.drawer_view(), Some((1, shown)));
        }
        s.step_drawer();
        assert_eq!(s.drawer_view(), None, "back in, gone");
        // Another rule's toggle: that one opens, from nothing.
        s.toggle_drawer(1);
        s.step_drawer();
        s.toggle_drawer(2);
        assert_eq!(s.drawer_view(), Some((2, 0)));
        // No motion: open at once, and closed at once.
        s.settle_drawer();
        assert_eq!(s.drawer_view(), Some((2, crate::sidebar::DRAWER)));
        s.toggle_drawer(2);
        s.settle_drawer();
        assert_eq!(s.drawer_view(), None);
    }

    /// A cup click sends the opposite of what the last list said, as a value, so a stale
    /// double click cannot toggle it back on the daemon.
    #[test]
    fn a_cup_click_asks_for_the_opposite_of_what_the_list_said() {
        let mut state = tiled_state();
        let mut request = 0;
        keep_awake(&mut state, 1, Some(2), &mut request);
        assert!(matches!(
            state.modes.outbox.pop(),
            Some(ControlRequest::SetKeepAwake {
                workspace: 1,
                terminal: Some(2),
                on: true,
                ..
            })
        ));
        if let Some(row) = state.rows.iter_mut().find(|r| r.terminal == 2) {
            row.keep_awake = true;
        }
        keep_awake(&mut state, 1, Some(2), &mut request);
        assert!(matches!(
            state.modes.outbox.pop(),
            Some(ControlRequest::SetKeepAwake {
                terminal: Some(2),
                on: false,
                ..
            })
        ));
        keep_awake(&mut state, 1, None, &mut request);
        assert!(matches!(
            state.modes.outbox.pop(),
            Some(ControlRequest::SetKeepAwake {
                workspace: 1,
                terminal: None,
                on: true,
                ..
            })
        ));
    }

    /// The `x` on an agent's row closes that agent's pane and nothing else: in a split the
    /// pane the keyboard is in stays, and a busy agent is asked about first.
    #[test]
    fn an_agent_row_close_ends_its_own_pane_of_a_split() {
        let mut state = tiled_state();
        let mut request = 0;
        assert_eq!(state.focused, 1);
        close_terminal(&mut state, 2, &mut request);
        assert!(
            state.modes.outbox.is_empty(),
            "a working claude is asked about"
        );
        assert_eq!(
            state.mode_line(),
            Some("close claude (working)? Y closes it, any other key keeps it")
        );
        assert_eq!(
            confirm_key(&mut state, b"Y", &mut request),
            ModeOutcome::Stay
        );
        assert!(matches!(
            state.modes.outbox.as_slice(),
            [ControlRequest::CloseTerminal { terminal: 2, .. }]
        ));
        assert_eq!(state.focused, 1, "the other pane keeps the keyboard");
    }

    /// Found in review: a sequence cut at the end of a read had its tail typed into the
    /// agent, `C` from a split arrow, `0;7M` from a drag report.
    #[test]
    fn a_sequence_split_across_reads_stays_in_the_mode() {
        let mut state = tiled_state();
        let mut request = 0;
        enter_resize(&mut state);
        let before = focused_rect(&state).cols;
        assert_eq!(
            mode_input(&mut state, b"\x1b[", 0x01, &mut request),
            ModeOutcome::Stay
        );
        assert_eq!(
            mode_input(&mut state, b"C", 0x01, &mut request),
            ModeOutcome::Stay
        );
        assert!(state.modes.resizing, "the arrow did not end the mode");
        assert!(
            focused_rect(&state).cols > before,
            "the arrow moved the divider"
        );
        assert_eq!(
            mode_input(&mut state, b"\x1b[<32;4", 0x01, &mut request),
            ModeOutcome::Stay
        );
        assert_eq!(
            mode_input(&mut state, b"0;7M", 0x01, &mut request),
            ModeOutcome::Stay
        );
        assert!(state.modes.resizing);
        // In copy mode too, and a lone esc is still the esc key.
        let mut state = copy_state();
        enter_copy(&mut state);
        assert_eq!(
            mode_input(&mut state, b"\x1bO", 0x01, &mut request),
            ModeOutcome::Stay
        );
        assert_eq!(
            mode_input(&mut state, b"A", 0x01, &mut request),
            ModeOutcome::Stay
        );
        assert_eq!(state.modes.copy.as_ref().unwrap().cursor.0, 1_002);
        assert_eq!(
            mode_input(&mut state, b"\x1b", 0x01, &mut request),
            ModeOutcome::Stay
        );
        assert!(state.modes.copy.is_none(), "esc left");
    }

    #[test]
    fn resize_while_zoomed_says_to_unzoom_and_a_press_under_the_pane_is_no_drag() {
        let mut state = tiled_state();
        let mut control_seq = 0;
        let mut seqs = std::collections::BTreeMap::new();
        zoom_flip(&mut state, &mut control_seq, &mut seqs).unwrap();
        enter_resize(&mut state);
        assert!(!state.modes.resizing);
        assert_eq!(
            state.message.as_deref(),
            Some("zoomed: ctrl-a z first, then ctrl-a e")
        );
        // Unzoomed, the status rows under the divider are not the divider.
        zoom_flip(&mut state, &mut control_seq, &mut seqs).unwrap();
        let rect = focused_rect(&state);
        let left = screen::left_for(state.cols, state.collapsed, state.sidebar_width);
        let divider_col = (left + 1 + rect.x + rect.cols) as u16;
        assert!(!state.split_press(divider_col, state.lines - 1));
        assert!(!state.split_press(divider_col, state.lines));
        assert!(state.split_press(divider_col, (screen::TABS + 2) as u16));
    }

    #[test]
    fn copy_mode_y_with_nothing_selected_leaves_and_home_end_and_ctrl_u_work() {
        let mut state = copy_state();
        let mut request = 0;
        enter_copy(&mut state);
        copy_key(&mut state, b"k\x1b[F", 0x01, &mut request);
        assert_eq!(
            state.modes.copy.as_ref().unwrap().cursor,
            (1_002, 4),
            "end of `third`"
        );
        copy_key(&mut state, b"\x1b[H", 0x01, &mut request);
        assert_eq!(state.modes.copy.as_ref().unwrap().cursor, (1_002, 0));
        copy_key(&mut state, b"/abc\x15", 0x01, &mut request);
        assert_eq!(
            state.modes.copy.as_ref().unwrap().query,
            Some((String::new(), false)),
            "ctrl-u cleared the query"
        );
        copy_key(&mut state, b"\x1by", 0x01, &mut request);
        assert!(state.modes.copy.is_none(), "y with nothing selected leaves");
        assert!(state.modes.clipboard.is_none(), "and copies nothing");
    }

    /// A terminal with 1,000 rows behind a four row screen, one pane.
    fn copy_state() -> State {
        let mut state = bare_state();
        state.snapshot = Some(SnapshotWire {
            cols: 40,
            rows: 4,
            cursor: (3, 2),
            lines: vec![
                "first screen row".to_string(),
                "MARK on the screen".to_string(),
                "third".to_string(),
                "$ ".to_string(),
            ],
            styles: Vec::new(),
            modes: Default::default(),
            behind: 1_000,
            links: Default::default(),
        });
        state.lines = 4 + (screen::TABS + screen::STATUS) as u16;
        state
    }

    #[test]
    fn copy_mode_opens_on_the_cursor_and_moves_through_history_asking_for_it() {
        let mut state = copy_state();
        let mut request = 0;
        enter_copy(&mut state);
        let copy = state.modes.copy.as_ref().expect("open");
        assert_eq!((copy.cursor, copy.top), ((1_003, 2), 1_000));
        assert_eq!(state.anchor, Some(1_000), "the view is pinned");
        // Up past the screen: the view follows and the rows behind it are asked for.
        assert_eq!(
            copy_key(&mut state, b"kkkk", 0x01, &mut request),
            ModeOutcome::Stay
        );
        assert_eq!(state.modes.copy.as_ref().unwrap().top, 999);
        assert!(matches!(
            state.modes.outbox.last(),
            Some(ControlRequest::Scrollback { id, .. }) if id == "behind"
        ));
        assert_eq!(
            copy_key(&mut state, b"g", 0x01, &mut request),
            ModeOutcome::Stay
        );
        assert_eq!(state.modes.copy.as_ref().unwrap().cursor.0, 0);
        assert_eq!(
            copy_key(&mut state, b"G", 0x01, &mut request),
            ModeOutcome::Stay
        );
        assert_eq!(state.modes.copy.as_ref().unwrap().cursor.0, 1_003);
        // The wheel moves the view, and a click leaves and is handed back as a click.
        copy_key(&mut state, b"\x1b[<64;10;3M", 0x01, &mut request);
        assert_eq!(state.modes.copy.as_ref().unwrap().top, 997);
        assert_eq!(
            copy_key(&mut state, b"\x1b[<0;10;3M", 0x01, &mut request),
            ModeOutcome::Passthrough(b"\x1b[<0;10;3M".to_vec())
        );
        assert!(state.modes.copy.is_none());
        assert_eq!(state.anchor, None, "the view goes back where it was");
    }

    /// Found in review: rows a pinned view showed from the live screen go behind it as an
    /// agent prints, and nothing asked for them, so they painted blank.
    #[test]
    fn a_pinned_view_asks_for_the_rows_that_went_behind_the_screen_once_per_answer() {
        let mut state = copy_state();
        enter_copy(&mut state);
        state.modes.outbox.clear();
        refetch_pinned(&mut state);
        assert!(state.modes.outbox.is_empty(), "nothing went behind yet");
        // The agent printed three rows: the top three of the view are history now.
        state.snapshot.as_mut().unwrap().behind = 1_003;
        refetch_pinned(&mut state);
        refetch_pinned(&mut state);
        let asked: Vec<_> = std::mem::take(&mut state.modes.outbox);
        assert_eq!(
            asked.len(),
            1,
            "one request per answer, not per frame: {asked:?}"
        );
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::Scrollback {
                id: "behind".to_string(),
                terminal: 1,
                from: 996,
                total: 1_003,
                lines: (996..1_003).map(|at| format!("row {at}")).collect(),
            },
            &mut 0,
            &mut dirty,
        );
        assert_eq!(copy_row(&state, 1_000).as_deref(), Some("row 1000"));
        refetch_pinned(&mut state);
        assert!(state.modes.outbox.is_empty(), "everything shown is held");
    }

    #[test]
    fn the_prefix_leaves_copy_mode_and_esc_puts_a_selection_down_first() {
        let mut state = copy_state();
        let mut request = 0;
        enter_copy(&mut state);
        copy_key(&mut state, b"vh", 0x01, &mut request);
        assert!(state.copy_selection().is_some());
        copy_key(&mut state, b"\x1b", 0x01, &mut request);
        assert!(
            state.modes.copy.is_some(),
            "the first esc only drops the selection"
        );
        assert!(state.copy_selection().is_none());
        // A prefix of ctrl-b wins over page up.
        assert_eq!(
            copy_key(&mut state, b"\x02d", 0x02, &mut request),
            ModeOutcome::Passthrough(b"\x02d".to_vec())
        );
        assert!(state.modes.copy.is_none());
    }

    #[test]
    fn a_search_tries_the_screen_first_then_asks_the_history_a_step_at_a_time() {
        let mut state = copy_state();
        let mut request = 0;
        enter_copy(&mut state);
        // On the screen: no request at all.
        copy_key(&mut state, b"?mark\r", 0x01, &mut request);
        assert_eq!(state.modes.copy.as_ref().unwrap().cursor, (1_001, 0));
        assert!(
            !state
                .modes
                .outbox
                .iter()
                .any(|a| matches!(a, ControlRequest::SearchScrollback { .. }))
        );
        // Not on the screen: the history, from the row above the screen.
        copy_key(&mut state, b"?needle\r", 0x01, &mut request);
        let Some(ControlRequest::SearchScrollback {
            id, from, backward, ..
        }) = state.modes.outbox.pop()
        else {
            panic!("no history search asked");
        };
        assert_eq!((from, backward), (999, true));
        // A step that ran out of budget says how far it got, and n carries on from there.
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::ScrollbackMatch {
                id: id.clone(),
                terminal: 1,
                row: None,
                col: 0,
                resume: Some(400),
            },
            &mut 0,
            &mut dirty,
        );
        assert_eq!(
            state.mode_line(),
            Some("no match in the last 599 rows, n searches further")
        );
        copy_key(&mut state, b"n", 0x01, &mut request);
        let Some(ControlRequest::SearchScrollback { id, from, .. }) = state.modes.outbox.pop()
        else {
            panic!("n asked nothing");
        };
        assert_eq!(from, 400);
        // The match moves the cursor to its row and cell, and the view with it.
        apply(
            &mut state,
            ControlResponse::ScrollbackMatch {
                id,
                terminal: 1,
                row: Some(123),
                col: 7,
                resume: None,
            },
            &mut 0,
            &mut dirty,
        );
        let copy = state.modes.copy.as_ref().unwrap();
        assert_eq!(copy.cursor, (123, 7));
        assert!(copy.top <= 123 && 123 < copy.top + 4);
        assert_eq!(state.anchor, Some(copy.top));
        // A stale answer changes nothing.
        apply(
            &mut state,
            ControlResponse::ScrollbackMatch {
                id: "search-old".to_string(),
                terminal: 1,
                row: Some(5),
                col: 0,
                resume: None,
            },
            &mut 0,
            &mut dirty,
        );
        assert_eq!(state.modes.copy.as_ref().unwrap().cursor, (123, 7));
    }

    #[test]
    fn an_old_daemon_refusing_the_search_is_the_answer_to_it() {
        let mut state = copy_state();
        let mut request = 0;
        enter_copy(&mut state);
        copy_key(&mut state, b"/zzz\r", 0x01, &mut request);
        copy_key(&mut state, b"k?zzz\r", 0x01, &mut request);
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::Failed {
                id: String::new(),
                reason: "this daemon is older (unknown variant `SearchScrollback`)".to_string(),
            },
            &mut 0,
            &mut dirty,
        );
        assert_eq!(
            state.mode_line(),
            Some("search needs a newer daemon: stop the session to upgrade")
        );
        assert_eq!(state.message, None, "said in the mode, not shouted");
    }

    /// A selection across more rows than one answer carries is fetched in chunks of
    /// `yank-` answers that never touch the view's window, and copied whole.
    #[test]
    fn a_yank_across_a_chunk_boundary_copies_every_row_and_leaves_the_window_alone() {
        let mut state = copy_state();
        let mut request = 0;
        enter_copy(&mut state);
        copy_key(&mut state, b"V", 0x01, &mut request);
        state.modes.copy.as_mut().unwrap().cursor = (200, 0);
        assert_eq!(
            copy_key(&mut state, b"y", 0x01, &mut request),
            ModeOutcome::Stay
        );
        assert!(state.modes.copy.is_none(), "y copies and leaves");
        let mut dirty = false;
        let mut answers = 0;
        while let Some(ask) = state.modes.outbox.pop() {
            let ControlRequest::Scrollback { id, up, rows, .. } = ask else {
                continue;
            };
            if !id.starts_with("yank-") {
                continue;
            }
            answers += 1;
            let from = 1_000 - up as u64;
            apply(
                &mut state,
                ControlResponse::Scrollback {
                    id,
                    terminal: 1,
                    from,
                    total: 1_000,
                    lines: (from..from + rows as u64)
                        .map(|at| format!("row {at}"))
                        .collect(),
                },
                &mut 0,
                &mut dirty,
            );
        }
        assert_eq!(answers, 2, "800 history rows are two answers");
        let text = state.modes.clipboard.take().expect("copied");
        assert_eq!(text.lines().count(), 804);
        assert!(text.starts_with("row 200\n"));
        assert!(text.ends_with("row 999\nfirst screen row\nMARK on the screen\nthird\n$"));
        assert!(
            state.behind.lines.is_empty(),
            "the window was never written"
        );
    }

    /// A machine with no qoro opens the offer to install it, under the same rules as the
    /// session list, with the platform on the row and root said in the title.
    #[test]
    fn a_machine_without_qoro_offers_the_install() {
        let mut state = bare_state();
        state.probing = Some("probe-1".into());
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::RemoteMissing {
                id: "probe-1".into(),
                target: "admin@10.0.0.5".into(),
                platform: "linux x86_64".into(),
                root: true,
            },
            &mut 0,
            &mut dirty,
        );
        assert_eq!(state.menu, Some(0));
        let rows = menu_rows(&state.menu_kind);
        assert!(rows[0].0.starts_with("install qoro "), "{rows:?}");
        assert_eq!(rows[0].1, "linux x86_64");
        assert_eq!(rows[1].0, "cancel");
        assert!(menu_title(&state.menu_kind).contains("as root"));

        let mut request = 0;
        match apply_menu_choice(&mut state, 0, &mut request) {
            MenuSend::Control(ControlRequest::InstallRemote { id, target }) => {
                assert_eq!(target, "admin@10.0.0.5");
                assert_eq!(state.installing.as_deref(), Some(id.as_str()));
            }
            _ => panic!("choosing install does not ask for it"),
        }
    }

    #[test]
    fn cancelling_the_install_asks_for_nothing() {
        let mut state = bare_state();
        state.menu_kind = MenuKind::Install {
            target: "h".into(),
            platform: "linux x86_64".into(),
            root: false,
        };
        let mut request = 0;
        assert!(matches!(
            apply_menu_choice(&mut state, 1, &mut request),
            MenuSend::Nothing
        ));
        assert_eq!(state.installing, None);
    }

    /// An install that finishes is always said; the session list follows only while the
    /// person is still waiting on that install.
    #[test]
    fn a_finished_install_asks_again_only_while_awaited() {
        let mut state = bare_state();
        let mut dirty = false;
        let done = |id: &str| ControlResponse::RemoteInstalled {
            id: id.into(),
            target: "h".into(),
            version: "0.2.0".into(),
        };
        state.installing = Some("install-2".into());
        apply(&mut state, done("install-1"), &mut 0, &mut dirty);
        assert!(
            state
                .message
                .as_deref()
                .is_some_and(|m| m.contains("installed"))
        );
        assert_eq!(state.reprobe, None, "a stale install asked again");
        apply(&mut state, done("install-2"), &mut 0, &mut dirty);
        assert_eq!(state.reprobe.as_deref(), Some("h"));
        assert_eq!(state.installing, None);
    }

    /// A space as a wake would bring it back: asleep, holding these tabs.
    fn asleep(id: i64, name: &str, tabs: &[u32]) -> WorkspaceRow {
        WorkspaceRow {
            asleep: true,
            terminals: 0,
            sleeping: tabs
                .iter()
                .map(|&terminal| qoro_runtime::control::SleepingTab {
                    lead: None,
                    terminal,
                    session: None,
                    title: None,
                    agent: Some("claude".into()),
                    cwd: "/tmp".into(),
                })
                .collect(),
            ..space(id, name)
        }
    }

    fn spaces(state: &mut State, workspaces: Vec<WorkspaceRow>) {
        let mut dirty = false;
        apply(
            state,
            ControlResponse::Workspaces {
                id: "spaces".into(),
                workspaces,
            },
            &mut 0,
            &mut dirty,
        );
    }

    /// Reaching for a sleeping tab from the keyboard (a palette jump) only puts it on screen:
    /// its space comes on screen with that tab picked and nothing is asked of the daemon.
    #[test]
    fn reaching_for_a_sleeping_tab_shows_it_and_wakes_nothing() {
        let mut state = state();
        state.spaces[0] = asleep(1, "app", &[7, 8]);
        state.rows.clear();
        let (mut control_seq, mut seqs) = (0, Default::default());
        peek(&mut state, 1, 8, &mut control_seq, &mut seqs).expect("peek");
        assert_eq!(state.space, Some(1));
        assert_eq!(state.focused, 0);
        assert_eq!(state.dim_tab(), Some(8));
        assert_eq!(state.waking, None);
        assert!(state.modes.outbox.is_empty(), "{:?}", state.modes.outbox);
    }

    /// The screen of a sleeping tab is asked for once when the keyboard rests on it, drawn
    /// dim in its pane when it arrives, and let go of when no sleeping tab is on screen.
    #[test]
    fn a_sleeping_tabs_last_screen_is_asked_for_once_and_drawn_dim() {
        let mut state = state();
        state.spaces[0] = asleep(1, "app", &[7, 8]);
        state.rows.clear();
        state.space = Some(1);
        state.focused = 0;
        state.dim = 8;
        ask_peek(&mut state);
        ask_peek(&mut state);
        assert!(
            matches!(
                state.modes.outbox.as_slice(),
                [ControlRequest::Scrollback { id, terminal: 8, .. }] if id == "peek-8"
            ),
            "asked once, for the tab the keyboard rests on: {:?}",
            state.modes.outbox
        );
        assert!(
            state.peek_view().is_none(),
            "nothing to draw before it arrives"
        );

        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::Scrollback {
                id: "peek-8".into(),
                terminal: 8,
                from: 0,
                total: 2,
                lines: vec!["$ cargo test".into(), "ok".into()],
            },
            &mut 0,
            &mut dirty,
        );
        let shown = state.peek_view().expect("the screen is drawn");
        assert_eq!(shown.lines, vec!["$ cargo test", "ok"]);
        assert!(
            shown
                .styles
                .iter()
                .all(|row| row[0].attrs == qoro_runtime::daemon::attrs::DIM)
        );
        assert!(!shown.modes.cursor_visible);
        assert!(state.frame().snapshot.is_some());

        // Another tab picked: its own screen is drawn, never the last one's.
        state.dim = 7;
        assert!(state.peek_view().is_none());

        // Leaving the sleeping space lets go of it, so a later sleep asks afresh.
        state.focused = 1;
        ask_peek(&mut state);
        assert!(state.modes.peek.is_none() && state.modes.peek_asked.is_none());
    }

    /// An answer for a tab no longer being looked at is dropped, and a resized pane asks
    /// again so the screen is cut to the new size.
    #[test]
    fn a_stale_peek_answer_is_dropped_and_a_resize_asks_again() {
        let mut state = state();
        state.spaces[0] = asleep(1, "app", &[7, 8]);
        state.rows.clear();
        state.space = Some(1);
        state.focused = 0;
        state.dim = 8;
        ask_peek(&mut state);
        state.modes.outbox.clear();
        let mut dirty = false;
        let answer = |terminal| ControlResponse::Scrollback {
            id: format!("peek-{terminal}"),
            terminal,
            from: 0,
            total: 1,
            lines: vec!["old".into()],
        };
        apply(&mut state, answer(7), &mut 0, &mut dirty);
        assert!(
            state.modes.peek.is_none(),
            "an answer for another tab was kept"
        );

        state.lines += 4;
        ask_peek(&mut state);
        assert_eq!(
            state.modes.outbox.len(),
            1,
            "a resized pane did not ask again"
        );
    }

    /// Only the tail that fits the pane is kept, and a tab with nothing kept says so.
    #[test]
    fn a_peek_keeps_the_tail_that_fits_and_names_an_empty_one() {
        let lines = (0..10).map(|n| n.to_string()).collect();
        assert_eq!(peek_screen(lines, 20, 3).lines, vec!["7", "8", "9"]);
        assert_eq!(
            peek_screen(Vec::new(), 20, 3).lines,
            vec!["nothing of this tab's screen was kept"]
        );
    }

    /// `ctrl-a Z` sleeps the tab the keyboard is in, alone, and on a sleeping tab the
    /// keyboard rests on wakes that tab and shows it once it is back. A refusal stays on
    /// the hint row with the daemon's reason; the rule's `sleep` only ever sleeps, and its
    /// `wake` wakes the whole space.
    #[test]
    fn ctrl_a_z_sleeps_the_tab_and_wakes_a_sleeping_one() {
        let mut state = state();
        let mut request = 0;
        sleep_toggle(&mut state, &mut request);
        assert_eq!(
            state.modes.outbox.pop(),
            Some(ControlRequest::SleepWorkspace {
                id: "sleep-tab-1".into(),
                workspace: 2,
                terminal: Some(2),
            })
        );

        state.spaces[0] = asleep(1, "app", &[7, 8]);
        state.space = Some(1);
        state.focused = 0;
        state.dim = 8;
        sleep_toggle(&mut state, &mut request);
        assert_eq!(
            state.modes.outbox.pop(),
            Some(ControlRequest::WakeWorkspace {
                id: "wake-tab-2".into(),
                workspace: Some(1),
                terminal: Some(8),
            })
        );
        assert_eq!(state.waking, Some((1, 8)));
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::WorkspaceAwake {
                id: "wake-tab-2".into(),
                workspace: 1,
                terminals: 1,
                resumed: 1,
                changed: Some(vec![8]),
            },
            &mut 0,
            &mut dirty,
        );
        assert_eq!(state.waking, None);
        assert_eq!(state.show_when_listed.map(|(t, _, _)| t), Some(8));

        apply(
            &mut state,
            ControlResponse::Failed {
                id: "sleep-3".into(),
                reason: "tab 2 (ssh) holds a connection".into(),
            },
            &mut 0,
            &mut dirty,
        );
        assert_eq!(
            state.hint.as_deref(),
            Some("tab 2 (ssh) holds a connection")
        );
        // The rule's `sleep` puts the whole space to sleep, and on a sleeping space asks
        // nothing to wake.
        sleep_space(&mut state, 1, &mut request);
        assert_eq!(
            state.modes.outbox.pop(),
            Some(ControlRequest::SleepWorkspace {
                id: "sleep-3".into(),
                workspace: 1,
                terminal: None,
            })
        );
        // Its `wake` asks for every tab of the space, and names none.
        wake_space(&mut state, 1, &mut request);
        assert_eq!(
            state.modes.outbox.pop(),
            Some(ControlRequest::WakeWorkspace {
                id: "wake-4".into(),
                workspace: Some(1),
                terminal: None,
            })
        );
        // An older daemon sleeps the space whatever was asked, and that is said.
        apply(
            &mut state,
            ControlResponse::WorkspaceAsleep {
                id: "sleep-tab-4".into(),
                workspace: 2,
                terminals: 3,
                changed: None,
                stayed: Vec::new(),
            },
            &mut 0,
            &mut dirty,
        );
        assert!(
            state
                .hint
                .as_deref()
                .is_some_and(|h| h.contains("daemon is older")),
            "{:?}",
            state.hint
        );
    }

    /// A refused space wake leaves a tab still waking alone; only the tab's own refusal
    /// ends its wait.
    #[test]
    fn a_refused_space_wake_keeps_a_tab_waking() {
        let mut state = state();
        let mut dirty = false;
        state.waking = Some((1, 8));
        let refuse = |state: &mut State, id: &str, dirty: &mut bool| {
            apply(
                state,
                ControlResponse::Failed {
                    id: id.into(),
                    reason: "no".into(),
                },
                &mut 0,
                dirty,
            );
        };
        refuse(&mut state, "wake-5", &mut dirty);
        assert_eq!(state.waking, Some((1, 8)));
        refuse(&mut state, "wake-tab-6", &mut dirty);
        assert_eq!(state.waking, None);
    }

    /// A space half asleep: its sleeping tabs sit on the bar after the awake ones, one per
    /// tab however many panes it had, dim; the keyboard can rest on one without waking it,
    /// and `ctrl-a n/p` from there go to the awake tabs.
    #[test]
    fn a_half_asleep_space_shows_its_sleeping_tabs_and_steps_off_a_peek() {
        let mut state = state();
        let mut half = asleep(2, "webshop", &[7, 8, 9]);
        half.asleep = false;
        half.terminals = 1;
        // 7 and 8 are one tab, split; 9 is a tab of its own.
        half.sleeping[0].lead = Some(7);
        half.sleeping[1].lead = Some(7);
        half.sleeping[2].lead = Some(9);
        state.spaces[1] = half;
        let bar = screen::bar_tabs(&state.rows, &state.panes(), state.space, &state.spaces);
        assert_eq!(
            bar.iter()
                .map(|r| (r.terminal, r.state.as_str()))
                .collect::<Vec<_>>(),
            vec![(2, "working"), (7, "sleeping"), (9, "sleeping")]
        );
        assert_eq!(state.sleeping_space_of(8), Some(2));
        // Resting on a sleeping tab: no terminal has the keyboard, and the dim one is it.
        state.focused = 0;
        state.dim = 9;
        assert_eq!(state.dim_tab(), Some(9));
        assert_eq!(state.neighbour(true), Some(2));
        assert_eq!(state.neighbour(false), Some(2));
        // The next index, with the awake tab in it, leaves the keyboard where it rests.
        index(&mut state, vec![agent(2, 2, "claude", "working")]);
        assert_eq!(state.focused, 0);
        assert_eq!(state.pending_focus, None, "a peek was taken back");
        // Closing the space counts its sleeping tabs as well as its awake one.
        assert_eq!(tabs_left(&state, 2), 4);
        assert_eq!(tabs_to_close(&state, 2), vec![2, 7, 8, 9]);
        assert!(!space_is_empty(&state, 2));
    }

    /// The keyboard on a sleeping tab: `ctrl-a x` discards that tab, not the space.
    #[test]
    fn closing_on_a_sleeping_space_discards_the_tab_the_keyboard_rests_on() {
        let mut state = state();
        state.spaces[0] = asleep(1, "app", &[7, 8]);
        state.space = Some(1);
        state.focused = 0;
        let mut request = 0;
        close_focused(&mut state, &mut request);
        assert_eq!(
            state.modes.outbox.pop(),
            Some(ControlRequest::CloseTerminal {
                id: "close-1".into(),
                terminal: 7,
            })
        );
    }

    /// `ctrl-a s/S` pass a sleeping space by, and with every other space asleep stay put.
    #[test]
    fn space_navigation_skips_a_sleeping_space() {
        let mut state = state();
        state.spaces = vec![space(1, "app"), asleep(2, "api", &[7]), space(3, "dub")];
        state.space = Some(1);
        assert_eq!(state.next_space(true), Some(3));
        state.space = Some(3);
        assert_eq!(state.next_space(false), Some(1));
        state.spaces[2] = asleep(3, "dub", &[9]);
        state.space = Some(1);
        assert_eq!(state.next_space(true), Some(1));
        assert_eq!(
            state.dim_tab(),
            None,
            "an awake space rests on no sleeping tab"
        );
    }

    /// The space on screen falls asleep: the screen moves to an awake space. Once every
    /// space is asleep, whoever slept them, nothing is left and the interface detaches.
    #[test]
    fn the_last_awake_space_falling_asleep_detaches() {
        let mut state = state();
        spaces(
            &mut state,
            vec![space(1, "app"), asleep(2, "webshop", &[2])],
        );
        assert_eq!(state.space, Some(1));
        assert_eq!(state.pending_focus, Some(1));
        assert!(state.shut.contains(&2), "a space falling asleep folds");
        assert!(!state.done);

        // `qoro sleep app` from a shell: the index loses its tabs first, the space list
        // follows. A sleeping remote space keeps nothing open either.
        state.pending_focus = None;
        index(&mut state, vec![]);
        assert!(
            state.done,
            "nothing awake anywhere, and the interface stayed"
        );
        state.done = false;
        spaces(
            &mut state,
            vec![
                asleep(1, "app", &[1]),
                asleep(2, "webshop", &[2]),
                asleep(R1 | 1, "devbox:work", &[]),
            ],
        );
        assert!(
            state.done,
            "a sleeping remote space kept the interface open"
        );
    }

    /// Sleep folds a space and the fold holds while it sleeps: the index losing its tabs
    /// and every later space list leave it shut. A person opening it again is kept, and a
    /// wake opens it.
    #[test]
    fn a_space_stays_folded_while_it_sleeps() {
        let mut state = state();
        spaces(&mut state, vec![space(1, "app"), asleep(2, "api", &[2])]);
        assert!(state.shut.contains(&2), "sleep folds the space");
        index(&mut state, vec![agent(1, 1, "claude", "idle")]);
        spaces(&mut state, vec![space(1, "app"), asleep(2, "api", &[2])]);
        assert!(state.shut.contains(&2), "a later space list opened it");
        state.shut.remove(&2);
        spaces(&mut state, vec![space(1, "app"), asleep(2, "api", &[2])]);
        assert!(!state.shut.contains(&2), "a person's unfold was undone");
        state.shut.insert(2);
        spaces(&mut state, vec![space(1, "app"), space(2, "api")]);
        assert!(!state.shut.contains(&2), "a wake leaves it folded");

        let mut fresh = self::state();
        fresh.spaces.clear();
        spaces(&mut fresh, vec![space(1, "app"), asleep(2, "api", &[2])]);
        assert!(
            fresh.shut.contains(&2),
            "a space first seen asleep was not folded"
        );
    }

    /// An index is the only word of a sleep or wake made elsewhere: an awake space whose
    /// tabs all left, or a sleeping one whose tabs came back, asks for the space list.
    #[test]
    fn tabs_leaving_or_returning_ask_for_the_space_list() {
        let rows = [agent(1, 1, "claude", "idle")];
        assert!(!slept_or_woken(&[space(1, "app")], &rows));
        assert!(slept_or_woken(&[space(1, "app"), space(2, "api")], &rows));
        assert!(slept_or_woken(&[asleep(1, "app", &[1])], &rows));
        assert!(!slept_or_woken(&[asleep(2, "api", &[5])], &rows));
    }

    /// Adopted from a first render, the space list lands before the first index: its
    /// space has a tab, so the interface that just opened stays.
    #[test]
    fn a_space_list_before_the_first_index_does_not_detach() {
        let mut state = state();
        state.rows.clear();
        spaces(&mut state, vec![space(1, "app")]);
        assert!(
            !state.done,
            "the interface left while its space still had a tab"
        );
    }
}
