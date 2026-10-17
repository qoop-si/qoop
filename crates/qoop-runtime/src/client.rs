//! The client: raw mode, a socket, and a screen.
//!
//! The outer terminal is not ours, so the rules are asymmetric. On attach we send a full
//! mode reset rather than assuming it is clean, which is also what reconciles a previous
//! abrupt disconnect. On exit we restore through a guard that runs on normal exit, on panic
//! and on SIGTERM, SIGHUP and SIGINT.
//!
//! Stated plainly: SIGKILL cannot be caught, so a SIGKILLed client can leave the outer
//! terminal in raw mode. The guarantee is that the next attach reconciles it, not that the
//! outer terminal is always restored.

use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use bytes::{Bytes, BytesMut};
pub use qoop_domain::{PtySize, Viewport};
use qoop_proto::{Channel, ClientHello, CodecOffers, Frame, Reassembler};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::daemon::{ClientWire, CopyWire, ImageWire, SnapshotWire, StoredImage};

/// Sent on attach. Reconciling unconditionally is cheaper than tracking what a previous
/// client may have left behind, and a previous client may not have exited cleanly at all.
/// Focus reports (`1004`) and scheme reports (`2031`) are included: an interface that died
/// with them on leaves a terminal typing `ESC [ I` into the next program whenever the
/// window is clicked.
const ATTACH_PREAMBLE: &[u8] =
    b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[?1004l\x1b[?2031l\x1b[?25h";
const RESTORE_SEQUENCE: &[u8] = b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[?1004l\x1b[?2031l\x1b[?25h\x1b[?1049l";

/// Set by the interface when it told the terminal it takes drops (`OSC 72 ; t=a`), so that
/// leaving, by any road, tells it to stop: a terminal left taking drops for a program that
/// is gone keeps a file dragged in from pasting its path.
pub static DROPS_ASKED: AtomicBool = AtomicBool::new(false);
/// `OSC 72 ; t=A`: no longer taking drops.
pub const STOP_DROPS: &[u8] = b"\x1b]72;t=A\x1b\\";

static RAW_MODE_ACTIVE: AtomicBool = AtomicBool::new(false);
static ORIGINAL_TERMIOS: std::sync::Mutex<Option<libc::termios>> = std::sync::Mutex::new(None);

/// Restores the outer terminal. Held for the life of the session, and also invoked from the
/// signal path, because a client that is asked to quit must not leave a raw terminal behind.
pub struct TerminalGuard;

impl TerminalGuard {
    pub fn enter_raw() -> io::Result<Self> {
        let fd = io::stdin().as_raw_fd();
        // SAFETY: zeroed termios is a valid out-param; fd is a live descriptor.
        let mut original: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: as above.
        if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
            return Err(io::Error::last_os_error());
        }
        *ORIGINAL_TERMIOS.lock().unwrap() = Some(original);

        let mut raw = original;
        // SAFETY: cfmakeraw only mutates the struct we own.
        unsafe { libc::cfmakeraw(&mut raw) };
        // SAFETY: fd is live and raw is a fully initialised termios.
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            return Err(io::Error::last_os_error());
        }
        RAW_MODE_ACTIVE.store(true, Ordering::SeqCst);

        let mut out = io::stdout();
        out.write_all(ATTACH_PREAMBLE)?;
        out.flush()?;
        Ok(Self)
    }

    /// Idempotent, because it runs from a guard, from a signal handler's follow-up, and
    /// from a panic hook, and any of the three may be the one that gets there first.
    pub fn restore() {
        if !RAW_MODE_ACTIVE.swap(false, Ordering::SeqCst) {
            return;
        }
        if let Some(original) = *ORIGINAL_TERMIOS.lock().unwrap() {
            // SAFETY: stdin is live and original is a termios we read from it.
            unsafe { libc::tcsetattr(io::stdin().as_raw_fd(), libc::TCSANOW, &original) };
        }
        let mut out = io::stdout();
        if DROPS_ASKED.swap(false, Ordering::SeqCst) {
            let _ = out.write_all(STOP_DROPS);
        }
        let _ = out.write_all(RESTORE_SEQUENCE);
        let _ = out.flush();
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        Self::restore();
    }
}

/// Terminal size of the outer terminal, which is what a resize request reports.
pub fn outer_size() -> (u16, u16) {
    // SAFETY: zeroed winsize is a valid out-param.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: stdout is live and ws is a valid out-param.
    if unsafe { libc::ioctl(io::stdout().as_raw_fd(), libc::TIOCGWINSZ, &mut ws) } == 0
        && ws.ws_col > 0
        && ws.ws_row > 0
    {
        (ws.ws_col, ws.ws_row)
    } else {
        (80, 24)
    }
}

/// The hello key a client sets to say it only watches: a daemon with nothing awake does
/// not stay up for it. An older client never says it, so it counts as somebody there.
pub const OBSERVER: &str = "observer";

/// The hello key a client sets to cap how many terminal messages it has unacknowledged
/// (1 to 16). The daemon then holds renders, newest kept, until an ack: a client behind a
/// relay gets the newest screen instead of a queue of old ones. Absent means unpaced.
pub const RENDER_WINDOW: &str = "render_window";

/// The hello key a link sets to a random value it keeps across its reconnects (at most 64
/// bytes). The daemon ends the connections that link made before, which a dropped ssh can
/// leave open on the far side, holding the size of every terminal they showed (ADR-007).
pub const REPLACES: &str = "replaces";

/// The `client_name` the phone app says in its hello, which the bridge carries unchanged.
/// Only such a connection opens a preview tunnel or reads a checkout's changes.
pub const PHONE: &str = "qoro-mobile";

/// The hello key an interface sets to say it lends a microphone to a tab's program
/// (`microphone`). An older interface never says it, so a recording is refused at once
/// rather than left hearing nothing.
pub const MICROPHONE: &str = "microphone";

fn hello(client_id: &str, observer: bool, replaces: Option<&str>) -> ClientHello {
    let mut extra = serde_json::Map::new();
    if client_id == qoop_proto::INTERFACE_ID {
        extra.insert(MICROPHONE.into(), serde_json::Value::Bool(true));
    }
    if observer {
        extra.insert(OBSERVER.into(), serde_json::Value::Bool(true));
    }
    if let Some(key) = replaces {
        extra.insert(REPLACES.into(), serde_json::Value::String(key.into()));
    }
    ClientHello {
        codec_generation_min: qoop_proto::CODEC_GENERATION,
        codec_generation_max: qoop_proto::CODEC_GENERATION,
        client_id: client_id.to_string(),
        client_name: "qoop".into(),
        client_version: env!("CARGO_PKG_VERSION").into(),
        codecs: CodecOffers {
            control: vec!["control.json.v1".into()],
            event: vec!["event.json.v1".into()],
            terminal: vec!["terminal.frame.v1".into()],
            blob: vec!["blob.raw.v1".into()],
        },
        extra,
    }
}

/// Connect and complete the handshake.
///
/// Returns the leftover bytes along with the stream, and that is not a detail. One `read`
/// can deliver the welcome and the first snapshot together, and a version of this function
/// that dropped its own buffer silently threw the snapshot away: the client then showed a
/// blank screen until something else happened to change. It passed every unit test and
/// failed the first time a person ran it.
pub async fn connect(socket: &Path, client_id: &str) -> io::Result<(UnixStream, BytesMut)> {
    connect_as(socket, client_id, false).await
}

/// Connect as one that only watches (`agents --watch`, a phone bridge's watcher): the same
/// connection, but it does not keep a daemon with nothing awake from leaving.
pub async fn connect_observer(
    socket: &Path,
    client_id: &str,
) -> io::Result<(UnixStream, BytesMut)> {
    connect_as(socket, client_id, true).await
}

async fn connect_as(
    socket: &Path,
    client_id: &str,
    observer: bool,
) -> io::Result<(UnixStream, BytesMut)> {
    // Bounded: a daemon that accepts and never answers (wedged, or a stale socket held by
    // something else) otherwise hangs every caller, a hook inside an agent's turn included.
    // Here and not in `handshake`: over an ssh relay the first seconds may be a password.
    tokio::time::timeout(qoop_proto::reassembly::SEQUENCE_DEADLINE, async {
        let stream = UnixStream::connect(socket).await.map_err(not_running)?;
        let (stream, leftover, _) = handshake_as(stream, client_id, observer, None).await?;
        Ok((stream, leftover))
    })
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "the daemon did not answer the handshake",
        )
    })?
}

/// A missing or dead socket is a daemon that is not running, said as such: "No such file or
/// directory" names no fix. The kind is kept for callers that start a daemon on it.
fn not_running(e: io::Error) -> io::Error {
    match e.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => io::Error::new(
            e.kind(),
            "nothing is awake: `qoop` or `qoop wake` starts it",
        ),
        _ => e,
    }
}

/// The handshake over any byte stream: a local socket, or the stdin and stdout of an `ssh`
/// running `qoop relay` on another machine. The relay does not change the protocol, so
/// nothing after this point needs to know which one it is.
///
/// The welcome comes back with the stream: it names the version the daemon is running, which
/// over a relay can be older than the qoop on that machine's disk.
/// `replaces` is the key of the link this connection belongs to ([`REPLACES`]).
pub async fn handshake<S>(
    stream: S,
    client_id: &str,
    replaces: Option<&str>,
) -> io::Result<(S, BytesMut, qoop_proto::ServerWelcome)>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    handshake_as(stream, client_id, false, replaces).await
}

