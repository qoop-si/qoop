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
    /// What each terminal's state was at the last index, so a change into one that wants a
    /// person can be noticed. Without it the interface would have to compare a row against
    /// itself, which is a comparison that is always equal.
    was: std::collections::BTreeMap<u32, String>,
    /// Set when something started needing a person, cleared when the bell has rung.
    ring: bool,
}

impl State {
    fn frame(&self) -> Frame<'_> {
        Frame {
            rows: &self.rows,
            focused: self.focused,
            snapshot: self.snapshot.as_ref(),
            cols: self.cols,
            lines: self.lines,
            message: self.message.as_deref(),
        }
    }

    /// The terminal after the focused one, wrapping. The index is the order the daemon
    /// reports, so this is the order a person sees.
    fn neighbour(&self, forward: bool) -> Option<u32> {
        if self.rows.is_empty() {
            return None;
        }
        let at = self
            .rows
            .iter()
            .position(|r| r.terminal == self.focused)
            .unwrap_or(0);
        let n = self.rows.len();
        let next = if forward {
            (at + 1) % n
        } else {
            (at + n - 1) % n
        };
        Some(self.rows[next].terminal)
    }
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
        was: std::collections::BTreeMap::new(),
        ring: false,
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
    // The alternate screen, so that detaching gives the user their scrollback back.
    out.write_all(b"\x1b[?1049h")?;
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
                                let focused = state.focused;
                                wr.write_all(&client::encode_command_to(
                                    focused,
                                    &ClientWire::Input { bytes },
                                    seqs.entry(focused).or_default(),
                                )?).await?;
                            }
                            Action::Detach => return Ok(()),
                            Action::Redraw => dirty = true,
                            Action::Next | Action::Previous => {
                                let forward = matches!(action, Action::Next);
                                if let Some(next) = state.neighbour(forward)
                                    && next != state.focused
                                {
                                    wr.write_all(&switch(
                                        state.focused, next, &mut control_seq,
                                    )?).await?;
                                    state.focused = next;
                                    state.snapshot = None;
                                    state.message = None;
                                    let (pane_cols, pane_rows) =
                                        screen::pane_size(state.cols, state.lines);
                                    wr.write_all(&client::encode_command_to(
                                        next,
                                        &ClientWire::Resize {
                                            cols: pane_cols,
                                            rows: pane_rows,
                                        },
                                        seqs.entry(next).or_default(),
                                    )?).await?;
                                    dirty = true;
                                }
                            }
                            Action::Create => {
                                request += 1;
                                wr.write_all(&client::encode_control_seq(
                                    &ControlRequest::CreateTerminal {
                                        id: format!("new-{request}"),
                                        cwd: None,
                                    },
                                    &mut control_seq,
                                )?).await?;
                                state.message = Some("starting a terminal".to_string());
                                dirty = true;
                            }
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
                                if terminal == state.focused || state.focused == 0 {
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
                                );
                                apply(&mut state, response, &mut dirty);
                            }
                        }
                    }
                    if let Some(ack) = connection.renders().due_ack(std::time::Instant::now()) {
                        wr.write_all(&ack).await?;
                    }
                    if let Some(next) = state.pending_focus.take()
                        && next != state.focused
                    {
                        wr.write_all(&switch(state.focused, next, &mut control_seq)?)
                            .await?;
                        state.focused = next;
                        state.snapshot = None;
                        let (pane_cols, pane_rows) = screen::pane_size(state.cols, state.lines);
                        wr.write_all(&client::encode_command_to(
                            next,
                            &ClientWire::Resize { cols: pane_cols, rows: pane_rows },
                            seqs.entry(next).or_default(),
                        )?).await?;
                        dirty = true;
                    }
                    if refresh {
                        request += 1;
                        wr.write_all(&client::encode_control_seq(
                            &ControlRequest::ListAgents { id: format!("index-{request}") },
                            &mut control_seq,
                        )?).await?;
                    }
                }
            }
        }
    }
    .await;

    let _ = out.write_all(b"\x1b[?1049l");
    let _ = out.flush();
    outcome
}

/// Stop watching one terminal and start watching another, in one write.
///
/// Both halves together, because a client that subscribes before it unsubscribes is a
/// client that receives renders for a terminal nobody is looking at, and hidden output not
/// costing anything is the whole point of the subscription.
fn switch(from: u32, to: u32, control_seq: &mut u32) -> io::Result<BytesMut> {
    let mut out = client::encode_control_seq(
        &ControlRequest::Unsubscribe {
            id: format!("hide-{from}"),
            terminals: vec![from],
        },
        control_seq,
    )?;
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
        ControlResponse::Agents { agents, .. } => {
            if state.focused == 0 {
                state.focused = agents.first().map(|r| r.terminal).unwrap_or(0);
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
                state.focused = state
                    .rows
                    .iter()
                    .map(|r| r.terminal)
                    .find(|t| *t != terminal)
                    .unwrap_or(0);
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
