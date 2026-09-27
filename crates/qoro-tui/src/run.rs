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

/// How long an index request may stay unanswered before the next change asks again.
const INDEX_PATIENCE: std::time::Duration = std::time::Duration::from_secs(2);

/// How often the column's spinner and sweep step while an agent works: twelve frames a
/// second, enough for a sweep across thirty columns to read as motion rather than jumps.
const MOTION_TICK: std::time::Duration = std::time::Duration::from_millis(80);

/// How often agent costs are sampled while `QORO_USAGE=1`: often enough to follow a
/// build, seldom enough that the percent is a real average and not noise.
const USAGE_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

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
    /// The first line of a finished agent's last answer, for the finish it was read for.
    Head(u32, Option<i64>, Option<String>),
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

/// The first line of an agent's last answer with its markdown marks taken off, for the
/// row that says it finished: `**Tests pass**: 1618 ok` reads `Tests pass: 1618 ok`.
fn answer_head(text: &str) -> Option<String> {
    text.lines()
        .map(|line| {
            line.trim()
                .trim_start_matches(['#', '>', '-', '*', '+'])
                .replace("**", "")
                .replace('`', "")
                .trim()
                .to_string()
        })
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(200).collect())
}

/// The finished agents in the pin whose last answer has not been asked for yet, marked
/// asked, as (terminal, finish time, program, session).
///
/// ponytail: `heads_asked` keeps every finish it has seen; a client lives for hours, not
/// for millions of finishes.
fn heads_wanted(state: &mut State) -> Vec<(u32, Option<i64>, String, Option<String>)> {
    let wanted: Vec<_> = state
        .rows
        .iter()
        // Another machine's transcripts are not on this disk: reading "the newest claude
        // answer" here would put this machine's answer on that machine's card.
        .filter(|r| r.agent && !crate::mux::is_remote_terminal(r.terminal))
        .filter(|r| !matches!(r.state.as_str(), "blocked" | "failed"))
        .filter(|r| {
            sidebar::needs_person(r, state.focused, &state.seen_done, &state.unseen_finished)
        })
        .filter(|r| {
            !state
                .heads_asked
                .contains(&(r.terminal, r.last_active_at_unix))
        })
        .map(|r| {
            (
                r.terminal,
                r.last_active_at_unix,
                r.program.clone().unwrap_or_default(),
                r.agent_session.clone(),
            )
        })
        .collect();
    for (terminal, stamp, _, _) in &wanted {
        state.heads_asked.insert((*terminal, *stamp));
    }
    wanted
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

/// Whether the column may move at all: yes, unless `QORO_MOTION=0` or `NO_COLOR` is set.
fn motion_on() -> bool {
    std::env::var("QORO_MOTION").map_or(true, |v| v.trim() != "0")
        && std::env::var_os("NO_COLOR").is_none()
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
    /// `TerminalCreated`/`Failed` answer.
    pending_agents: std::collections::BTreeMap<String, String>,
    /// Lines owed to new agent tabs, by terminal. Written at the top of the loop,
    /// where there is always a write in hand; the shell holds the line as
    /// typeahead, so it lands even before its first prompt is drawn.
    agent_typing: Vec<(u32, Vec<u8>)>,
    cols: u16,
    lines: u16,
    message: Option<String>,
    /// A terminal to move to once the loop can write the subscription for it.
    pending_focus: Option<u32>,
    /// A terminal `qoro focus` named before this interface listed it: a launcher that
    /// opens one and shows it at once gets there first. Shown once an index has it; a
    /// few seconds on, a terminal that never came is forgotten rather than jumped to.
    show_when_listed: Option<(u32, std::time::Instant)>,
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
    /// An overlay showing something read only: the keys, or what this daemon is set to.
    reading: Option<Reading>,
    /// Spaces asked for and not yet answered for.
    ///
    /// Only the naming needs this, and the naming needs it badly: two presses of `ctrl-a w`
    /// before either answer came back both counted the same list and both produced
    /// `space 2`, so the panel showed two rows with one name on them.
    creating: usize,
    /// How the column orders its agents: the tree of spaces, or flat by urgency.
    view: sidebar::View,
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
    /// The first line of each finished agent's last answer, for its row in the pin.
    heads: sidebar::Heads,
    /// The (terminal, finish time) pairs whose answer has been asked for, so each finish
    /// is read once however many frames it stays pinned.
    heads_asked: std::collections::BTreeSet<(u32, Option<i64>)>,
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
    /// The colours last told to the daemon, so an unchanged answer is not sent again.
    host_sent: Option<([u8; 3], [u8; 3])>,
    /// The daemon predates `HostColors` and said so: not asked again this session.
    host_refused: bool,
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
}

impl Modes {
    /// Whether a mode owns the keyboard.
    fn open(&self) -> bool {
        self.copy.is_some() || self.resizing || self.confirm_close.is_some()
    }
}

/// What resize mode says on the notice row: its own keys, because in a mode the others do
/// not work.
const RESIZE_KEYS: &str = "resize: h j k l move 5%  H J K L 20%  = equalize  esc done";

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
            panes: self.panes(),
            focused: self.focused,
            snapshot: self.snapshot.as_ref(),
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
                usage: &self.usage,
                heads: &self.heads,
                answer: self.answer.is_some(),
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
            .filter(|r| !panes.contains(&r.terminal))
            .collect()
    }

    /// Every terminal that is a pane inside somebody's tab. See `is_pane`.
    fn panes(&self) -> std::collections::BTreeSet<u32> {
        self.trees()
            .flat_map(|tree| tree.leaves().into_iter().skip(1))
            .collect()
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

    /// What an open mode says on the notice row, if one is open.
    fn mode_line(&self) -> Option<&str> {
        if let Some((_, question)) = &self.modes.confirm_close {
            return Some(question);
        }
        if self.modes.resizing {
            return Some(RESIZE_KEYS);
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
        // The last painted frame's clock, and frame zero of the animation: tiers move
        // rows, the tick never does, and a click maps onto what the paint drew.
        let column = self.column(now_unix, 0);
        let hit = if self.collapsed {
            sidebar::rows_collapsed(&column, column_rows)
        } else {
            // The paint reserves art rows above the foot when a pet is picked, so the
            // click maps onto the same layout it drew.
            sidebar::rows_folded(&column, column_rows, sidebar::current_avatar().as_ref())
        }
        .into_iter()
        .nth(within)
        .and_then(|(_, hit)| hit)?;
        // A row that does two things answers with the row; the column decides which.
        match sidebar::act(hit, col as usize, &column, self.collapsed)? {
            sidebar::Act::Space(space) => Some(Clicked::Space(space)),
            sidebar::Act::Fold(space) => Some(Clicked::Fold(space)),
            sidebar::Act::Terminal(terminal) => Some(Clicked::Terminal(terminal)),
            sidebar::Act::NewSpace => Some(Clicked::NewSpace),
            sidebar::Act::Menu => Some(Clicked::Menu),
            sidebar::Act::View => Some(Clicked::View),
            sidebar::Act::Collapse => Some(Clicked::Collapse),
        }
    }

    /// The pieces the left column is drawn from, at a given clock and animation frame.
    fn column(&self, now_unix: i64, tick: u64) -> sidebar::Column<'_> {
        sidebar::Column {
            spaces: &self.spaces,
            agents: &self.rows,
            focused_space: self.space.unwrap_or_default(),
            focused_terminal: self.focused,
            tick,
            view: self.view,
            seen: &self.seen_done,
            unseen: &self.unseen_finished,
            now_unix,
            width: self.sidebar_width,
            shut: &self.shut,
            usage: Some(&self.usage),
            heads: Some(&self.heads),
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
    ("rename space", "R", "rename what is on screen"),
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
    ("settings", "", "what this daemon is set to"),
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
    /// The sessions on a machine about to be connected, then a row to start a new one.
    Sessions {
        target: String,
        sessions: Vec<qoro_runtime::control::RemoteSession>,
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
        MenuKind::Sessions { sessions, .. } => {
            let mut rows: Vec<(String, String)> = sessions
                .iter()
                .map(|s| {
                    let tabs = match s.tabs {
                        1 => "1 tab".to_string(),
                        n => format!("{n} tabs"),
                    };
                    let state = if s.running { "running" } else { "stopped" };
                    (s.name.clone(), format!("{state}  {tabs}"))
                })
                .collect();
            // Nothing there yet: the session it would open anyway, named, so the first
            // choice is not a blank.
            if rows.is_empty() {
                rows.push(("default".to_string(), "new".to_string()));
            }
            rows.push(("new session".to_string(), "name it".to_string()));
            rows
        }
        MenuKind::Install { platform, .. } => vec![
            (
                format!("install qoro {} there", env!("CARGO_PKG_VERSION")),
                platform.clone(),
            ),
            ("cancel".to_string(), String::new()),
        ],
    }
}

/// What the menu box says it is.
pub fn menu_title(kind: &MenuKind) -> String {
    match kind {
        MenuKind::Main => " menu".to_string(),
        MenuKind::Plus => " spaces".to_string(),
        MenuKind::Sessions { target, .. } => format!(" sessions on {target}"),
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

/// Ask the daemon for a tab running an agent command, and remember the line to
/// type once the tab opens.
///
/// The daemon only starts a shell (ADR-015): the command is typed, never spawned,
/// the same path `qoro new -- <agent>` takes. The tab takes the command's first
/// word as its name, so it reads as the agent until renamed. Empty commands never
/// reach here: the prompt treats them as a cancel. The caller owns the request
/// number, incremented the same way every other prompt answer is.
fn start_agent_terminal(state: &mut State, id: u64, command: &str) -> ControlRequest {
    let id = format!("agent-{id}");
    let name = command
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
        .filter(|word| !word.is_empty());
    state.pending_agents.insert(id.clone(), command.to_string());
    ControlRequest::CreateTerminal {
        id,
        // Like every other new tab: the agent starts where the
        // person already is rather than where the daemon started.
        cwd: focused_cwd(state),
        // The space the person is looking at.
        workspace: state.space,
        name,
    }
}

/// The line an agent tab is owed, from its `TerminalCreated` answer, or `None`
/// when the answer carries no agent.
///
/// The id prefix is the whole protocol: only `agent-` requests recorded a line
/// in `pending_agents`. A `Failed` answer calls this too and drops the line, so
/// a tab that never opened types nothing.
fn take_agent_command(state: &mut State, id: &str) -> Option<Vec<u8>> {
    if !id.starts_with("agent-") {
        return None;
    }
    state.pending_agents.remove(id).map(|command| {
        let mut line = command.into_bytes();
        line.push(b'\r');
        line
    })
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
    let ask = |what| {
        Some(Prompt {
            what,
            text: String::new(),
        })
    };
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
        MenuKind::Sessions { target, sessions } => {
            // The last row names a new session; every row before it is one to open, or
            // the default when the machine had none (see `menu_rows`).
            let picked = if item >= sessions.len().max(1) {
                state.connecting = Some(target);
                state.prompt = ask(Naming::RemoteSession);
                return MenuSend::Nothing;
            } else {
                sessions
                    .get(item)
                    .map_or_else(|| "default".to_string(), |s| s.name.clone())
            };
            *request += 1;
            state.message = Some(format!("connecting to {target}"));
            return MenuSend::Control(ControlRequest::ConnectRemote {
                id: format!("connect-{request}"),
                target,
                session: picked,
            });
        }
    }
    match MENU[item].0 {
        "connect a machine" => {
            state.prompt = ask(Naming::Remote);
            MenuSend::Nothing
        }
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
                state.message = Some(format!(
                    "nothing to rename, {} c starts a terminal",
                    crate::keys::prefix_name()
                ));
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
            } else if let Some(question) = close_question(state) {
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
            MenuSend::Nothing
        }
        "settings" => {
            // Read again on the way in, so the box shows the file as it is now and an
            // edit made since the start takes effect here, with no watcher and no restart.
            use_config(state, &crate::config::read());
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
    /// Which agent to run in a new tab, from the palette's agent row. The answer is a
    /// command line, not a tab name: the tab takes the command's first word, and the
    /// whole line is typed into its shell once the tab opens. Any command works, so
    /// an agent the table never heard of starts the same way a listed one does.
    Agent,
    /// Rename what already exists, which is the common case: a name given at creation is a
    /// guess, and the terminal somebody opened to "look at something" is the one they are
    /// still in an hour later doing the thing it should have been called after.
    RenameSpace(i64),
    RenameTerminal(u32),
    /// The ssh address of a machine to connect, from the `+` beside the spaces.
    Remote,
    /// A new session on the machine being connected.
    RemoteSession,
}

impl Naming {
    /// What the box says it is asking for.
    pub fn asks(self) -> &'static str {
        match self {
            Naming::Space => "name the new space",
            Naming::Terminal => "name the new terminal",
            Naming::Agent => "which agent to run",
            Naming::RenameSpace(_) => "rename this space",
            Naming::RenameTerminal(_) => "rename this tab",
            Naming::Remote => "ssh address of the machine",
            Naming::RemoteSession => "name the new session",
        }
    }

    /// How many columns an answer may take. An address is longer than a name and is shown
    /// in the box, which is wider than the column a name has to fit. An agent command can
    /// carry flags, so it gets the wider box too.
    pub fn limit(self) -> usize {
        match self {
            Naming::Remote | Naming::Agent => 56,
            _ => NAME_LIMIT,
        }
    }

    /// What an empty answer means here, which is not the same thing in all four cases.
    pub fn hint(self) -> &'static str {
        match self {
            Naming::Space | Naming::Terminal => {
                "enter to make it, esc to stop, empty for a default name"
            }
            Naming::Agent => "an agent command, like claude; enter to start it, esc to stop",
            Naming::RenameTerminal(_) => {
                "enter to rename, esc to stop, empty for the program's own name"
            }
            // A space with no name is a space nobody can point at, so there is nothing for
            // empty to mean and it is a cancel.
            Naming::RenameSpace(_) => "enter to rename, esc to stop",
            Naming::Remote => "like admin@10.0.0.5; enter lists its sessions, esc to stop",
            Naming::RemoteSession => "enter to connect, esc to stop, empty for default",
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
    let prefix = state.config.prefix;
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
            byte if *byte == prefix || *byte == crate::keys::DETACH => {
                state.prompt = None;
                return PromptOutcome::Passthrough(bytes[at..].to_vec());
            }
            byte if (byte.is_ascii_graphic() || *byte == b' ')
                && crate::width::width(&prompt.text) < prompt.what.limit() =>
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
    let outcome = if state.modes.confirm_close.is_some() {
        confirm_key(state, &input, request)
    } else if state.modes.resizing {
        resize_key(state, &input)
    } else {
        copy_key(state, &input, prefix, request)
    };
    // Held for a mode that is still open, and never more than a sequence's worth: anything
    // longer that has not ended is not one.
    if !state.modes.open() || state.modes.pending.len() > 64 {
        state.modes.pending.clear();
    }
    outcome
}

/// The question to ask before closing the focused terminal, when closing it would end an
/// agent in the middle of something. Shells and resting agents close at once.
fn close_question(state: &State) -> Option<String> {
    let row = state.rows.iter().find(|r| r.terminal == state.focused)?;
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
fn close_focused(state: &mut State, request: &mut u64) {
    if state.focused == 0 {
        return;
    }
    match close_question(state) {
        Some(question) => state.modes.confirm_close = Some((state.focused, question)),
        None => {
            *request += 1;
            state.modes.outbox.push(ControlRequest::CloseTerminal {
                id: format!("close-{request}"),
                terminal: state.focused,
            });
        }
    }
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
                    copy.status = crate::copy::KEYS.to_string();
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

/// Send what the modes asked for, and copy a finished yank. Runs wherever the loop has a
/// write in hand.
async fn drain_modes(
    state: &mut State,
    control_seq: &mut u32,
    wr: &mut (impl tokio::io::AsyncWriteExt + Unpin),
    dirty: &mut bool,
) -> io::Result<()> {
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
enum Clicked {
    Menu,
    Fold(i64),
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
        sized: std::collections::BTreeMap::new(),
        layouts: std::collections::BTreeMap::new(),
        parked: std::collections::BTreeMap::new(),
        split_writes: Vec::new(),
        split_saving: 0,
        resync: false,
        split_origin: std::collections::BTreeMap::new(),
        pending_agents: std::collections::BTreeMap::new(),
        agent_typing: Vec::new(),
        cols,
        lines,
        // What the caller wanted said, or that this is connecting. A notice about a
        // missing hook is worth more than the word "connecting", which is gone by the time
        // anybody reads it.
        message: Some(CONNECTING.to_string()),
        hint: notice,
        pending_focus: None,
        show_when_listed: None,
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

        shut: sidebar::Shut::new(),

        in_column: false,

        prefix_pending: false,

        answer: None,

        usage: sidebar::Usage::new(),

        heads: sidebar::Heads::new(),

        heads_asked: std::collections::BTreeSet::new(),

        usage_seen: std::collections::BTreeMap::new(),
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
        host_sent: None,
        host_refused: false,
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
    // `1006` asks for those reports in the extended form, because the original one cannot
    // express a column past 223 and this interface is regularly wider than that.
    out.write_all(b"\x1b[?1049h\x1b[?1002h\x1b[?1006h")?;
    // Focus reports (`CSI ? 1004 h`), so the interface knows when the person looked away,
    // and the questions about the terminal's own colours. See `input`.
    out.write_all(b"\x1b[?1004h")?;
    if may_ask_terminal() {
        out.write_all(ASK_COLORS)?;
        out.write_all(ASK_SCHEME)?;
    }
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
    // The id of that request, so only its own answer clears it: behind the mux one request
    // can be answered once per machine, and a late answer must not open the gate early.
    let mut index_id = String::new();
    let mut index_owed = false;
    // The column moves only while something works and motion is on: `QORO_MOTION=0` or
    // `NO_COLOR` keeps it still. The next frame's deadline, when one is armed.
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
            // At most one frame per `FRAME`. The first render after a quiet spell goes out
            // at once, so a keystroke echoes with no added wait; renders arriving closer
            // together than that wait for the timer arm below and go out as one frame.
            // `ctrl-a` waiting for its letter produces no action of its own, so the key row
            // learns it here: a flip either way repaints the row.
            if keys.prefix_pending() != state.prefix_pending {
                state.prefix_pending = keys.prefix_pending();
                dirty = true;
            }
            // A finished agent in the pin says the first line of its answer, read off the
            // loop's thread once per finish.
            for (terminal, stamp, program, session) in heads_wanted(&mut state) {
                let tx = blocking_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let read = || {
                        last_answer(&program, session.as_deref())
                            .ok()
                            .and_then(|text| answer_head(&text))
                    };
                    // The finish can reach the daemon a moment before the transcript's
                    // last write reaches the disk: one more look before giving up.
                    let head = read().or_else(|| {
                        std::thread::sleep(std::time::Duration::from_millis(1500));
                        read()
                    });
                    let _ = tx.send(Blocked::Head(terminal, stamp, head));
                });
            }
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
                && sidebar::moves(&state.column(0, 0));
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
                }
                _ = sigterm.recv() => return Ok(()),
                _ = sighup.recv() => return Ok(()),
                _ = sigint.recv() => return Ok(()),
                _ = sigwinch.recv() => {
                    let (cols, lines) = client::outer_size();
                    state.cols = cols;
                    state.lines = lines;
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
                    if ask_again && may_ask_terminal() {
                        out.write_all(ASK_COLORS)?;
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
                                    palette::Item::NewAgentAsk => {
                                        state.prompt = Some(Prompt {
                                            what: Naming::Agent,
                                            text: String::new(),
                                        });
                                    }
                                    palette::Item::NewAgent(program) => {
                                        request += 1;
                                        let ask = start_agent_terminal(
                                            &mut state,
                                            request,
                                            &program,
                                        );
                                        state.message = Some(format!("starting {program}"));
                                        wr.write_all(&client::encode_control_seq(
                                            &ask, &mut control_seq,
                                        )?)
                                        .await?;
                                    }
                                    palette::Item::NewSpace => {
                                        state.prompt = Some(Prompt {
                                            what: Naming::Space,
                                            text: String::new(),
                                        });
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
                        // A key or a click closes a thing that is only being read.
                        // Through the one decoder, so half a report carried across
                        // reads is still half a report, not two keys.
                        if closes_reading(&keys.feed(&bytes)) {
                            state.reading = None;
                            dirty = true;
                        }
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
                                        Some(ControlRequest::RenameTerminal {
                                            id: format!("rename-{request}"),
                                            terminal,
                                            // Empty hands the tab back to its program's
                                            // name, which is the only way to undo a name
                                            // without inventing a second key for it.
                                            name,
                                        })
                                    }
                                    // A space with no name is a space nobody can point
                                    // at, so an empty answer is a cancel. Not a
                                    // `continue`: that also dropped the keys typed after
                                    // the enter, which are in `feed` and run below.
                                    Naming::RenameSpace(workspace) => {
                                        name.map(|name| ControlRequest::RenameWorkspace {
                                            id: format!("rename-{request}"),
                                            workspace,
                                            name,
                                        })
                                    }
                                    Naming::Terminal => Some(ControlRequest::CreateTerminal {
                                        id: format!("new-{request}"),
                                        // The new tab follows the
                                        // focused terminal's directory.
                                        cwd: focused_cwd(&state),
                                        // The space the person is looking at.
                                        workspace: state.space,
                                        name,
                                    }),
                                    // A command line, not a name: empty is a cancel, as
                                    // with a space's name, and anything else starts an
                                    // agent tab, listed or not.
                                    Naming::Agent => name.map(|command| {
                                        start_agent_terminal(&mut state, request, &command)
                                    }),
                                    Naming::Space => Some(ControlRequest::CreateWorkspace {
                                        id: format!("space-{request}"),
                                        // Empty means "you name it", which is the numbered
                                        // name this counted before anybody was asked.
                                        name: name.unwrap_or_else(|| next_space_name(&state)),
                                    }),
                                    // An address is what the box is for: empty is a
                                    // cancel, as with a space's name.
                                    Naming::Remote => name.map(|target| {
                                        let id = format!("probe-{request}");
                                        state.probing = Some(id.clone());
                                        state.connecting = Some(target.clone());
                                        ControlRequest::ListRemoteSessions { id, target }
                                    }),
                                    Naming::RemoteSession => {
                                        state.connecting.clone().map(|target| {
                                            ControlRequest::ConnectRemote {
                                                id: format!("connect-{request}"),
                                                target,
                                                session: name
                                                    .unwrap_or_else(|| "default".into()),
                                            }
                                        })
                                    }
                                };
                                if let Some(ask) = ask {
                                    if matches!(what, Naming::Space) {
                                        state.creating += 1;
                                    }
                                    wr.write_all(&client::encode_control_seq(
                                        &ask, &mut control_seq,
                                    )?).await?;
                                    state.message = Some(match what {
                                        Naming::Terminal => "starting a terminal".to_string(),
                                        Naming::Agent => match &ask {
                                            ControlRequest::CreateTerminal {
                                                name: Some(tab),
                                                ..
                                            } => format!("starting {tab}"),
                                            _ => "starting an agent".to_string(),
                                        },
                                        Naming::Space => "starting a space".to_string(),
                                        Naming::RenameSpace(_) | Naming::RenameTerminal(_) => {
                                            "renamed".to_string()
                                        }
                                        Naming::Remote => format!(
                                            "looking at {}",
                                            state.connecting.as_deref().unwrap_or_default()
                                        ),
                                        Naming::RemoteSession => format!(
                                            "connecting to {}",
                                            state.connecting.as_deref().unwrap_or_default()
                                        ),
                                    });
                                }
                                dirty = true;
                            }
                            PromptOutcome::Passthrough(rest) => feed = Some(rest),
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
                    // The settings box read the file again and the prefix changed: the
                    // decoder starts over with the new one.
                    if keys.prefix() != state.config.prefix {
                        keys = Keys::with_prefix(state.config.prefix);
                    }
                    for action in keys.feed(&input) {
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
                                    // Two things a new space can be: one here, or the
                                    // spaces of another machine. `ctrl-a w` still goes
                                    // straight to the first.
                                    Some(Clicked::NewSpace) => {
                                        state.menu_kind = MenuKind::Plus;
                                        state.menu = Some(0);
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
                                        state.menu_kind = MenuKind::Main;
                                        state.menu = Some(0);
                                        dirty = true;
                                    }
                                    Some(Clicked::Fold(space)) => {
                                        if !state.shut.remove(&space) {
                                            state.shut.insert(space);
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
                                state.prompt = Some(Prompt {
                                    what: Naming::RenameTerminal(state.focused),
                                    text: current,
                                });
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
                            // A busy agent is asked about first: one stray `x` after the
                            // prefix must not end an hour of its work.
                            Action::Close => {
                                close_focused(&mut state, &mut request);
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
                                state.palette = Some(Palette::open());
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
                            // Only in the tree, and only a space with agents in it: a fold with
                            // nothing under it shows nothing, and would hide the first agent that
                            // arrives later without anybody having seen it fold.
                            Action::FoldSpace => {
                                if let Some(space) = state.space
                                    && state.view == sidebar::View::Tree
                                    && state.rows.iter().any(|a| a.agent && a.workspace == space)
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
                                        Some((terminal, std::time::Instant::now()));
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
                    // qoro was just put on a machine the person is connecting: ask it about its
                    // sessions again, and the list follows as if it had been there all along.
                    if let Some(target) = state.reprobe.take() {
                        request += 1;
                        let id = format!("probe-{request}");
                        state.probing = Some(id.clone());
                        state.connecting = Some(target.clone());
                        wr.write_all(&client::encode_control_seq(
                            &ControlRequest::ListRemoteSessions { id, target },
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
                    Blocked::Head(terminal, stamp, Some(head)) => {
                        state.heads.insert(terminal, (stamp, head));
                        dirty = true;
                    }
                    Blocked::Head(_, _, None) => {}
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
    let _ = out.write_all(b"\x1b[?2031l\x1b[?1004l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?1049l");
    let _ = out.flush();
    outcome
}

/// Put a read of `config.toml` in effect: the prefix every overlay and the screen's words
/// use, and what the settings box describes. The decoder follows the prefix on the next
/// keystroke (see the stdin arm).
fn use_config(state: &mut State, read: &crate::config::Read) {
    state.config = read.config;
    crate::keys::show_prefix(read.config.prefix);
    retheme(state);
    crate::config::show(crate::config::describe(
        read,
        crate::theme::light(),
        &crate::notify::describe(state.route),
    ));
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

/// Whether the terminal may be asked at all. The Linux console answers none of these and
/// has been seen to print the question instead.
fn may_ask_terminal() -> bool {
    std::env::var("TERM").map_or(true, |term| term != "linux")
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
                    Some(_) => match buf[i + 2..]
                        .iter()
                        .position(|b| (0x40..=0x7e).contains(b) || *b < 0x20)
                    {
                        Some(at) if buf[i + 2 + at] >= 0x40 => i += 2 + at + 1,
                        Some(at) => i += 2 + at,
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
    if to == 0 || to == state.focused {
        return Ok(None);
    }
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
    // Dropped from the onlookers: a snapshot kept past its unsubscription is
    // a screen that can never update again.
    let visible: std::collections::BTreeSet<u32> = visible_set(state).into_iter().collect();
    state.snapshots.retain(|t, _| visible.contains(t));
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

/// Take the keyboard out of every terminal, because this space has none to put it in.
///
/// The unsubscribe is the point. Leaving the old terminal focused kept every keystroke
/// going to a terminal in a space the person had left and was not looking at, kept its
/// output painting under this space's tab bar, and left `ctrl-a x` closing it. Nothing on
/// screen said any of that was happening.
fn unfocus(state: &mut State, why: &str, control_seq: &mut u32) -> io::Result<Option<BytesMut>> {
    state.focused = 0;
    state.snapshot = None;
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
        MenuKind::Sessions { target, .. } | MenuKind::Install { target, .. } => target.clone(),
        MenuKind::Main | MenuKind::Plus => String::new(),
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
fn apply(state: &mut State, response: ControlResponse, dirty: &mut bool) {
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
            state.spaces = workspaces;
            // A fold outlives nothing it folded: a space that went away takes its fold
            // with it, so a later space reusing the id does not start folded.
            let live: sidebar::Shut = state.spaces.iter().map(|s| s.id).collect();
            state.shut.retain(|id| live.contains(id));
            *dirty = true;
        }
        ControlResponse::WorkspaceCreated { workspace, .. } => {
            state.creating = state.creating.saturating_sub(1);
            state.space = Some(workspace);
            state.snapshot = None;
            // A new space starts bare: no tree. The old space's subscriptions
            // stay in `subs` until the next sync drops them, since `apply`
            // cannot write: forgetting them here meant the unsubscribe never
            // went out, the daemon kept them, and coming back subscribed to a
            // terminal it still watched, which asks it for no screen. The
            // render filter drops the old space's renders by space meanwhile.
            state.layouts.remove(&workspace);
            state.parked.remove(&workspace);
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
            state.parked.remove(&workspace);
            if state.space == Some(workspace) {
                state.space = None;
                state.snapshot = None;
            }
            *dirty = true;
        }
        ControlResponse::Agents { agents, splits, .. } => {
            // First, so the bootstrap below already knows which rows are panes.
            adopt_splits(state, &agents, splits);
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
            // TerminalClosed and Agents prune the local set silently: the
            // daemon ended the terminal, which ends its subscription
            // server-side too, so there is nothing to unsubscribe from. The
            // assumption is one-directional on purpose: pruning an id the
            // daemon still streams would resurface as a render for an unknown
            // terminal, which the filter above drops.
            let live: std::collections::BTreeSet<u32> = agents.iter().map(|r| r.terminal).collect();
            state.snapshots.retain(|t, _| live.contains(t));
            state.subs.retain(|t| live.contains(t));
            // Pictures too, up to 4 MiB each, or every terminal that exited on its own
            // kept its last one for the life of this client.
            state.images.retain(|t, _| live.contains(t));
            // And what each was last sized to: one that comes back (a remote daemon that
            // restarted while its link was down restores its tabs at 80x24) has to be
            // sized again, and an equal cached size would send it nothing.
            state.sized.retain(|t, _| live.contains(t));
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
                    state.message = Some(format!(
                        "this space is empty, {} c starts a terminal",
                        crate::keys::prefix_name()
                    ));
                }
            }
            // Nothing left anywhere. The interface is a way into terminals, so with none of
            // them it is a screen a person has to know a key to escape from. Ending is what
            // every multiplexer does when its last window closes, and the daemon stays: it
            // is the client that goes.
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
            if state.adopted
                && state.focused == 0
                && state.pending_focus.is_none()
                && let Some(space) = state.space
            {
                state.pending_focus = agents
                    .iter()
                    .find(|r| {
                        r.workspace == space && !state.is_pane(r.terminal) && r.state != "sleeping"
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
                let was_working = before
                    .as_deref()
                    .is_some_and(|b| matches!(b, "working" | "starting"));
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
            if let Some((terminal, asked)) = state.show_when_listed.take()
                && asked.elapsed() < std::time::Duration::from_secs(5)
            {
                if state.rows.iter().any(|r| r.terminal == terminal) {
                    state.pending_focus = Some(terminal);
                } else {
                    state.show_when_listed = Some((terminal, asked));
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
                // first version did.
                state.snapshot = None;
            }
            // An unbound split stays where the keyboard is: yanking the focus
            // to a terminal in another space would collapse that space's tree
            // onto one pane. So does one bound into a parked tree: the person
            // has moved to another tab since.
            let on_screen =
                bound && matches!(home, Some((space, None)) if state.space == Some(space));
            if on_screen || !is_split {
                state.pending_focus = Some(terminal);
            }
            // An agent tab's shell is up: queue its command line. The shell holds
            // the line as typeahead, so it lands even before its first prompt.
            if let Some(line) = take_agent_command(state, &id) {
                state.agent_typing.push((terminal, line));
            }
            *dirty = true;
        }
        ControlResponse::TerminalClosed { terminal, .. } => {
            // Out of every tree, so no space keeps a pane nobody can focus.
            // Trees left with no leaves and no pending split go away, and the
            // unsubscription of a tile nobody watches piggybacks on the next
            // sync: the daemon already ended the terminal itself.
            prune_trees(state, |t| t == terminal);
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
            // A daemon older than this client, refusing the colours it was told. It has
            // nothing to do with them anyway: said once, silently, and never again.
            if id.is_empty() && reason.contains("`HostColors`") {
                state.host_refused = true;
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

        // The sessions on the machine being connected. Opened only as the answer to the
        // question still standing, and only over nothing else: an answer can take seconds,
        // and a list that opened over a box the person has moved on to would take their
        // keys.
        ControlResponse::RemoteSessions {
            id,
            target,
            mut sessions,
        } => {
            // What is running first, then what holds the most, then by name.
            sessions.sort_by(|a, b| {
                b.running
                    .cmp(&a.running)
                    .then(b.tabs.cmp(&a.tabs))
                    .then(a.name.cmp(&b.name))
            });
            open_answer_list(state, &id, MenuKind::Sessions { target, sessions });
            *dirty = true;
        }
        // No qoro on the machine being connected: the same kind of list, offering to put it
        // there, under the same rules.
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
        // Said whatever the person is doing now, because it happened; the list of its
        // sessions follows only while they are still waiting for it.
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

    /// A finished row says the first line of the answer as a person reads it: the heading
    /// and emphasis marks are the page's, not the words.
    #[test]
    fn an_answer_head_is_its_first_line_without_markdown() {
        assert_eq!(
            answer_head("\n## **Done**: `1618` ok\nmore"),
            Some("Done: 1618 ok".to_string())
        );
        assert_eq!(
            answer_head("- **Sebep:** a rule"),
            Some("Sebep: a rule".to_string())
        );
        assert_eq!(answer_head("  \n \n"), None);
        assert_eq!(answer_head(&"x".repeat(500)).map(|h| h.len()), Some(200));
    }

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
            activity: None,
            pgid: None,
            discards: None,
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
            show_when_listed: None,
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

            shut: sidebar::Shut::new(),

            in_column: false,

            prefix_pending: false,

            answer: None,

            usage: sidebar::Usage::new(),

            heads: sidebar::Heads::new(),

            heads_asked: std::collections::BTreeSet::new(),

            usage_seen: std::collections::BTreeMap::new(),
            seen_done: sidebar::Seen::new(),
            unseen_finished: sidebar::Unseen::new(),
            tick: 0,
            paint_now: 0,
            sidebar_width: sidebar::WIDTH,
            sidebar_drag: None,
            sidebar_sized: sidebar::WIDTH,
            palette: Some(Palette::open()),
            palette_pending: Vec::new(),
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
            host_sent: None,
            host_refused: false,
            notes: Vec::new(),
            notes_due: None,
            route: crate::notify::Route::Nowhere(crate::notify::Why::NoProgram),
            nowhere: false,
            config: crate::config::Config::default(),
            snapshots: std::collections::BTreeMap::new(),
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
            pending_agents: std::collections::BTreeMap::new(),
            agent_typing: Vec::new(),
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
        let ask = start_agent_terminal(&mut s, 7, "claude");
        match ask {
            ControlRequest::CreateTerminal { id, name, .. } => {
                assert_eq!(id, "agent-7");
                assert_eq!(name.as_deref(), Some("claude"));
            }
            other => panic!("an agent tab is a create: {other:?}"),
        }
        assert_eq!(
            s.pending_agents.get("agent-7").map(String::as_str),
            Some("claude")
        );
    }

    /// Flags stay on the typed line, off the tab name: the tab says the agent,
    /// the shell gets the whole command.
    #[test]
    fn an_agent_command_with_flags_keeps_the_whole_line() {
        let mut s = state();
        let ask = start_agent_terminal(&mut s, 7, "my-agent --resume");
        match ask {
            ControlRequest::CreateTerminal { name, .. } => {
                assert_eq!(name.as_deref(), Some("my-agent"));
            }
            other => panic!("an agent tab is a create: {other:?}"),
        }
        assert_eq!(
            s.pending_agents.get("agent-7").map(String::as_str),
            Some("my-agent --resume")
        );
    }

    /// The `TerminalCreated` answer for an agent queues its line with enter,
    /// and an answer for any other tab queues nothing.
    #[test]
    fn a_terminal_answer_for_an_agent_queues_its_line() {
        let mut s = state();
        s.pending_agents
            .insert("agent-7".to_string(), "claude".to_string());
        assert_eq!(
            take_agent_command(&mut s, "agent-7"),
            Some(b"claude\r".to_vec())
        );
        assert!(s.pending_agents.is_empty(), "consumed, not kept");
        assert_eq!(take_agent_command(&mut s, "new-8"), None);
        s.pending_agents
            .insert("agent-9".to_string(), "codex".to_string());
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
        state.prompt = Some(Prompt {
            what: Naming::Terminal,
            text: String::new(),
        });
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
            sized: std::collections::BTreeMap::new(),
            layouts: std::collections::BTreeMap::new(),
            parked: std::collections::BTreeMap::new(),
            split_writes: Vec::new(),
            split_saving: 0,
            resync: false,
            split_origin: std::collections::BTreeMap::new(),
            pending_agents: std::collections::BTreeMap::new(),
            agent_typing: Vec::new(),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            pending_focus: None,
            show_when_listed: None,
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

            shut: sidebar::Shut::new(),

            in_column: false,

            prefix_pending: false,

            answer: None,

            usage: sidebar::Usage::new(),

            heads: sidebar::Heads::new(),

            heads_asked: std::collections::BTreeSet::new(),

            usage_seen: std::collections::BTreeMap::new(),
            seen_done: sidebar::Seen::new(),
            unseen_finished: sidebar::Unseen::new(),
            tick: 0,
            paint_now: 0,
            sidebar_width: sidebar::WIDTH,
            sidebar_drag: None,
            sidebar_sized: sidebar::WIDTH,
            palette: None,
            palette_pending: Vec::new(),
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
            host_sent: None,
            host_refused: false,
            notes: Vec::new(),
            notes_due: None,
            route: crate::notify::Route::Nowhere(crate::notify::Why::NoProgram),
            nowhere: false,
            config: crate::config::Config::default(),
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
            rows: vec![agent(1, 1, "zsh", "idle"), agent(2, 1, "claude", "working")],
            focused: 1,
            snapshot: Some(snapshot_with_behind(0)),
            snapshots: std::collections::BTreeMap::from([(2, snapshot_with_behind(0))]),
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
            pending_agents: std::collections::BTreeMap::new(),
            agent_typing: Vec::new(),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            pending_focus: None,
            show_when_listed: None,
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

            shut: sidebar::Shut::new(),

            in_column: false,

            prefix_pending: false,

            answer: None,

            usage: sidebar::Usage::new(),

            heads: sidebar::Heads::new(),

            heads_asked: std::collections::BTreeSet::new(),

            usage_seen: std::collections::BTreeMap::new(),
            seen_done: sidebar::Seen::new(),
            unseen_finished: sidebar::Unseen::new(),
            tick: 0,
            paint_now: 0,
            sidebar_width: sidebar::WIDTH,
            sidebar_drag: None,
            sidebar_sized: sidebar::WIDTH,
            palette: None,
            palette_pending: Vec::new(),
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
            host_sent: None,
            host_refused: false,
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
            &mut dirty,
        );
        apply(
            &mut state,
            ControlResponse::TerminalCreated {
                id: "split-5".to_string(),
                terminal: 5,
            },
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
        apply(&mut state, index_with(vec![three_panes()]), &mut dirty);
        assert!(state.layouts.is_empty(), "no space yet, no tree on screen");
        apply(
            &mut state,
            ControlResponse::Workspaces {
                id: "spaces".to_string(),
                workspaces: vec![space(1, "app")],
            },
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
        apply(&mut state, index_with(vec![three_panes()]), &mut dirty);
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
        apply(&mut state, index, &mut dirty);
        assert_eq!(state.layouts.get(&1).map(|l| l.leaves()), Some(vec![9]));
        let tabs: Vec<u32> = state.tabs().iter().map(|r| r.terminal).collect();
        assert_eq!(tabs, vec![1, 9]);
    }

    #[test]
    fn a_split_this_client_holds_is_not_replaced_by_the_index() {
        let mut state = tiled_state();
        state.rows.push(agent(3, 1, "sh", "idle"));
        let mut dirty = false;
        apply(&mut state, index_with(vec![three_panes()]), &mut dirty);
        assert_eq!(state.layouts.get(&1).map(|l| l.leaves()), Some(vec![1, 2]));
        assert!(!state.is_pane(3), "the local tree wins; 3 stays a tab");
    }

    #[test]
    fn nothing_is_adopted_while_a_recording_is_on_its_way() {
        let mut state = fresh_client_with_one_pane_on_screen();
        state.split_saving = 1;
        let mut dirty = false;
        apply(&mut state, index_with(vec![three_panes()]), &mut dirty);
        assert_eq!(state.layouts.get(&1).map(|l| l.leaves()), Some(vec![1]));
        apply(
            &mut state,
            ControlResponse::SplitSaved {
                id: "tree-1".to_string(),
            },
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

    /// A visible terminal with no screen here is subscribed again although it already is,
    /// which the daemon answers with its live screen: back in a space whose copy was dropped,
    /// the pane is not left blank until the terminal prints.
    #[test]
    fn a_visible_terminal_with_no_screen_is_asked_for_one_again() {
        let mut state = tiled_state();
        let mut control_seq = 0;
        assert!(sync_subs(&mut state, &mut control_seq).unwrap().is_none());
        state.snapshot = None;
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
            &mut dirty,
        );
        assert!(state.layouts.get(&1).unwrap().contains(3));
        assert!(!state.layouts.get(&2).unwrap().contains(3));
        assert!(state.is_pane(3), "bound splits hide from the tab bar");
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
        assert_eq!(state.menu, Some(12));
        // Keyless rows answer to their first letter.
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
                "rename space",
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
        let row = (top + 2 + 2 + 1) as u16;
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
            &mut dirty,
        );
        assert_eq!(
            state.pending_focus,
            Some(1),
            "the keyboard comes back to its space: {:?}",
            state.pending_focus
        );
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
        state.show_when_listed = Some((9, std::time::Instant::now()));
        let mut dirty = false;
        // An index from before the new terminal: still waiting.
        apply(
            &mut state,
            index(vec![agent(1, 1, "zsh", "working")]),
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
        ));
        apply(
            &mut state,
            index(vec![
                agent(1, 1, "zsh", "working"),
                agent(9, 1, "claude", "idle"),
            ]),
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
                splits: Vec::new(),
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
                splits: Vec::new(),
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
                splits: Vec::new(),
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
                splits: Vec::new(),
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
                splits: Vec::new(),
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

    fn remote(name: &str, running: bool, tabs: usize) -> qoro_runtime::control::RemoteSession {
        qoro_runtime::control::RemoteSession {
            name: name.to_string(),
            running,
            tabs,
        }
    }

    /// The sessions come back sorted into a list to pick from; a pick connects to it, and
    /// the last row asks for a new session's name instead.
    #[test]
    fn a_session_is_picked_from_the_machine_or_named_new() {
        let mut state = bare_state();
        state.probing = Some("probe-1".into());
        let mut dirty = false;
        apply(
            &mut state,
            ControlResponse::RemoteSessions {
                id: "probe-1".into(),
                target: "admin@10.0.0.5".into(),
                sessions: vec![remote("old", false, 5), remote("work", true, 2)],
            },
            &mut dirty,
        );
        assert_eq!(state.menu, Some(0));
        let rows = menu_rows(&state.menu_kind);
        assert_eq!(rows[0].0, "work", "running sorts first: {rows:?}");
        assert_eq!(rows[1].0, "old");
        assert_eq!(rows[2].0, "new session");
        let mut request = 0;
        match apply_menu_choice(&mut state, 0, &mut request) {
            MenuSend::Control(ControlRequest::ConnectRemote {
                target, session, ..
            }) => {
                assert_eq!(target, "admin@10.0.0.5");
                assert_eq!(session, "work");
            }
            _ => panic!("picking a session does not connect to it"),
        }

        state.menu_kind = MenuKind::Sessions {
            target: "admin@10.0.0.5".into(),
            sessions: vec![],
        };
        assert_eq!(menu_rows(&state.menu_kind)[0].0, "default");
        match apply_menu_choice(&mut state, 0, &mut request) {
            MenuSend::Control(ControlRequest::ConnectRemote { session, .. }) => {
                assert_eq!(session, "default")
            }
            _ => panic!("a machine with no sessions does not offer the default"),
        }
        state.menu_kind = MenuKind::Sessions {
            target: "admin@10.0.0.5".into(),
            sessions: vec![],
        };
        apply_menu_choice(&mut state, 1, &mut request);
        assert!(
            state
                .prompt
                .as_ref()
                .is_some_and(|p| p.what == Naming::RemoteSession)
        );
        assert_eq!(state.connecting.as_deref(), Some("admin@10.0.0.5"));
    }

    /// An answer to a question no longer standing, or one that lands on an open box, never
    /// opens a list over what the person is doing.
    #[test]
    fn a_late_session_list_does_not_take_the_keys() {
        let mut state = bare_state();
        let mut dirty = false;
        let answer = |id: &str| ControlResponse::RemoteSessions {
            id: id.into(),
            target: "h".into(),
            sessions: vec![remote("default", true, 1)],
        };
        state.probing = Some("probe-2".into());
        apply(&mut state, answer("probe-1"), &mut dirty);
        assert_eq!(state.menu, None, "a stale answer opened the list");
        state.prompt = Some(Prompt {
            what: Naming::Space,
            text: String::new(),
        });
        apply(&mut state, answer("probe-2"), &mut dirty);
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
        let divider_col = (left + 1 + rect.cols) as u16;
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
        let divider_col = (left + 1 + rect.cols) as u16;
        assert!(!state.split_press(divider_col, state.lines - 1));
        assert!(!state.split_press(divider_col, state.lines));
        assert!(state.split_press(divider_col, (screen::TABS + 1) as u16));
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
        apply(&mut state, done("install-1"), &mut dirty);
        assert!(
            state
                .message
                .as_deref()
                .is_some_and(|m| m.contains("installed"))
        );
        assert_eq!(state.reprobe, None, "a stale install asked again");
        apply(&mut state, done("install-2"), &mut dirty);
        assert_eq!(state.reprobe.as_deref(), Some("h"));
        assert_eq!(state.installing, None);
    }
}