/// [`handshake`] as one that only watches, over any byte stream: the asker of a move that
/// must not count as a window of its own.
pub async fn handshake_observer<S>(
    stream: S,
    client_id: &str,
) -> io::Result<(S, BytesMut, qoop_proto::ServerWelcome)>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    handshake_as(stream, client_id, true, None).await
}

async fn handshake_as<S>(
    mut stream: S,
    client_id: &str,
    observer: bool,
    replaces: Option<&str>,
) -> io::Result<(S, BytesMut, qoop_proto::ServerWelcome)>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let body = serde_json::to_vec(&hello(client_id, observer, replaces))
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let mut buf = BytesMut::new();
    Frame::new(Channel::Handshake, 0, Bytes::from(body))
        .encode(&mut buf)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    stream.write_all(&buf).await?;

    let mut inbuf = BytesMut::new();
    let mut read = [0u8; 8192];
    loop {
        let n = stream.read(&mut read).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the daemon closed the connection during the handshake",
            ));
        }
        inbuf.extend_from_slice(&read[..n]);
        if let Some(frame) = Frame::decode(&mut inbuf)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?
        {
            // A refusal is a refusal: it names the category that failed, and proceeding
            // with a partially negotiated codec set would be worse than not connecting.
            if let Ok(welcome) = serde_json::from_slice::<qoop_proto::ServerWelcome>(&frame.body) {
                return Ok((stream, inbuf, welcome));
            }
            let refuse: qoop_proto::ServerRefuse = serde_json::from_slice(&frame.body)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            return Err(io::Error::other(format!(
                "daemon refused the connection: {} ({:?})",
                refuse.human_message, refuse.reason
            )));
        }
    }
}

