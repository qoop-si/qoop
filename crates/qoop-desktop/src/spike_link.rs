//! The Phase 0 window's connection to the daemon (`--features bench`), on a thread of its own:
//! this machine only, no mux. The product window links through `link.rs`.
//!
//! Reading the socket, reassembling frames and parsing every screen's JSON happen here,
//! never on the window's thread. A screen goes into one slot per terminal, the newest
//! replacing an older one the window has not taken yet, so memory stays at one screen per
//! pane and a busy terminal costs the window only the screens it draws. The daemon hears
//! that this client keeps up only once the window has taken what arrived.
//!
//! When the daemon goes away (`qoop update` restarts it) the link connects again, shows the
//! same terminals and tells the window, which keeps the last screens drawn faded meanwhile.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::BytesMut;
use qoop_runtime::client::{self, Connection, Incoming};
use qoop_runtime::control::{ControlRequest, ControlResponse, EventMessage, TerminalStream};
use qoop_runtime::daemon::{ClientWire, SnapshotWire};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// The newest screen per terminal that the window has not taken yet.
pub type Latest = Arc<Mutex<HashMap<TerminalStream, SnapshotWire>>>;

/// What the window asks of the daemon.
#[derive(Debug)]
pub enum Up {
    Input(TerminalStream, Vec<u8>),
    Resize(TerminalStream, u16, u16),
    /// Receive renders for these again (the window became visible).
    Show(Vec<TerminalStream>),
    /// Stop the renders for these (the window is hidden: nothing would be presented).
    Hide(Vec<TerminalStream>),
    /// A pane shows another terminal now (`qoop focus` named one no pane showed).
    Swap {
        from: Option<TerminalStream>,
        to: TerminalStream,
    },
    /// The window took the screens out of `Latest`.
    Taken,
}

/// What the daemon told the window.
#[derive(Debug)]
pub enum Down {
    /// The terminals the panes show, in pane order, after every (re)connect.
    Terminals(Vec<TerminalStream>),
    /// New screens are waiting in `Latest`.
    Screens,
    /// These terminals closed, or the daemon refused to show them.
    Gone(Vec<TerminalStream>),
    /// `qoop focus` asked for this terminal.
    Focus(TerminalStream),
    /// The connection ended; the link is trying again.
    Disconnected(String),
}

/// Start the link: connect as the person's interface (`qoro-tui`), make sure `want`
/// terminals exist and show them, and keep doing so across daemon restarts.
pub fn start(
    socket: PathBuf,
    want: usize,
    host: ([u8; 3], [u8; 3]),
) -> (UnboundedSender<Up>, UnboundedReceiver<Down>, Latest) {
    let (up_tx, up_rx) = unbounded_channel();
    let (down_tx, down_rx) = unbounded_channel();
    let latest = Latest::default();
    let slot = latest.clone();
    // This window has no microphone to lend; a recording in a tab must not be told it has
    // one (the TUI keeps the default and lends its own).
    client::set_lends_microphone(false);
    std::thread::Builder::new()
        .name("qoop-link".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let Ok(runtime) = runtime else {
                let _ = down_tx.send(Down::Disconnected("no async runtime".into()));
                return;
            };
            runtime.block_on(keep_linked(socket, want, host, up_rx, down_tx, slot));
        })
        .expect("spawn the link thread");
    (up_tx, down_rx, latest)
}

/// One connection after another until the window goes: a backoff from 100 ms doubling to
/// 2 s between tries, back to 100 ms once one connects.
async fn keep_linked(
    socket: PathBuf,
    want: usize,
    host: ([u8; 3], [u8; 3]),
    mut up: UnboundedReceiver<Up>,
    down: UnboundedSender<Down>,
    latest: Latest,
) {
    let mut shown: Vec<TerminalStream> = Vec::new();
    let mut backoff = Duration::from_millis(100);
    loop {
        let mut connected = false;
        match session(
            &socket,
            want,
            host,
            &mut shown,
            &mut up,
            &down,
            &latest,
            &mut connected,
        )
        .await
        {
            Ok(()) => return,
            Err(e) => {
                if down.send(Down::Disconnected(e.to_string())).is_err() {
                    return;
                }
            }
        }
        if connected {
            backoff = Duration::from_millis(100);
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(2));
    }
}

struct Wire {
    rd: OwnedReadHalf,
    wr: OwnedWriteHalf,
    inbuf: BytesMut,
    conn: Connection,
    control_seq: u32,
    terminal_seqs: HashMap<TerminalStream, u32>,
}

