//! Test fixtures for driving a real daemon over a real socket.
//!
//! A milestone 1 deliverable, named in the plan rather than discovered mid-milestone: five
//! of M1's gates require spawning the real thing and inspecting it from outside, and a
//! harness invented per test file is how those gates end up testing different things.
//!
//! Two rules the fixtures enforce rather than document. Every fixture gets its own runtime
//! directory, so the suite runs in parallel and never attaches to a developer's own daemon.
//! And every spawned pid is registered, so a leak is detectable across the whole run rather
//! than only inside the test that caused it.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use bytes::BytesMut;
use qoro_runtime::client;
use qoro_runtime::client::RenderStream;
use qoro_runtime::daemon::{ClientWire, SnapshotWire};
use qoro_runtime::{Daemon, paths};
use tokio::io::AsyncWriteExt;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

fn tracker() -> &'static Mutex<Vec<i32>> {
    static T: OnceLock<Mutex<Vec<i32>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(Vec::new()))
}

/// Register a pid the suite created, so a leak is visible suite-wide.
pub fn track_pid(pid: i32) {
    tracker().lock().unwrap().push(pid);
}

/// Pids the suite created that are still alive. A non-empty result at the end of a run is
/// a leak, whichever test caused it.
pub fn leaked_pids() -> Vec<i32> {
    tracker()
        .lock()
        .unwrap()
        .iter()
        .copied()
        // SAFETY: signal 0 only probes for existence.
        .filter(|p| unsafe { libc::kill(*p, 0) } == 0)
        .collect()
}

/// A daemon with its own private runtime directory.
/// Where a daemon started with `QORO_RUNTIME_DIR = dir` puts its socket.
///
/// One place, because six tests had written the join themselves and all six broke the day
/// a session became a directory of its own. A test that builds a product path by hand is
/// a test that stops testing the product.
pub fn socket_in(dir: &Path) -> std::path::PathBuf {
    paths::socket_path(&paths::session_dir(dir, paths::DEFAULT_SESSION))
}

/// Open terminal 1 in a daemon started as `qoro daemon`, which binds with none.
///
/// Blocking, for the harnesses that start the real binary from synchronous code. The
/// shell starts where the test runs, which is what the daemon's own first tab did.
pub fn open_first_terminal(socket: &Path) {
    if let Err(e) = try_open_first_terminal(socket) {
        panic!("{e}");
    }
}

/// The same, answering instead of panicking, for a harness that reports a failed
/// scenario rather than aborting the run. On a thread of its own, so it works from
/// inside a runtime as well as outside one.
pub fn try_open_first_terminal(socket: &Path) -> Result<(), String> {
    let socket = socket.to_path_buf();
    std::thread::spawn(move || open_first_terminal_now(&socket))
        .join()
        .map_err(|_| "the first terminal's thread panicked".to_string())?
}

fn open_first_terminal_now(socket: &Path) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let (stream, inbuf) = client::connect(socket, "test-first")
            .await
            .map_err(|e| format!("connect for the first terminal: {e}"))?;
        let mut c = TestClient::from_parts(stream, inbuf, 1);
        let here = std::env::current_dir().map_err(|e| e.to_string())?;
        let created = c
            .control(
                &qoro_runtime::control::ControlRequest::CreateTerminal {
                    id: "first".into(),
                    cwd: Some(here.display().to_string()),
                    workspace: None,
                    name: None,
                },
                Duration::from_secs(10),
            )
            .await;
        match created {
            Some(qoro_runtime::control::ControlResponse::TerminalCreated { .. }) => Ok(()),
            other => Err(format!("the first terminal was not created: {other:?}")),
        }
    })
}

pub struct TestDaemon {
    store: qoro_runtime::store::StoreHandle,
    workspace: i64,
    dir: tempfile::TempDir,
    socket: PathBuf,
    connections: std::sync::Arc<qoro_runtime::daemon::ConnectionRegistry>,
    terminals: std::sync::Arc<qoro_runtime::daemon::TerminalRegistry>,
    agents: std::sync::Arc<qoro_runtime::agents::AgentRegistry>,
    scanner: std::sync::Arc<qoro_runtime::daemon::ContainmentScanner>,
    warnings: std::sync::Arc<Mutex<Vec<qoro_runtime::daemon::Warning>>>,
}