/// Draw a snapshot. A full redraw per update is correct and cheap enough for milestone 1;
/// the frame contract is already cumulative, so deltas are an optimisation of the same
/// contract rather than a different one.
pub fn render(snap: &SnapshotWire, out: &mut impl Write) -> io::Result<()> {
    let whole = Viewport::new(
        PtySize {
            cols: snap.cols as u16,
            rows: snap.rows as u16,
        },
        0,
        0,
        snap.cols as u16,
        snap.rows as u16,
    );
    render_viewport(snap, whole, out)
}

/// Draw the part of the grid this client can show.
///
/// The terminal has one grid, at its owner's size, and a client that cannot show all of it
/// shows a window onto it (ADR-007). Drawing the whole grid into a smaller window instead
/// does not fail, it wraps, which turns one long line into several and makes the client's
/// idea of the screen quietly different from the terminal's.
pub fn render_viewport(
    snap: &SnapshotWire,
    view: Viewport,
    out: &mut impl Write,
) -> io::Result<()> {
    let lines = view.crop(snap.lines.iter().map(String::as_str));
    let mut buf = Vec::with_capacity(view.cols as usize * view.rows as usize + 64);
    // Home, then draw each line and clear to end of line, then clear below.
    buf.extend_from_slice(b"\x1b[H");
    for (i, line) in lines.iter().enumerate() {
        buf.extend_from_slice(line.as_bytes());
        buf.extend_from_slice(b"\x1b[K");
        if i + 1 < lines.len() {
            buf.extend_from_slice(b"\r\n");
        }
    }
    buf.extend_from_slice(b"\x1b[J");
    // Terminal coordinates are one based, and the cursor is placed where it falls inside
    // this window. A cursor outside the window is parked at the nearest edge rather than
    // drawn off screen, which the outer terminal would clamp anyway and less predictably.
    // Placed and shown only while the program wants it seen: a hidden cursor stays
    // hidden with nothing more said, so a child that redraws behind a hidden cursor
    // does not get a shown one painted over it.
    let (row, col) = snap.cursor;
    if !snap.modes.cursor_visible {
        buf.extend_from_slice(b"\x1b[?25l");
        out.write_all(&buf)?;
        return out.flush();
    }
    let local_row = (row as u16)
        .saturating_sub(view.row)
        .min(view.rows.saturating_sub(1));
    let local_col = (col as u16)
        .saturating_sub(view.col)
        .min(view.cols.saturating_sub(1));
    buf.extend_from_slice(format!("\x1b[{};{}H", local_row + 1, local_col + 1).as_bytes());
    // Shown, idempotently: no hide precedes it, so a shown cursor streaming
    // does not blink the way hide-plus-show per frame does.
    buf.extend_from_slice(b"\x1b[?25h");
    out.write_all(&buf)?;
    out.flush()
}

