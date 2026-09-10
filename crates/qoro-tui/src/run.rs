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
use crate::screen::{self, Frame};
use crate::{sidebar, tabs};

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
    /// Spaces asked for and not yet answered for.
    ///
    /// Only the naming needs this, and the naming needs it badly: two presses of `ctrl-a w`
    /// before either answer came back both counted the same list and both produced
    /// `space 2`, so the panel showed two rows with one name on them.
    creating: usize,
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
    fn clicked(&self, row: u16, col: u16) -> Option<Clicked> {
        let (_, pane_rows) = screen::pane_size(self.cols, self.lines);
        if (row as usize) <= screen::TABS {
            return tabs::hit(
                &self.tabs(),
                self.focused,
                self.cols as usize,
                (col as usize).saturating_sub(1),
            )
            .map(Clicked::Terminal);
        }
        if col as usize > sidebar::WIDTH {
            // The pane. A terminal application that wants the mouse asks for it, and
            // passing a click to one that did not would type its report at a prompt.
            return None;
        }
        let within = (row as usize) - 1 - screen::TABS;
        match sidebar::hit(
            &self.spaces,
            &self.rows,
            self.space.unwrap_or_default(),
            self.focused,
            pane_rows as usize,
            within,
        )? {
            sidebar::Hit::Space(space) => Some(Clicked::Space(space)),
            sidebar::Hit::Terminal(terminal) => Some(Clicked::Terminal(terminal)),
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

/// What the interface drew where a person clicked.
enum Clicked {
    Space(i64),
    Terminal(u32),
}

/// Run the interface until the user detaches or the daemon goes away.
pub async fn run(socket: &Path) -> io::Result<()> {
    let (stream, leftover) = client::connect(socket, "qoro-tui").await?;
    let guard = TerminalGuard::enter_raw()?;
    // A panic must not leave the outer terminal raw either, which is the one failure a
    // person cannot recover from without knowing that `qoro reset` exists.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        TerminalGuard::restore();
        previous_hook(info);
    }));

    let outcome = drive(stream, leftover).await;

    drop(guard);
    outcome
}

async fn drive(stream: tokio::net::UnixStream, leftover: BytesMut) -> io::Result<()> {
    let (mut rd, mut wr) = stream.into_split();
    let (cols, lines) = client::outer_size();
    let mut state = State {
        rows: Vec::new(),
        focused: 0,
        snapshot: None,
        cols,
        lines,
        message: Some("connecting".to_string()),
        pending_focus: None,
        spaces: Vec::new(),
        space: None,
        was: std::collections::BTreeMap::new(),
        ring: false,
        wants_terminal_in: Vec::new(),
        adopted: false,
        creating: 0,
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
    // `1000` is press and release only, not motion: tracking every movement would be a
    // report per pixel row for a feature that only needs to know where a press landed.
    // `1006` asks for those reports in the extended form, because the original one cannot
    // express a column past 223 and this interface is regularly wider than that.
    out.write_all(b"\x1b[?1049h\x1b[?1000h\x1b[?1006h")?;
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
                    dirty = true;
                }
                typed = stdin_rx.recv() => {
                    let Some(bytes) = typed else { return Ok(()) };
                    for action in keys.feed(&bytes) {
                        match action {
                            Action::Input(bytes) => {
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
                            Action::Click { row, col } => {
                                match state.clicked(row, col) {
                                    Some(Clicked::Terminal(picked)) => {
                                        if let Some(bytes) = focus(
                                            &mut state, picked, &mut control_seq, &mut seqs,
                                        )? {
                                            wr.write_all(&bytes).await?;
                                            dirty = true;
                                        }
                                    }
                                    Some(Clicked::Space(space)) => {
                                        if let Some(bytes) = enter_space(
                                            &mut state, space, &mut control_seq, &mut seqs,
                                        )? {
                                            wr.write_all(&bytes).await?;
                                        }
                                        dirty = true;
                                    }
                                    None => {}
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
                            Action::CreateSpace => {
                                request += 1;
                                wr.write_all(&client::encode_control_seq(
                                    &ControlRequest::CreateWorkspace {
                                        id: format!("space-{request}"),
                                        // Numbered rather than asked for. A prompt is a
                                        // second input mode this interface does not have
                                        // yet, and a space with a name a person did not
                                        // choose is still a space they can work in.
                                        name: format!(
                                            "space {}",
                                            state.spaces.len() + state.creating + 1
                                        ),
                                    },
                                    &mut control_seq,
                                )?).await?;
                                state.creating += 1;
                                state.message = Some("starting a space".to_string());
                                dirty = true;
                            }
                            Action::Create => {
                                request += 1;
                                wr.write_all(&client::encode_control_seq(
                                    &ControlRequest::CreateTerminal {
                                        id: format!("new-{request}"),
                                        cwd: None,
                                        // The space the person is looking at.
                                        workspace: state.space,
                                    },
                                    &mut control_seq,
                                )?).await?;
                                state.message = Some("starting a terminal".to_string());
                                dirty = true;
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
                    if refresh {
                        request += 1;
                        wr.write_all(&client::encode_control_seq(
                            &ControlRequest::ListAgents { id: format!("index-{request}") },
                            &mut control_seq,
                        )?).await?;
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
    let _ = out.write_all(b"\x1b[?1006l\x1b[?1000l\x1b[?1049l");
    let _ = out.flush();
    outcome
}

/// Move the keyboard to another terminal: unsubscribe, subscribe, and size it to the pane,
/// all in one write.
///
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