/// Held across `Daemon::bind`, and the reason is a fork racing a file descriptor.
///
/// macOS has no `SOCK_CLOEXEC`, so creating a socket is `socket()` followed by a separate
/// `ioctl(FIOCLEX)`, and a `fork` in another thread between those two steps gives the child
/// a copy of the listening descriptor that `exec` will not close. Binding a daemon also
/// forks, because it starts a shell, so a test binary that binds several daemons at once
/// races itself: measured, `gates_m4` failed to rebind its own socket in six runs of twelve,
/// with the daemon reporting that something was still listening on a socket it had closed.
///
/// One process binds one daemon, so this is a property of the test binary rather than of
/// the product. The lock is held across the whole of `bind`, which is what makes the two
/// steps that race, the listener's creation and the first shell's fork, never overlap.
pub static BINDING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

impl TestDaemon {
    pub async fn start() -> Self {
        Self::start_with_shell(Path::new("/bin/sh")).await
    }

    pub async fn start_with_shell(shell: &Path) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        // `tempdir` creates 0755 and the daemon refuses anything group or other accessible.
        // Tightening here rather than loosening the check is the point.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
        paths::ensure_private_dir(dir.path()).expect("private dir");

        let daemon = {
            let _binding = BINDING.lock().await;
            let daemon = Daemon::bind(dir.path(), shell).await.expect("bind");
            // A bind opens nothing by itself now; the tests were written against a daemon
            // that always had terminal 1, and that is still the daemon they start.
            daemon.open_first_terminal().await.expect("first terminal");
            daemon
        };
        let socket = daemon.socket().to_path_buf();
        // Taken before `serve` consumes the daemon, so a gate can look at the same state
        // the admission policy uses rather than at a copy of it.
        let connections = daemon.connections();
        let terminals = daemon.terminals();
        let agents = daemon.agents();
        let scanner = daemon.scanner();
        let warnings = daemon.warnings();
        let store = daemon.store();
        let workspace = daemon.workspace();
        tokio::spawn(async move {
            let _ = daemon.serve().await;
        });
        Self {
            dir,
            socket,
            connections,
            terminals,
            agents,
            scanner,
            warnings,
            store,
            workspace,
        }
    }

    /// The durable metadata, so a gate can read what the daemon wrote down.
    pub fn store(&self) -> qoro_runtime::store::StoreHandle {
        self.store.clone()
    }

    /// The workspace this daemon records terminals in.
    pub fn workspace(&self) -> i64 {
        self.workspace
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// The daemon's live connection registry.
    pub fn connections(&self) -> &qoro_runtime::daemon::ConnectionRegistry {
        &self.connections
    }

    /// The daemon's terminal registry.
    pub fn terminals(&self) -> &qoro_runtime::daemon::TerminalRegistry {
        &self.terminals
    }

    fn connections_terminals(&self) -> Vec<u32> {
        self.terminals.streams()
    }

    /// Handles for tests that need to build a terminal themselves.
    pub fn terminals_arc(&self) -> &std::sync::Arc<qoro_runtime::daemon::TerminalRegistry> {
        &self.terminals
    }

    pub fn agents_arc(&self) -> std::sync::Arc<qoro_runtime::agents::AgentRegistry> {
        self.agents.clone()
    }

    pub fn scanner_arc(&self) -> std::sync::Arc<qoro_runtime::daemon::ContainmentScanner> {
        self.scanner.clone()
    }

    pub fn warnings_arc(&self) -> std::sync::Arc<Mutex<Vec<qoro_runtime::daemon::Warning>>> {
        self.warnings.clone()
    }

    /// Who the daemon believes is running in a terminal.
    pub fn agent(&self, stream: u32) -> Option<qoro_runtime::agents::TerminalAgent> {
        self.agents.agent(stream)
    }

    /// Warnings the daemon has recorded.
    pub fn warnings(&self) -> Vec<qoro_runtime::daemon::Warning> {
        self.warnings.lock().unwrap().clone()
    }

    pub async fn attach(&self, id: &str) -> TestClient {
        let (stream, inbuf) = client::connect(&self.socket, id).await.expect("connect");
        // The daemon subscribes a new connection to the lowest numbered terminal, so that
        // is what unqualified input addresses.
        let default_stream = self.connections_terminals().first().copied().unwrap_or(1);
        let (rd, wr) = stream.into_split();
        TestClient {
            rd,
            wr,
            inbuf,
            seqs: std::collections::BTreeMap::new(),
            default_stream,
            control_seq: 0,
            renders: RenderStream::new(),
            control_in: qoro_proto::Reassembler::new(),
            pending_control: Vec::new(),
            screens: std::collections::BTreeMap::new(),
            seen: std::collections::BTreeMap::new(),
            delivered: Vec::new(),
        }
    }
}