/// The window this client should show of a grid, following the cursor.
///
/// Recomputed per snapshot rather than held, because the grid's size is the owner's to
/// change and a window remembered across a resize is a window that no longer fits.
pub fn viewport_for(
    snap: &SnapshotWire,
    previous: Option<Viewport>,
    cols: u16,
    rows: u16,
) -> Viewport {
    let grid = PtySize {
        cols: snap.cols as u16,
        rows: snap.rows as u16,
    };
    let base = match previous {
        Some(v) => Viewport::new(grid, v.col, v.row, cols, rows),
        None => Viewport::new(grid, 0, 0, cols, rows),
    };
    base.follow(grid, snap.cursor.1 as u16, snap.cursor.0 as u16)
}

/// Encode a client command, chunked across frames when it does not fit in one.
///
/// A multi-megabyte paste is one logical command and many frames. The frame cap is small on
/// purpose: it bounds how long one message can monopolise the socket, so that terminal
/// traffic is not starved by a transfer. Chunking is how a large message lives with that
/// cap rather than an exception to it.
///
/// `seq` is the caller's per-stream counter and is advanced by the number of frames
/// produced. The reassembler requires `seq == last_seq + 1` within a sequence, so the
/// counter has to belong to the connection rather than to the call.
pub fn encode_command_seq(cmd: &ClientWire, seq: &mut u32) -> io::Result<BytesMut> {
    encode_command_to(1, cmd, seq)
}
/// The same, addressed to one terminal. The frame's stream field is the terminal.
pub fn encode_command_to(stream: u32, cmd: &ClientWire, seq: &mut u32) -> io::Result<BytesMut> {
    let body =
        serde_json::to_vec(cmd).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let frames = qoop_proto::chunk(Channel::Terminal, stream, *seq, None, Bytes::from(body));
    let mut buf = BytesMut::new();
    for frame in &frames {
        frame
            .encode(&mut buf)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    }
    *seq = seq.wrapping_add(frames.len() as u32);
    Ok(buf)
}

/// Encode a command on a fresh sequence, for a caller that sends exactly one.
pub fn encode_command(cmd: &ClientWire) -> io::Result<BytesMut> {
    let mut seq = 0;
    encode_command_seq(cmd, &mut seq)
}

/// One logical input message may be this large before it is split.
///
/// A multi-megabyte paste as one message inflates several times over on the wire
/// (bytes travel as a JSON number array) and trips the reassembly bound, so the
/// whole paste is dropped where a streamed one would have flowed. Below the split
/// the bytes travel as they always have.
pub const MAX_INPUT_MESSAGE: usize = 32 * 1024;

/// Past this size a piece is emitted mid-sequence rather than held for ground.
///
/// A well-formed envelope returns to ground constantly (base64 is ground bytes),
/// so only a truly unterminated sequence reaches the ceiling, and holding that
/// is worse than splitting it: the daemon's own chunker applies the same rule.
pub const MAX_INPUT_RUN: usize = 64 * 1024;

/// Split input into pieces that each fit one logical message, on escape-sequence
/// boundaries from the shared scanner, so the two directions cannot disagree
/// about what a sequence is.
///
/// Order and bytes are preserved exactly: concatenated, the pieces are the input.
/// A control byte (`ctrl-a`, `ctrl-q`) is decoded before this runs, so splitting
/// never creates or hides one.
pub fn split_input(input: &[u8]) -> Vec<&[u8]> {
    use qoop_domain::EscapeState;
    let mut out = Vec::new();
    let mut state = EscapeState::default();
    let mut start = 0usize;
    for (i, b) in input.iter().enumerate() {
        state = state.step(*b);
        let len = i + 1 - start;
        if (state.is_ground() && len >= MAX_INPUT_MESSAGE) || len >= MAX_INPUT_RUN {
            out.push(&input[start..=i]);
            start = i + 1;
        }
    }
    if start < input.len() {
        out.push(&input[start..]);
    }
    out
}

/// Read stdin on a dedicated thread and forward it.
///
/// stdin is a blocking descriptor we do not own and must not reconfigure beyond raw mode,
/// so it gets a thread rather than the reactor. This is the one place a thread is the
/// simpler answer.
pub fn spawn_stdin_reader(tx: tokio::sync::mpsc::Sender<Vec<u8>>) {
    std::thread::spawn(move || {
        let mut stdin = io::stdin();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    });
}

/// How many render messages may go unacknowledged, and for how long.
///
/// Gate G1.18: a client that never types must still say it is keeping up, because a daemon
/// with no signal from a silent client cannot tell it apart from a stalled one, and picking
/// an eviction victim by buffered size instead punishes the client with the biggest screen
/// rather than the one that stopped reading.
pub const ACK_EVERY: u64 = 16;
pub const ACK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// The client half of the render lane: reassembly, snapshot selection and acknowledgement.
///
/// It lives here rather than in each caller so the CLI and the test harness cannot drift
/// apart about what a render message is, which is how a harness ends up proving something
/// about itself.
pub struct RenderStream {
    reassembler: Reassembler,
    received: u64,
    acked: u64,
    last_ack: std::time::Instant,
}