/// What one read brought besides screens.
#[derive(Default)]
struct Brought {
    answers: Vec<ControlResponse>,
    events: Vec<EventMessage>,
}

impl Wire {
    /// What one read brought: screens go to the slot, the rest to the caller.
    fn take(
        &mut self,
        read: usize,
        down: &UnboundedSender<Down>,
        latest: &Latest,
    ) -> io::Result<Brought> {
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the daemon closed the connection",
            ));
        }
        let mut brought = Brought::default();
        for incoming in self.conn.drain(&mut self.inbuf)? {
            match incoming {
                Incoming::Render {
                    terminal, snapshot, ..
                } => {
                    let was_empty = {
                        let mut slot = latest.lock().unwrap_or_else(|e| e.into_inner());
                        let empty = slot.is_empty();
                        slot.insert(terminal, snapshot);
                        empty
                    };
                    if was_empty {
                        let _ = down.send(Down::Screens);
                    }
                }
                Incoming::Control(answer) => brought.answers.push(answer),
                Incoming::Event(event) => brought.events.push(event),
                _ => {}
            }
        }
        Ok(brought)
    }

    /// Say this client has taken everything that arrived. Sent once the window has, so a
    /// window that stalls reads as one that stalled.
    async fn ack(&mut self) -> io::Result<()> {
        if let Some(ack) = self.conn.renders().due_ack(Instant::now()) {
            self.wr.write_all(&ack).await?;
        }
        Ok(())
    }

    async fn request(&mut self, request: &ControlRequest) -> io::Result<()> {
        let frame = client::encode_control_seq(request, &mut self.control_seq)?;
        self.wr.write_all(&frame).await
    }

    /// Send a request and wait for the daemon's answer, keeping screens meanwhile. Only at
    /// the start of a connection, while no other request is out.
    async fn ask(
        &mut self,
        request: &ControlRequest,
        down: &UnboundedSender<Down>,
        latest: &Latest,
    ) -> io::Result<ControlResponse> {
        self.request(request).await?;
        loop {
            let n = self.rd.read_buf(&mut self.inbuf).await?;
            if let Some(answer) = self.take(n, down, latest)?.answers.into_iter().next() {
                return Ok(answer);
            }
        }
    }

    async fn send(&mut self, terminal: TerminalStream, wire: &ClientWire) -> io::Result<()> {
        let seq = self.terminal_seqs.entry(terminal).or_default();
        let frame = client::encode_command_to(terminal, wire, seq)?;
        self.wr.write_all(&frame).await
    }

    async fn subscribe(&mut self, terminals: Vec<TerminalStream>, on: bool) -> io::Result<()> {
        let request = if on {
            ControlRequest::Subscribe {
                id: "show".into(),
                terminals,
            }
        } else {
            ControlRequest::Unsubscribe {
                id: "hide".into(),
                terminals,
            }
        };
        self.request(&request).await
    }
}

/// A request id the daemon has never seen. It keeps the answer to an id for 30 days and
/// answers a repeat with the first answer, so a reused id would bring back a terminal made
/// long ago.
fn fresh_id(what: &str) -> String {
    static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("desktop-{what}-{}-{nanos}-{n}", std::process::id())
}

/// The terminals to show: those shown before that still exist, then other existing ones,
/// lowest first, then new ones.
fn choose(
    shown: &[TerminalStream],
    existing: &[TerminalStream],
    want: usize,
) -> Vec<TerminalStream> {
    let mut chosen: Vec<TerminalStream> = shown
        .iter()
        .copied()
        .filter(|t| existing.contains(t))
        .collect();
    let mut rest: Vec<TerminalStream> = existing
        .iter()
        .copied()
        .filter(|t| !chosen.contains(t))
        .collect();
    rest.sort_unstable();
    chosen.extend(rest);
    chosen.truncate(want);
    chosen
}