pub struct TestClient {
    rd: OwnedReadHalf,
    wr: OwnedWriteHalf,
    inbuf: BytesMut,
    seqs: std::collections::BTreeMap<u32, u32>,
    default_stream: u32,
    control_seq: u32,
    renders: RenderStream,
    control_in: qoro_proto::Reassembler,
    /// Control responses decoded while waiting for something else. Kept rather than
    /// dropped, because the alternative is a harness whose result depends on whether a
    /// render happened to arrive first.
    pending_control: Vec<qoro_runtime::control::ControlResponse>,
    /// The last snapshot per terminal, from the same decode loop the control path uses.
    screens: std::collections::BTreeMap<u32, SnapshotWire>,
    /// The text of every snapshot delivered per terminal, so "did this ever appear" is a
    /// question about what arrived rather than about what happened to arrive last.
    seen: std::collections::BTreeMap<u32, Vec<String>>,
    /// Every snapshot delivered, in order, across terminals.
    delivered: Vec<SnapshotWire>,
}

impl TestClient {
    /// Build a client over a connection the caller already made.
    ///
    /// For tests that drive a daemon they constructed themselves, which is how a restart is
    /// tested: two daemons over one directory, neither of them a `TestDaemon`.
    pub fn from_parts(
        stream: tokio::net::UnixStream,
        inbuf: BytesMut,
        default_stream: u32,
    ) -> TestClient {
        let (rd, wr) = stream.into_split();
        TestClient {
            rd,
            wr,
            inbuf,
            seqs: std::collections::BTreeMap::new(),
            default_stream,
            control_seq: 0,
            renders: RenderStream::new(),
            control_in: qoro_proto::Reassembler::new(),
            pending_control: Vec::new(),
            screens: std::collections::BTreeMap::new(),
            seen: std::collections::BTreeMap::new(),
            delivered: Vec::new(),
        }
    }

    pub async fn input(&mut self, bytes: &[u8]) {
        let stream = self.default_stream;
        self.input_to(stream, bytes).await;
    }

    /// Input addressed to one terminal. The frame's stream field is the address, which is
    /// what that field is for.
    pub async fn input_to(&mut self, stream: u32, bytes: &[u8]) {
        self.send_to(
            stream,
            &ClientWire::Input {
                bytes: bytes.to_vec(),
                from_terminal: None,
                from_token: None,
            },
        )
        .await;
    }

    /// Forget every snapshot seen so far.
    ///
    /// `wait_for_text_on` answers "did this ever arrive", which is the right question for a
    /// state that may be superseded before the next read. It is the wrong question for "is
    /// this true now": a test that asserts a size, changes something, and asserts the same
    /// size again passes on the first answer whatever happened in between. Calling this
    /// between the two makes the second assertion about the second state.
    pub fn forget_seen(&mut self) {
        self.seen.clear();
        self.delivered.clear();
    }