impl Default for RenderStream {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderStream {
    pub fn new() -> Self {
        Self {
            reassembler: Reassembler::new(),
            received: 0,
            acked: 0,
            last_ack: std::time::Instant::now(),
        }
    }

    pub fn received(&self) -> u64 {
        self.received
    }

    /// Decode everything buffered and return the last snapshot in it.
    ///
    /// Every snapshot is cumulative, so drawing the intermediate ones in a burst is work
    /// nobody sees, and skipping them is what keeps a slow terminal from falling behind a
    /// fast one. They are still counted, because the count is what the acknowledgement
    /// reports.
    pub fn take_latest(&mut self, inbuf: &mut BytesMut) -> io::Result<Option<SnapshotWire>> {
        Ok(self.take_all(inbuf)?.pop())
    }

    /// Every snapshot in the buffer, in order.
    ///
    /// A renderer wants only the last one, which is what `take_latest` gives it. A test
    /// asking "did this ever appear" wants all of them: one read can carry several
    /// snapshots, and keeping only the last silently drops a state that really was
    /// delivered. That distinction is the difference between a flaky test and a real one.
    ///
    /// It does not make delivery lossless. The daemon's render lane coalesces per terminal
    /// as well, by design (ADR-013), so a state that was superseded before it was written
    /// never reaches this buffer at all.
    pub fn take_all(&mut self, inbuf: &mut BytesMut) -> io::Result<Vec<SnapshotWire>> {
        let mut all = Vec::new();
        while let Some(frame) = Frame::decode(inbuf)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?
        {
            if frame.channel != Channel::Terminal {
                continue;
            }
            let message = self
                .reassembler
                .push(frame, std::time::Instant::now())
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            let Some(message) = message else { continue };
            if let Ok(snapshot) = serde_json::from_slice::<SnapshotWire>(&message.body) {
                self.received += 1;
                all.push(snapshot);
            } else if serde_json::from_slice::<ImageWire>(&message.body).is_ok() {
                // A picture on a snapshot-only reader: skipped. Not counted either,
                // because the daemon counts only screens as sent; a count here ran the
                // acknowledgement ahead of what was sent and hid a reader that stopped.
            }
        }
        Ok(all)
    }

    /// The acknowledgement to send, if one is due.
    pub fn due_ack(&mut self, now: std::time::Instant) -> Option<BytesMut> {
        let outstanding = self.received - self.acked;
        if outstanding == 0 {
            return None;
        }
        if outstanding < ACK_EVERY && now.duration_since(self.last_ack) < ACK_INTERVAL {
            return None;
        }
        self.acked = self.received;
        self.last_ack = now;
        let mut frame = Frame::new(Channel::Terminal, 1, Bytes::new());
        frame.ack = Some(self.acked);
        let mut buf = BytesMut::new();
        frame.encode(&mut buf).ok()?;
        Some(buf)
    }
}

/// One thing the daemon said.
///
/// A client that draws an index needs both lanes: the renders it subscribes to and the
/// answers to the questions it asked. Decoding them in two places means two reassemblers
/// fed from one socket, and the one that is not currently reading silently drops the
/// other's frames, which is a bug that only appears when both lanes are busy.
#[derive(Debug)]
pub enum Incoming {
    Render {
        terminal: u32,
        snapshot: SnapshotWire,
    },
    /// A picture the terminal's child emitted. Separate from the render because it
    /// is not grid cells: the client draws it as an overlay or a placeholder.
    Image {
        terminal: u32,
        image: StoredImage,
    },
    /// Text the terminal's child asked to put on the clipboard (OSC 52). The client
    /// decides whether to: only the person looking at that terminal should get it.
    Copy {
        terminal: u32,
        text: String,
    },
    /// A render or a picture, undecoded, from a [`Connection::raw`].
    Raw {
        terminal: u32,
        body: Bytes,
    },
    Control(crate::control::ControlResponse),
    /// Something changed that nobody asked about. A client that ignores these still works,
    /// it just finds out later.
    Event(crate::control::EventMessage),
}

/// Both lanes of the connection, decoded together.
///
/// Two reassemblers and not one, because a chunk sequence is identified by its stream and
/// the control channel numbers its own from zero: a control message and a terminal message
/// could otherwise be spliced into each other.
#[derive(Default)]
pub struct Connection {
    renders: RenderStream,
    control: Reassembler,
    events: Reassembler,
    /// Hand terminal-channel messages on as they arrived instead of decoding them. For a
    /// relay of renders, which needs to know which terminal a message is for and nothing
    /// else: parsing a whole screen to write the same screen out again is the one cost
    /// such a relay would add to every frame.
    raw: bool,
}

impl Connection {
    pub fn new() -> Self {
        Self::default()
    }