#[allow(clippy::too_many_arguments)]
async fn session(
    socket: &std::path::Path,
    want: usize,
    host: ([u8; 3], [u8; 3]),
    shown: &mut Vec<TerminalStream>,
    up: &mut UnboundedReceiver<Up>,
    down: &UnboundedSender<Down>,
    latest: &Latest,
    connected: &mut bool,
) -> io::Result<()> {
    let (stream, inbuf) = client::connect(socket, qoop_proto::INTERFACE_ID).await?;
    *connected = true;
    let (rd, wr) = stream.into_split();
    let mut wire = Wire {
        rd,
        wr,
        inbuf,
        conn: Connection::new().paced(),
        control_seq: 0,
        terminal_seqs: HashMap::new(),
    };
    let existing = match wire
        .ask(
            &ControlRequest::ListTerminals {
                id: fresh_id("list"),
            },
            down,
            latest,
        )
        .await?
    {
        ControlResponse::Terminals { terminals, .. } => terminals,
        other => return Err(io::Error::other(format!("listing terminals: {other:?}"))),
    };
    *shown = choose(shown, &existing, want);
    while shown.len() < want {
        let request = ControlRequest::CreateTerminal {
            id: fresh_id("new"),
            cwd: None,
            workspace: None,
            name: None,
        };
        match wire.ask(&request, down, latest).await? {
            ControlResponse::TerminalCreated { terminal, .. } => shown.push(terminal),
            other => return Err(io::Error::other(format!("making a terminal: {other:?}"))),
        }
    }
    let _ = down.send(Down::Terminals(shown.clone()));
    // Shown, or refused (asleep, gone): a refused one is a pane with nothing in it.
    match wire
        .ask(
            &ControlRequest::Subscribe {
                id: fresh_id("show"),
                terminals: shown.clone(),
            },
            down,
            latest,
        )
        .await?
    {
        ControlResponse::Subscribed { terminals, .. } => {
            let refused: Vec<TerminalStream> = shown
                .iter()
                .copied()
                .filter(|t| !terminals.contains(t))
                .collect();
            if !refused.is_empty() {
                shown.retain(|t| !refused.contains(t));
                let _ = down.send(Down::Gone(refused));
            }
        }
        other => return Err(io::Error::other(format!("showing terminals: {other:?}"))),
    }
    // What a program inside asks for with `OSC 10 ?` and `OSC 11 ?`: this window's colours.
    // An older daemon refuses the request, which changes nothing.
    wire.request(&ControlRequest::HostColors {
        id: fresh_id("colors"),
        fg: host.0,
        bg: host.1,
    })
    .await?;

    loop {
        tokio::select! {
            // Only the read itself races the other arm: it loses nothing when cancelled,
            // and a write cut in half by a cancel would corrupt the stream.
            read = wire.rd.read_buf(&mut wire.inbuf) => {
                let brought = wire.take(read?, down, latest)?;
                for answer in brought.answers {
                    if let ControlResponse::Terminals { terminals, .. } = answer {
                        let gone: Vec<TerminalStream> = shown.iter().copied().filter(|t| !terminals.contains(t)).collect();
                        if !gone.is_empty() {
                            shown.retain(|t| !gone.contains(t));
                            let _ = down.send(Down::Gone(gone));
                        }
                    }
                }
                for event in brought.events {
                    match event {
                        // Something about the terminals changed, or the daemon dropped news
                        // for this client: list them again, as the TUI does.
                        EventMessage::AgentChanged { .. } | EventMessage::Resync => {
                            wire.request(&ControlRequest::ListTerminals { id: fresh_id("list") }).await?;
                        }
                        EventMessage::Focus { terminal } => {
                            let _ = down.send(Down::Focus(terminal));
                        }
                        _ => {}
                    }
                }
            }
            asked = up.recv() => match asked {
                None => return Ok(()),
                Some(Up::Input(terminal, bytes)) => {
                    for piece in client::split_input(&bytes) {
                        let input = ClientWire::Input {
                            bytes: piece.to_vec(),
                            from_terminal: None,
                            from_token: None,
                        };
                        wire.send(terminal, &input).await?;
                    }
                }
                Some(Up::Resize(terminal, cols, rows)) => {
                    wire.send(terminal, &ClientWire::Resize { cols, rows }).await?;
                }
                Some(Up::Show(terminals)) => wire.subscribe(terminals, true).await?,
                Some(Up::Hide(terminals)) => wire.subscribe(terminals, false).await?,
                Some(Up::Swap { from, to }) => {
                    // In place, so the pane order survives a reconnect.
                    match shown.iter().position(|t| Some(*t) == from) {
                        Some(i) => shown[i] = to,
                        None => shown.push(to),
                    }
                    if let Some(from) = from {
                        wire.subscribe(vec![from], false).await?;
                    }
                    wire.subscribe(vec![to], true).await?;
                }
                Some(Up::Taken) => wire.ack().await?,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reconnect_keeps_the_terminals_that_still_exist() {
        assert_eq!(choose(&[5, 2], &[1, 2, 3, 5], 4), vec![5, 2, 1, 3]);
        assert_eq!(choose(&[5, 2], &[2, 9], 4), vec![2, 9]);
        assert_eq!(choose(&[], &[4, 1, 3], 2), vec![1, 3]);
    }

    #[test]
    fn request_ids_are_never_reused() {
        assert_ne!(fresh_id("new"), fresh_id("new"));
    }
}