    /// Wait for text on one terminal's screen.
    pub async fn wait_for_text_on(&mut self, stream: u32, needle: &str, within: Duration) -> bool {
        use tokio::io::AsyncReadExt;
        let deadline = Instant::now() + within;
        let mut read = [0u8; 64 * 1024];
        loop {
            self.decode_buffered();
            if self
                .seen
                .get(&stream)
                .is_some_and(|texts| texts.iter().any(|t| t.contains(needle)))
            {
                return true;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            match tokio::time::timeout(remaining, self.rd.read(&mut read)).await {
                Ok(Ok(0)) | Err(_) => return false,
                Ok(Ok(n)) => {
                    self.inbuf.extend_from_slice(&read[..n]);
                    self.decode_buffered();
                    self.ack_if_due().await;
                }
                Ok(Err(_)) => return false,
            }
        }
    }

    pub async fn resize(&mut self, cols: u16, rows: u16) {
        self.send(&ClientWire::Resize { cols, rows }).await;
    }

    pub async fn send(&mut self, cmd: &ClientWire) {
        let stream = self.default_stream;
        self.send_to(stream, cmd).await;
    }

    pub async fn send_to(&mut self, stream: u32, cmd: &ClientWire) {
        // One sequence counter per stream: the reassembler requires consecutive numbers
        // within a sequence and reassembles streams independently, so a shared counter
        // would make one terminal's traffic depend on another's.
        let seq = self.seqs.entry(stream).or_default();
        let buf = client::encode_command_to(stream, cmd, seq).expect("encode");
        self.wr.write_all(&buf).await.expect("write");
    }

    /// Send a control request and wait for the response that echoes its id.
    ///
    /// Matching on the id rather than on arrival order is the point of having one: renders
    /// and other responses arrive in between, and a harness that took the next frame would
    /// be testing timing.
    pub async fn control(
        &mut self,
        request: &qoro_runtime::control::ControlRequest,
        within: Duration,
    ) -> Option<qoro_runtime::control::ControlResponse> {
        use tokio::io::AsyncReadExt;
        let want = request.request_id().to_string();
        let buf = client::encode_control_seq(request, &mut self.control_seq).expect("encode");
        self.wr.write_all(&buf).await.expect("write");

        let deadline = Instant::now() + within;
        let mut read = [0u8; 64 * 1024];
        loop {
            self.decode_buffered();
            if let Some(i) = self
                .pending_control
                .iter()
                .position(|r| r.request_id() == want)
            {
                return Some(self.pending_control.remove(i));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            match tokio::time::timeout(remaining, self.rd.read(&mut read)).await {
                Ok(Ok(0)) | Err(_) => return None,
                Ok(Ok(n)) => self.inbuf.extend_from_slice(&read[..n]),
                Ok(Err(_)) => return None,
            }
        }
    }

    /// The one decode loop. Everything that arrives is routed here, so nothing is dropped
    /// because it was not the thing the caller happened to be waiting for.
    fn decode_buffered(&mut self) {
        while let Some(frame) = qoro_proto::Frame::decode(&mut self.inbuf).expect("decode") {
            match frame.channel {
                qoro_proto::Channel::Control => {
                    if let Ok(Some(message)) = self.control_in.push(frame, Instant::now())
                        && let Ok(response) = serde_json::from_slice::<
                            qoro_runtime::control::ControlResponse,
                        >(&message.body)
                    {
                        self.pending_control.push(response);
                    }
                }
                qoro_proto::Channel::Terminal => {
                    let stream = frame.stream;
                    let mut one = BytesMut::new();
                    frame.encode(&mut one).expect("re-encode");
                    // Every snapshot, not the last one. A read can carry several, and a
                    // harness that keeps only the last reports "never appeared" for a
                    // state that really was delivered.
                    if let Ok(snapshots) = self.renders.take_all(&mut one) {
                        for snapshot in snapshots {
                            self.seen
                                .entry(stream)
                                .or_default()
                                .push(snapshot.lines.join("\n"));
                            self.delivered.push(snapshot.clone());
                            self.screens.insert(stream, snapshot);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// The terminal this client's unqualified input addresses.
    pub fn default_stream(&self) -> u32 {
        self.default_stream
    }

    /// The last screen seen for one terminal.
    pub fn screen(&self, stream: u32) -> Option<&SnapshotWire> {
        self.screens.get(&stream)
    }

    /// Wait for a screen satisfying `pred`. Returns `None` on timeout, so a test says what
    /// it expected rather than hanging.
    pub async fn wait_for(
        &mut self,
        pred: impl Fn(&SnapshotWire) -> bool,
        within: Duration,
    ) -> Option<SnapshotWire> {
        use tokio::io::AsyncReadExt;
        let deadline = Instant::now() + within;
        let mut buf = [0u8; 64 * 1024];
        // Anything already buffered from the handshake counts: the first snapshot often
        // arrives in the same read as the welcome.
        if let Some(s) = self.drain_buffered(&pred) {
            return Some(s);
        }
        while Instant::now() < deadline {
            // Acknowledged as it reads and on a quiet tick, the way the real client does: a
            // wait longer than the daemon's no-acknowledgement deadline otherwise ends with
            // a disconnect.
            self.ack_if_due().await;
            let remaining = deadline.saturating_duration_since(Instant::now());
            let tick = remaining.min(client::ACK_INTERVAL);
            let n = match tokio::time::timeout(tick, self.rd.read(&mut buf)).await {
                Err(_) => continue,
                Ok(Ok(0)) | Ok(Err(_)) => break,
                Ok(Ok(n)) => n,
            };
            self.inbuf.extend_from_slice(&buf[..n]);
            if let Some(s) = self.drain_buffered(&pred) {
                self.ack_if_due().await;
                return Some(s);
            }
        }
        None
    }

    /// Decode through the same reassembly the real client uses, and keep the last snapshot
    /// that satisfies the predicate. A harness with its own decoder would be proving
    /// something about the harness.
    fn drain_buffered(&mut self, pred: &impl Fn(&SnapshotWire) -> bool) -> Option<SnapshotWire> {
        self.decode_buffered();
        self.delivered.iter().rev().find(|s| pred(s)).cloned()
    }

    /// Read whatever is available for a moment, without waiting for a particular screen.
    pub async fn drain_incoming(&mut self, within: Duration) {
        use tokio::io::AsyncReadExt;
        let mut buf = [0u8; 64 * 1024];
        if let Ok(Ok(n)) = tokio::time::timeout(within, self.rd.read(&mut buf)).await {
            self.inbuf.extend_from_slice(&buf[..n]);
            self.decode_buffered();
        }
    }

    /// Send an acknowledgement if one is due, the way the real client does.
    pub async fn ack_if_due(&mut self) {
        if let Some(buf) = self.renders.due_ack(Instant::now()) {
            let _ = self.wr.write_all(&buf).await;
        }
    }

    /// How many render messages this client has taken off the wire.
    pub fn renders_received(&self) -> u64 {
        self.renders.received()
    }

    /// Wait for text to appear anywhere on screen, at any point since the last
    /// `forget_seen`.
    ///
    /// "Did this ever arrive", not "is this true now". The distinction matters more than it
    /// looks: a test that asserts a state, changes something, and asserts the old state
    /// again passes on the first answer whatever happened in between, and so does a test
    /// whose marker is still sitting on the screen from earlier. For "is this true now",
    /// wait for a fresh marker and then read `screen`.
    pub async fn wait_for_text(&mut self, needle: &str, within: Duration) -> bool {
        let needle = needle.to_string();
        self.wait_for(move |s| s.lines.join("\n").contains(&needle), within)
            .await
            .is_some()
    }
}

/// Default patience for a real shell to respond. Generous on purpose: a tight bound turns a
/// loaded CI runner into a red build for a reason unrelated to the code.
pub const PATIENCE: Duration = Duration::from_secs(10);