    /// A connection whose renders and pictures arrive as [`Incoming::Raw`].
    pub fn raw() -> Self {
        Self {
            raw: true,
            ..Self::default()
        }
    }

    /// How many renders have arrived, which is what an acknowledgement reports.
    pub fn renders(&mut self) -> &mut RenderStream {
        &mut self.renders
    }

    pub fn drain(&mut self, inbuf: &mut BytesMut) -> io::Result<Vec<Incoming>> {
        let mut out = Vec::new();
        while let Some(frame) = Frame::decode(inbuf)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?
        {
            let channel = frame.channel;
            let terminal = frame.stream;
            let target = match channel {
                Channel::Terminal => &mut self.renders.reassembler,
                Channel::Control => &mut self.control,
                Channel::Event => &mut self.events,
                _ => continue,
            };
            let message = target
                .push(frame, std::time::Instant::now())
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            let Some(message) = message else { continue };
            match channel {
                Channel::Terminal => {
                    if self.raw {
                        // Counted like a decoded one: the acknowledgement reports messages
                        // taken off the lane, whatever the reader then does with them.
                        self.renders.received += 1;
                        out.push(Incoming::Raw {
                            terminal,
                            body: message.body,
                        });
                        continue;
                    }
                    if let Ok(snapshot) = serde_json::from_slice::<SnapshotWire>(&message.body) {
                        self.renders.received += 1;
                        out.push(Incoming::Render { terminal, snapshot });
                    } else if let Ok(wire) = serde_json::from_slice::<ImageWire>(&message.body) {
                        // An older daemon never sends this shape; a newer client reads
                        // it here, an older client fails the snapshot parse above and
                        // skips it. Hostile or truncated images decode to `None` and
                        // are skipped the same way. Not counted: the daemon counts only
                        // screens as sent, and a count on one side alone is a deficit
                        // an older client could never repay.
                        if let Some(mut image) = wire.decode() {
                            image.terminal = terminal;
                            out.push(Incoming::Image { terminal, image });
                        }
                    } else if let Ok(wire) = serde_json::from_slice::<CopyWire>(&message.body)
                        && wire.v == 1
                    {
                        // Not counted, like a picture.
                        out.push(Incoming::Copy {
                            terminal,
                            text: wire.copy,
                        });
                    }
                }
                Channel::Control => {
                    if let Ok(response) =
                        serde_json::from_slice::<crate::control::ControlResponse>(&message.body)
                    {
                        out.push(Incoming::Control(response));
                    }
                }
                Channel::Event => {
                    // An event this build does not know about is ignored rather than
                    // treated as a protocol error: ADR-010's rule is that a newer daemon
                    // may say more than an older client understands.
                    if let Ok(event) =
                        serde_json::from_slice::<crate::control::EventMessage>(&message.body)
                    {
                        out.push(Incoming::Event(event));
                    }
                }
                _ => {}
            }
        }
        Ok(out)
    }
}

/// Encode a control request, chunked like any other message.
///
/// The control channel carries requests a client is waiting on, so it gets its own stream
/// counter: interleaving it with the terminal channel's would make one lane's sequence
/// depend on the other's traffic.
pub fn encode_control_seq(
    request: &crate::control::ControlRequest,
    seq: &mut u32,
) -> io::Result<BytesMut> {
    let body =
        serde_json::to_vec(request).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let frames = qoop_proto::chunk(Channel::Control, 0, *seq, None, Bytes::from(body));
    let mut buf = BytesMut::new();
    for frame in &frames {
        frame
            .encode(&mut buf)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    }
    *seq = seq.wrapping_add(frames.len() as u32);
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(lines: &[&str]) -> SnapshotWire {
        SnapshotWire {
            modes: Default::default(),
            behind: 0,
            cols: 20,
            rows: lines.len(),
            cursor: (1, 3),
            lines: lines.iter().map(|s| s.to_string()).collect(),
            styles: Vec::new(),
            links: Vec::new(),
        }
    }

    /// An older daemon never sent the field, and its cursor was always shown:
    /// missing reads as visible, never as hidden.
    #[test]
    fn a_snapshot_without_cursor_visibility_reads_as_visible() {
        let wire: SnapshotWire =
            serde_json::from_str(r#"{"cols":20,"rows":1,"cursor":[1,3],"lines":["$"],"modes":{}}"#)
                .expect("an old snapshot stops parsing");
        assert!(wire.modes.cursor_visible);
        assert!(crate::daemon::ModesWire::default().cursor_visible);
    }

    /// The hiding direction round-trips: an explicit false survives the wire
    /// and still reads back as hidden.
    #[test]
    fn an_explicitly_hidden_cursor_survives_the_wire() {
        let mut hidden = snap(&["$"]);
        hidden.modes.cursor_visible = false;
        let body = serde_json::to_string(&hidden).expect("a snapshot serialises");
        assert!(
            body.contains("cursor_visible"),
            "hidden is the case worth paying bytes for: {body}"
        );
        let back: SnapshotWire = serde_json::from_str(&body).expect("and parses back");
        assert!(!back.modes.cursor_visible);
        hidden.modes.cursor_visible = true;
        let shown_body = serde_json::to_string(&hidden).expect("a snapshot serialises");
        assert!(
            !shown_body.contains("cursor_visible"),
            "shown stays the size it always was: {shown_body}"
        );
    }

    #[test]
    fn rendering_positions_the_cursor_one_based() {
        let mut out = Vec::new();
        render(&snap(&["a", "b"]), &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.contains("\x1b[2;4H"),
            "cursor sequence missing from {s:?}"
        );
    }

    /// Small input travels as the one message it always was.
    #[test]
    fn small_input_is_not_split() {
        assert_eq!(split_input(b"hello"), vec![b"hello".as_slice()]);
        assert!(split_input(b"").is_empty());
    }

    /// A large paste arrives as several bounded pieces that concatenate back to
    /// the input, with no piece crossing the hard ceiling.
    #[test]
    fn a_large_paste_splits_and_reassembles_exactly() {
        let mut input = Vec::new();
        for i in 0..2000 {
            input.extend_from_slice(format!("line {i:04} of pasted text\r\n").as_bytes());
        }
        // An image envelope in the middle, larger than one message: it must
        // survive the split whole in content, wherever the boundaries land.
        input.extend_from_slice(b"\x1b_Gf=100,a=T;");
        input.extend(vec![b'A'; MAX_INPUT_MESSAGE]);
        input.extend_from_slice(b"\x1b\\");
        input.extend_from_slice(b"trailing text");
        let pieces = split_input(&input);
        assert!(pieces.len() > 2, "only {} pieces", pieces.len());
        assert!(
            pieces.iter().all(|p| p.len() <= MAX_INPUT_RUN),
            "a piece crossed the ceiling"
        );
        let back: Vec<u8> = pieces.concat();
        assert_eq!(back, input);
    }

    /// A sequence boundary is preferred over a mid-sequence cut: when a sequence
    /// straddles the target size, the cut waits for its end.
    #[test]
    fn pieces_prefer_sequence_boundaries() {
        let mut input = vec![b'x'; MAX_INPUT_MESSAGE - 8];
        input.extend_from_slice(b"\x1b]0;title\x07");
        input.extend(vec![b'y'; 100]);
        let pieces = split_input(&input);
        assert_eq!(pieces.len(), 2);
        assert!(
            pieces[0].ends_with(b"\x07"),
            "cut inside the OSC: {:?}",
            &pieces[0][pieces[0].len().saturating_sub(20)..]
        );
        let back: Vec<u8> = pieces.concat();
        assert_eq!(back, input);
    }

    #[test]
    fn rendering_clears_to_end_of_line_so_a_shorter_line_does_not_leave_debris() {
        let mut out = Vec::new();
        render(&snap(&["short"]), &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.contains("short\x1b[K"),
            "no clear after the line in {s:?}"
        );
        assert!(s.contains("\x1b[J"), "no clear below in {s:?}");
    }

    #[test]
    fn rendering_starts_at_home_so_a_frame_is_absolute_not_relative() {
        // Relative positioning would accumulate drift across frames, and a dropped frame
        // is normal by design.
        let mut out = Vec::new();
        render(&snap(&["x"]), &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().starts_with("\x1b[H"));
    }

    #[test]
    fn a_command_round_trips_through_a_frame() {
        let cmd = ClientWire::Resize {
            cols: 132,
            rows: 43,
        };
        let mut buf = encode_command(&cmd).unwrap();
        let frame = Frame::decode(&mut buf).unwrap().unwrap();
        assert_eq!(frame.channel, Channel::Terminal);
        let back: ClientWire = serde_json::from_slice(&frame.body).unwrap();
        match back {
            ClientWire::Resize { cols, rows } => assert_eq!((cols, rows), (132, 43)),
            other => panic!("wrong command: {other:?}"),
        }
    }

    /// An input frame from before the sender claim decodes with no claim:
    /// old clients keep today's unattributed behavior, and a claimed input
    /// round-trips with its claim.
    #[test]
    fn an_input_without_a_sender_claim_decodes_unattributed() {
        let old: ClientWire =
            serde_json::from_str(r#"{"t":"input","bytes":[104,105]}"#).expect("old input decodes");
        match old {
            ClientWire::Input {
                bytes,
                from_terminal,
                from_token,
            } => {
                assert_eq!(bytes, b"hi");
                assert_eq!(from_terminal, None);
                assert_eq!(from_token, None);
            }
            other => panic!("wrong command: {other:?}"),
        }
        let claimed = ClientWire::Input {
            bytes: b"hi".to_vec(),
            from_terminal: Some(3),
            from_token: Some("tok".into()),
        };
        let back: ClientWire =
            serde_json::from_slice(&serde_json::to_vec(&claimed).expect("encode")).expect("decode");
        match back {
            ClientWire::Input {
                from_terminal,
                from_token,
                ..
            } => {
                assert_eq!(from_terminal, Some(3));
                assert_eq!(from_token.as_deref(), Some("tok"));
            }
            other => panic!("wrong command: {other:?}"),
        }
    }

    #[test]
    fn restore_is_idempotent() {
        // It runs from a Drop guard, from a signal follow-up and from a panic hook, and any
        // of the three may get there first.
        TerminalGuard::restore();
        TerminalGuard::restore();
    }

    #[test]
    fn the_attach_preamble_disables_every_mouse_mode_we_might_have_inherited() {
        // An abruptly dropped previous client leaves the outer
        // terminal emitting mouse sequences, and the fix is to reconcile on attach rather
        // than to rely on a clean exit that may never happen.
        for mode in ["1000", "1002", "1003", "1006"] {
            let seq = format!("\x1b[?{mode}l");
            assert!(
                String::from_utf8_lossy(ATTACH_PREAMBLE).contains(&seq),
                "the preamble does not disable mouse mode {mode}"
            );
        }
    }

    /// Focus and scheme reports go off with the mouse, on attach and on the way out, so a
    /// client that died with them on never leaves the next program receiving them.
    #[test]
    fn focus_and_scheme_reports_are_turned_off_on_attach_and_on_restore() {
        for seq in ["\x1b[?1004l", "\x1b[?2031l"] {
            assert!(String::from_utf8_lossy(ATTACH_PREAMBLE).contains(seq));
            assert!(String::from_utf8_lossy(RESTORE_SEQUENCE).contains(seq));
        }
    }

    /// An image on the wire decodes to an `Incoming::Image` carrying the terminal
    /// and the verbatim envelope, in order between snapshots.
    #[test]
    fn an_image_decodes_between_snapshots() {
        use crate::daemon::{ImageWire, StoredImage};
        let image = StoredImage {
            terminal: 0,
            id: 7,
            protocol: crate::daemon::ImageProtocol::Kitty,
            bytes: b"\x1b_Gf=100,a=T;Zm9v\x1b\\".to_vec(),
        };
        let snap_body = serde_json::to_vec(&snap(&["hi"])).expect("snapshot");
        let img_body = serde_json::to_vec(&ImageWire::from(&image)).expect("image");
        let mut inbuf = BytesMut::new();
        for (seq, body) in [snap_body, img_body].into_iter().enumerate() {
            for frame in
                qoop_proto::chunk(Channel::Terminal, 9, seq as u32, None, Bytes::from(body))
            {
                frame.encode(&mut inbuf).expect("encode");
            }
        }
        let mut conn = Connection::new();
        let got = conn.drain(&mut inbuf).expect("drain");
        assert_eq!(got.len(), 2);
        assert!(matches!(got[0], Incoming::Render { terminal: 9, .. }));
        match &got[1] {
            Incoming::Image {
                terminal: 9,
                image: back,
            } => {
                assert_eq!(back.id, 7);
                assert_eq!(back.bytes, image.bytes);
            }
            other => panic!("wrong message: {other:?}"),
        }
    }

    /// A clipboard write decodes to `Incoming::Copy` on a reader that knows it, and a
    /// snapshot-only reader skips it uncounted, the way it skips a picture.
    #[test]
    fn a_copy_decodes_for_a_new_reader_and_is_skipped_by_an_old_one() {
        let body = serde_json::to_vec(&CopyWire {
            v: 1,
            copy: "hi there".into(),
        })
        .expect("copy");
        assert!(serde_json::from_slice::<SnapshotWire>(&body).is_err());
        assert!(serde_json::from_slice::<ImageWire>(&body).is_err());
        let encode = || {
            let mut inbuf = BytesMut::new();
            for frame in qoop_proto::chunk(Channel::Terminal, 4, 0, None, Bytes::from(body.clone()))
            {
                frame.encode(&mut inbuf).expect("encode");
            }
            inbuf
        };
        let got = Connection::new().drain(&mut encode()).expect("drain");
        assert!(
            matches!(&got[..], [Incoming::Copy { terminal: 4, text }] if text == "hi there"),
            "{got:?}"
        );
        let mut old = RenderStream::new();
        assert!(old.take_all(&mut encode()).expect("take").is_empty());
        assert_eq!(old.received(), 0);
    }
}
