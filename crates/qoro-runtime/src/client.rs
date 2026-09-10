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
pub use qoro_domain::{PtySize, Viewport};
use qoro_proto::{Channel, ClientHello, CodecOffers, Frame, Reassembler};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::daemon::{ClientWire, SnapshotWire};

/// Sent on attach. Reconciling unconditionally is cheaper than tracking what a previous
/// client may have left behind, and a previous client may not have exited cleanly at all.
const ATTACH_PREAMBLE: &[u8] = b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[?25h";
const RESTORE_SEQUENCE: &[u8] =
    b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[?25h\x1b[?1049l";

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

fn hello(client_id: &str) -> ClientHello {
    ClientHello {
        codec_generation_min: qoro_proto::CODEC_GENERATION,
        codec_generation_max: qoro_proto::CODEC_GENERATION,
        client_id: client_id.to_string(),
        client_name: "qoro".into(),
        client_version: env!("CARGO_PKG_VERSION").into(),
        codecs: CodecOffers {
            control: vec!["control.json.v1".into()],
            event: vec!["event.json.v1".into()],
            terminal: vec!["terminal.frame.v1".into()],
            blob: vec!["blob.raw.v1".into()],
        },
        extra: Default::default(),
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
    let mut stream = UnixStream::connect(socket).await?;
    let body = serde_json::to_vec(&hello(client_id))
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
            if serde_json::from_slice::<qoro_proto::ServerWelcome>(&frame.body).is_ok() {
                return Ok((stream, inbuf));
            }
            let refuse: qoro_proto::ServerRefuse = serde_json::from_slice(&frame.body)
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
    let (row, col) = snap.cursor;
    let local_row = (row as u16)
        .saturating_sub(view.row)
        .min(view.rows.saturating_sub(1));
    let local_col = (col as u16)
        .saturating_sub(view.col)
        .min(view.cols.saturating_sub(1));
    buf.extend_from_slice(format!("\x1b[{};{}H", local_row + 1, local_col + 1).as_bytes());
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
    let frames = qoro_proto::chunk(Channel::Terminal, stream, *seq, None, Bytes::from(body));
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
}

impl Connection {
    pub fn new() -> Self {
        Self::default()
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
                    if let Ok(snapshot) = serde_json::from_slice::<SnapshotWire>(&message.body) {
                        self.renders.received += 1;
                        out.push(Incoming::Render { terminal, snapshot });
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
    let frames = qoro_proto::chunk(Channel::Control, 0, *seq, None, Bytes::from(body));
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
            cols: 20,
            rows: lines.len(),
            cursor: (1, 3),
            lines: lines.iter().map(|s| s.to_string()).collect(),
            styles: Vec::new(),
        }
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
}
