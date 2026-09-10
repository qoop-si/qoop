//! The daemon: one socket, one terminal for now, real PTYs.
//!
//! Milestone 1 is one terminal on purpose. Everything that makes many terminals work
//! (workspaces, per-client focus, layout) is milestone 2, and adding it before the single
//! terminal is boringly reliable is how a runtime ends up unreliable in a way that features
//! cannot fix.

use std::collections::BTreeMap;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bytes::{Bytes, BytesMut};
use qoro_domain::{
    ClientId, OwnershipEvent, PtySize, SizePolicy, SizeRequest, SizeVerdict, TerminalId,
};
use qoro_proto::{Channel, Frame, Reassembler, ServerCapabilities, negotiate};
use qoro_pty::sys;
use qoro_pty::teardown::{self, Containment, Step};
use qoro_pty::writeq::{WritePriority, WriteQueue};
use qoro_terminal::{AlacrittyEngine, Snapshot, TerminalEngine};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncReadExt, AsyncWriteExt, Interest};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, watch};

use crate::control::{ControlRequest, ControlResponse};
use crate::paths;
use crate::terminal::{PARSE_SLICE, ReadOutcome, classify_read, replies};

/// A message from a client to the terminal.
///
/// Every command carries who sent it. Resize is arbitrated per client (ADR-007), and a
/// command type that could not say who asked would make that arbitration impossible to
/// enforce at the only place it matters.
#[derive(Clone, Debug)]
pub enum ClientCommand {
    Input {
        from: ClientId,
        bytes: Bytes,
    },
    Resize {
        from: ClientId,
        writable: bool,
        cols: u16,
        rows: u16,
    },
    /// A client's connection ended. Ownership moves, because a dead owner would otherwise
    /// strand the geometry at its last size.
    Detached {
        client: ClientId,
    },
    /// End this terminal. The terminal runs its own teardown, because containment is the
    /// terminal's property and nobody else holds the set.
    Close,
    /// The owner has agreed to hand the geometry to this client. The only path that moves
    /// ownership other than the owner's connection ending (ADR-007).
    GrantOwnership {
        from: ClientId,
        to: ClientId,
    },
}

/// What the daemon publishes to attached clients.
#[derive(Clone, Debug)]
pub enum TerminalUpdate {
    /// The whole visible screen. Cumulative by construction, which is what makes dropping
    /// a stale one safe (ADR-013). Deltas are a milestone 2 optimisation of the same
    /// contract, not a different one.
    Full(Snapshot),
    /// The terminal has ended, carrying the last screen it had.
    ///
    /// The snapshot travels with the end rather than before it because the channel is
    /// latest wins: a separate `Full` followed by a bare `Gone` lets the end overwrite the
    /// final screen before any reader sees it, and the child's last output is exactly what
    /// teardown works to preserve. Measured as a regression the moment the render lane
    /// started coalescing.
    Ended(Snapshot),
}

/// ADR-004's accepted-connection budget, and the admission queue behind it.
///
/// Gate G1.10 attaches 32 clients at once, so the budget has to be above that with room to
/// spare or the gate and the budget contradict each other.
pub const MAX_CONNECTIONS: usize = 64;

/// How often a connection checks the things that are not driven by an arriving frame.
///
/// Two of them: expiring reassembly buffers whose deadline passed while the peer was
/// silent, and noticing a peer that stopped acknowledging. Both were previously evaluated
/// only when a frame arrived, which is exactly never for the peer that has gone quiet.
pub const SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// ADR-013: a client that has not acknowledged for this long, while something is
/// outstanding, is disconnected. Independent of the connection count, because a wedged
/// reader below the admission limit was previously never noticed at all.
pub const NO_ACK_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// ADR-004's ceiling on outbound bytes buffered across every connection at once.
///
/// A per-connection bound is not enough on its own: sixty three connections each holding a
/// single one megabyte snapshot for a 1000 by 1000 terminal is 190 MB, three times this
/// budget, with the admission limit never reached and so the eviction policy never run.
pub const MAX_OUTBOUND_BYTES: usize = 64 * 1024 * 1024;

/// How many parse slices run before the terminal task gives the runtime a turn.
///
/// The slice is sized by the engine's staging ring, at half its capacity, which is far
/// smaller than a parse turn should be. Sixteen slices is about 8 KiB, measured at roughly
/// 350 microseconds, and still breaks a 64 KiB read into several turns rather than one.
///
/// Re-measured once the latency gate stopped timing an interactive shell's prompt, because
/// the comment defending this value had cited that measurement. Twenty terminals streaming
/// a megabyte a second each, keystroke to echo at the ninety fifth percentile, against the
/// cost of one terminal streaming the same rate:
///
/// | slices per turn | p95 | one streaming terminal |
/// |---|---|---|
/// | 1 | 1.1 ms | 0.95 cores |
/// | 4 | 2.5 to 3.1 ms | 0.75 cores |
/// | 16 | 2.9 to 3.4 ms | 0.74 cores |
///
/// So yielding after every slice does buy latency, which the old comment denied, and it is
/// not bought here: the budget is 8 milliseconds and sixteen slices meets it with more than
/// twice the margin, while a quarter more CPU per streaming terminal is the difference
/// between twelve and sixteen of them on this machine.
pub const SLICES_PER_TURN: usize = 16;

/// How long the history writer waits before draining, so that a burst becomes one batch.
pub const HISTORY_BATCH_WINDOW: std::time::Duration = std::time::Duration::from_millis(20);

/// How often buffered history reaches the operating system. A crash loses at most this
/// much scrollback, which is a cheaper promise than a syscall per batch.
pub const HISTORY_FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// What every live connection has been sent and has acknowledged, plus the handle that can
/// end it.
///
/// The registry exists so that overload has an honest answer to "who is not reading".
/// Choosing by buffered size answers a different question and picks the client with the
/// biggest terminal.
#[derive(Default)]
pub struct ConnectionRegistry {
    inner: std::sync::Mutex<Vec<Connection>>,
}

pub struct Connection {
    pub client: ClientId,
    pub progress: Arc<Progress>,
    /// Asking the connection to end, rather than aborting its task.
    ///
    /// Aborting was the first version and it leaked everything: the tasks a connection
    /// spawns are detached, so aborting the parent left the writer running and holding the
    /// only write half, and the socket stayed open. The registry then said 63 while the
    /// process held 64 sockets. Measured: an evicted peer went on reading renders.
    shutdown: Arc<tokio::sync::Notify>,
    admitted: std::time::Instant,
}

impl ConnectionRegistry {
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Sent and acknowledged counts per live connection, for gates and for diagnostics.
    pub fn progress(&self) -> Vec<(ClientId, u64, u64)> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .map(|c| {
                (
                    c.client.clone(),
                    c.progress.sent.load(Ordering::Relaxed),
                    c.progress.acked.load(Ordering::Relaxed),
                )
            })
            .collect()
    }

    fn register(&self, connection: Connection) {
        self.inner.lock().unwrap().push(connection);
    }

    fn remove(&self, client: &ClientId) {
        self.inner.lock().unwrap().retain(|c| &c.client != client);
    }

    /// Ask the connection that is furthest behind to end, and say which one that was.
    ///
    /// The rank is how far behind the connection is, and only then how long it has been
    /// behind. A lifetime average was the first version and it inverted: a desktop client
    /// attached for an hour to a quiet workspace, fully caught up, sorted below a wedged
    /// reader that had acknowledged one attach snapshot three seconds ago and stopped.
    /// ADR-004 names that inversion as the thing not to do.
    ///
    /// A connection with nothing outstanding is never a victim while one with something
    /// outstanding exists, because "not behind" is not a rate at all.
    fn evict_worst(&self, now: std::time::Instant) -> Option<ClientId> {
        let mut inner = self.inner.lock().unwrap();
        let (index, _) = inner
            .iter()
            .enumerate()
            .max_by_key(|(_, c)| (c.progress.outstanding(), c.behind_for(now).as_millis()))?;
        let victim = inner.remove(index);
        // `notify_one` and not `notify_waiters`: the read loop creates its `Notified` future
        // inside the select, so there is a window each iteration with no waiter registered,
        // and `notify_waiters` sent into that window is simply lost. `notify_one` stores a
        // permit, so the signal survives until the loop comes back around.
        victim.shutdown.notify_one();
        Some(victim.client)
    }

    /// Connections that have not acknowledged anything for longer than the deadline while
    /// having something outstanding. Independent of the connection count on purpose: a
    /// wedged reader below the admission limit was previously never noticed at all.
    fn wedged(&self, now: std::time::Instant, deadline: std::time::Duration) -> Vec<ClientId> {
        let inner = self.inner.lock().unwrap();
        inner
            .iter()
            .filter(|c| c.progress.outstanding() > 0 && c.behind_for(now) > deadline)
            .map(|c| c.client.clone())
            .collect()
    }

    /// End one connection by name.
    ///
    /// The one path both eviction and the no-acknowledgement sweep go through, and the one
    /// an operator command would use. Public so that a test can assert the mechanism
    /// without having to arrange sixty four connections and then guess which one the policy
    /// will pick.
    pub fn disconnect(&self, client: &ClientId) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(i) = inner.iter().position(|c| &c.client == client) {
            let victim = inner.remove(i);
            // `notify_one` and not `notify_waiters`: the read loop creates its `Notified` future
            // inside the select, so there is a window each iteration with no waiter registered,
            // and `notify_waiters` sent into that window is simply lost. `notify_one` stores a
            // permit, so the signal survives until the loop comes back around.
            victim.shutdown.notify_one();
        }
    }
}

impl Connection {
    /// How long this connection has had something outstanding.
    fn behind_for(&self, now: std::time::Instant) -> std::time::Duration {
        let since = self
            .progress
            .last_ack
            .lock()
            .unwrap()
            .unwrap_or(self.admitted);
        now.saturating_duration_since(since)
    }
}

/// A live terminal, as seen by everything that is not the terminal task.
///
/// Cloning one is how a connection subscribes, so a subscription costs a `watch::Receiver`
/// and not a task. The runtime state (the pty, the engine, the write queue) is owned by the
/// terminal task and is deliberately not reachable from here.
#[derive(Clone)]
pub struct TerminalHandle {
    /// The workspace this terminal was created in. Held here rather than looked up in the
    /// store on every index, because an index is drawn far more often than a terminal is
    /// created.
    pub workspace: i64,
    pub id: TerminalId,
    /// The token this terminal's child was launched with. Held so that a later observation
    /// can be checked against it without asking the child.
    pub token: qoro_domain::CapabilityToken,
    pub commands: mpsc::Sender<ClientCommand>,
    pub updates: watch::Sender<TerminalUpdate>,
}

/// The daemon's terminals, addressed by the number that travels in a frame's `stream`
/// field. Stream 0 is reserved and is never a terminal, so a decoder that defaults a
/// missing field to zero fails loudly instead of writing into the first terminal.
#[derive(Default)]
pub struct TerminalRegistry {
    inner: std::sync::Mutex<BTreeMap<u32, TerminalHandle>>,
    next: AtomicU64,
}

impl TerminalRegistry {
    pub fn get(&self, stream: u32) -> Option<TerminalHandle> {
        self.inner.lock().unwrap().get(&stream).cloned()
    }

    /// The workspace a terminal belongs to, for an index that groups by one.
    pub fn workspace_of(&self, stream: u32) -> Option<i64> {
        self.inner.lock().unwrap().get(&stream).map(|h| h.workspace)
    }

    pub fn streams(&self) -> Vec<u32> {
        self.inner.lock().unwrap().keys().copied().collect()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Mint the next stream number. Monotonic and never reused within a run, so a client
    /// holding a stale id addresses nothing rather than addressing somebody else's
    /// terminal.
    fn mint(&self) -> u32 {
        self.next.fetch_add(1, Ordering::Relaxed) as u32 + 1
    }

    fn insert(&self, stream: u32, handle: TerminalHandle) {
        self.inner.lock().unwrap().insert(stream, handle);
    }

    fn remove(&self, stream: u32) -> Option<TerminalHandle> {
        self.inner.lock().unwrap().remove(&stream)
    }
}

pub struct Daemon {
    listener: UnixListener,
    socket: PathBuf,
    /// Remembered so that a terminal created on request starts the same program as the one
    /// created at bind. A daemon that took the shell from the requesting client would be
    /// letting a client choose what the daemon executes, which is ADR-015's whole subject.
    shell: PathBuf,
    terminals: Arc<TerminalRegistry>,
    /// Who is running in each terminal, and the token that admits a claim about it.
    agents: Arc<crate::agents::AgentRegistry>,
    /// The durable log. Commands are in it before they are acknowledged; observations are
    /// in it when the disk allows, and the loss is recorded (ADR-005, ADR-017).
    events: crate::eventlog::EventLog,
    connections: Arc<ConnectionRegistry>,
    warnings: Arc<std::sync::Mutex<Vec<Warning>>>,
    /// Outbound bytes buffered across every connection, against ADR-004's ceiling.
    outbound: Arc<std::sync::atomic::AtomicUsize>,
    store: crate::store::StoreHandle,
    /// Where terminal history segments live, one directory per terminal beneath it.
    history_dir: Option<PathBuf>,
    scanner: Arc<ContainmentScanner>,
    /// The workspace new terminals are recorded in. One per daemon for now: workspace
    /// switching is a client surface and a later milestone, but the row has to exist from
    /// the first terminal or the metadata has nowhere to hang.
    workspace: i64,
}

impl Daemon {
    /// Bind, spawn the terminal, and start serving. The caller must already hold the
    /// startup lock (ADR-012), which is what makes the stale-socket removal below safe.
    pub async fn bind(dir: &Path, shell: &Path) -> io::Result<Self> {
        let socket = paths::socket_path(dir);
        remove_stale_socket(&socket)?;
        let listener = UnixListener::bind(&socket)?;
        // Restrict before anyone can connect. A umask race would otherwise leave a window
        // in which the socket exists with default permissions.
        std::fs::set_permissions(&socket, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;

        let warnings: Arc<std::sync::Mutex<Vec<Warning>>> = Arc::default();

        // The database lives beside the socket, in the same private directory, so its
        // permissions are the ones ADR-012 already checks.
        let store = crate::store::StoreHandle::new(
            qoro_store::Store::open(&dir.join("qoro.db")).map_err(io::Error::other)?,
        );
        let workspace = match store.open_workspace().await {
            Ok(id) => id,
            Err(e) => return Err(io::Error::other(e)),
        };

        let terminals = Arc::new(TerminalRegistry::default());
        // One terminal at bind, which is the whole of milestone 1's surface and the default
        // a client with no arguments attaches to.
        // Whether this is a restart is decided before the file is opened, because opening
        // it creates it: a first start would otherwise record a gap for an interval that
        // never existed.
        let log_path = dir.join("events.db");
        let restarting = log_path.exists();
        let last_seen = std::fs::metadata(&log_path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let events = crate::eventlog::EventLog::new(
            qoro_events::EventStore::open(&log_path).map_err(io::Error::other)?,
        );
        // A gap covering the interval nobody watched. The daemon cannot know what it
        // missed, and a reader that assumed continuity across a restart would be wrong
        // exactly when it matters most (ADR-005).
        if restarting {
            let _ = events.note_restart_gap(last_seen).await;
        }
        let agents = Arc::new(crate::agents::AgentRegistry::new()?);
        let scanner = Arc::new(ContainmentScanner::default());
        // One pass before the first terminal exists, so a terminal created immediately has
        // a set to absorb rather than an empty map.
        scanner.refresh().await;
        open_terminal(
            &terminals,
            workspace,
            shell,
            None,
            warnings.clone(),
            Some(dir.join("history")),
            scanner.clone(),
            agents.clone(),
            events.clone(),
        )?;

        Ok(Self {
            listener,
            socket,
            shell: shell.to_path_buf(),
            terminals,
            connections: Arc::new(ConnectionRegistry::default()),
            warnings,
            outbound: Arc::default(),
            store,
            workspace,
            history_dir: Some(dir.join("history")),
            scanner,
            agents,
            events,
        })
    }

    /// The terminal registry. Taken before `serve` consumes the daemon.
    pub fn terminals(&self) -> Arc<TerminalRegistry> {
        self.terminals.clone()
    }

    /// The live connection registry. Taken before `serve` consumes the daemon, which is
    /// what lets a gate look at the same state the eviction policy uses.
    pub fn connections(&self) -> Arc<ConnectionRegistry> {
        self.connections.clone()
    }

    /// The shared containment scanner.
    pub fn scanner(&self) -> Arc<ContainmentScanner> {
        self.scanner.clone()
    }

    /// The durable log.
    pub fn events(&self) -> crate::eventlog::EventLog {
        self.events.clone()
    }

    /// Who is running in each terminal.
    pub fn agents(&self) -> Arc<crate::agents::AgentRegistry> {
        self.agents.clone()
    }

    /// Warnings the daemon has recorded.
    pub fn warnings(&self) -> Arc<std::sync::Mutex<Vec<Warning>>> {
        self.warnings.clone()
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub async fn serve(self) -> io::Result<()> {
        // The wedged-reader sweep. It runs whatever the connection count is, because a peer
        // that stopped reading is a problem at one connection as much as at sixty four, and
        // eviction only ever ran at the limit.
        let watched = self.connections.clone();
        let outbound = self.outbound.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(SWEEP_INTERVAL).await;
                let now = std::time::Instant::now();

                // The global ceiling. Enforced here rather than only at admission, because
                // the connection that pushes the daemon over it is usually one that is
                // already attached and has stopped reading.
                while outbound.load(Ordering::Relaxed) > MAX_OUTBOUND_BYTES {
                    match watched.evict_worst(now) {
                        Some(evicted) => eprintln!(
                            "qorod: disconnecting {} to stay within the outbound budget",
                            evicted.as_str()
                        ),
                        None => break,
                    }
                }

                for client in watched.wedged(now, NO_ACK_DEADLINE) {
                    eprintln!(
                        "qorod: disconnecting {} after {NO_ACK_DEADLINE:?} with no acknowledgement",
                        client.as_str()
                    );
                    watched.disconnect(&client);
                }
            }
        });

        // The containment scan, once for the whole daemon. It only runs while at least one
        // terminal exists, so an idle daemon makes no syscalls at all, which is what gate
        // G2.2 asserts: the cost of being idle cannot depend on how many unrelated
        // processes the host happens to be running.
        let scanning = self.scanner.clone();
        let scanned_terminals = self.terminals.clone();
        tokio::spawn(async move {
            loop {
                // Parked until something happens, then rate limited. Nothing changed, so
                // nothing runs: a scan is a pass over the whole process table, and running
                // it on a timer regardless of activity made a hundred idle terminals cost
                // 1.09 seconds of CPU a minute against a 50 millisecond budget. A process
                // cannot join a terminal without that terminal saying something first.
                scanning.wait_for_activity().await;
                tokio::time::sleep(CONTAINMENT_SCAN_INTERVAL).await;
                if scanned_terminals.is_empty() {
                    continue;
                }
                scanning.refresh().await;
            }
        });

        // Process changes. On macOS the kqueue descriptor is itself pollable, so this is a
        // subscription and costs nothing while nothing happens. On Linux there is no
        // unprivileged way to be told about an `exec`, so the same task runs ADR-009's
        // adaptive probe: fast while something is changing, backing off to a floor.
        let watched_agents = self.agents.clone();
        let settle_queue = Arc::new(SettleQueue::default());
        tokio::spawn(settle_loop(self.agents.clone(), settle_queue.clone()));
        tokio::spawn(async move {
            if qoro_pty::procwatch::is_subscription() {
                let fd = watched_agents.raw_fd();
                let Ok(afd) = AsyncFd::with_interest(WatchFd(fd), Interest::READABLE) else {
                    eprintln!("qorod: cannot watch for process changes");
                    return;
                };
                loop {
                    let Ok(mut ready) = afd.readable().await else {
                        return;
                    };
                    let touched = watched_agents.apply_events();
                    ready.clear_ready();
                    settle_queue.mark(touched);
                }
            } else {
                let mut interval = qoro_pty::procwatch::PROBE_FAST;
                loop {
                    tokio::time::sleep(interval).await;
                    let touched = watched_agents.apply_events();
                    let changed = !touched.is_empty();
                    // The probe platform re-reads every terminal, because it has no event
                    // to tell it which one moved.
                    settle_queue.mark(watched_agents.streams());
                    interval = qoro_pty::procwatch::next_probe(interval, changed);
                }
            }
        });

        let mut next_client = 0u64;
        loop {
            let (stream, _) = self.listener.accept().await?;
            next_client += 1;
            // Per connection, never per client program: ownership must end when the
            // connection does, and a stable identity would survive a crash and strand the
            // geometry (ADR-007).
            let client = ClientId::new(format!("c{next_client}"));
            let terminals = self.terminals.clone();
            let shell = self.shell.clone();
            let warnings = self.warnings.clone();

            // Admission before service. At the budget, the connection making the least
            // progress leaves so that the new one can be served, rather than the new one
            // being refused while a wedged reader keeps its slot forever.
            if self.connections.len() >= MAX_CONNECTIONS
                && let Some(evicted) = self.connections.evict_worst(std::time::Instant::now())
            {
                eprintln!(
                    "qorod: evicted {} to admit {}",
                    evicted.as_str(),
                    client.as_str()
                );
            }

            let progress = Arc::new(Progress::default());
            let registry = self.connections.clone();
            let registered = client.clone();
            let served_progress = progress.clone();
            let shutdown = Arc::new(tokio::sync::Notify::new());
            let served_shutdown = shutdown.clone();
            let served_outbound = self.outbound.clone();
            let served_store = self.store.clone();
            let workspace = self.workspace;
            let served_history = self.history_dir.clone();
            let served_scanner = self.scanner.clone();
            let served_agents = self.agents.clone();
            let served_events = self.events.clone();
            tokio::spawn(async move {
                // Milestone 1 has one attach mode and it is writable. Read-only observers
                // are a milestone 2 surface; the arbitration below already refuses them.
                let result = serve_client(
                    stream,
                    registered.clone(),
                    true,
                    ServeContext {
                        terminals,
                        shell,
                        warnings,
                        progress: served_progress,
                        shutdown: served_shutdown,
                        outbound: served_outbound,
                        store: served_store,
                        workspace,
                        history_dir: served_history,
                        scanner: served_scanner,
                        agents: served_agents,
                        events: served_events,
                    },
                )
                .await;
                if let Err(e) = result {
                    eprintln!("qorod: client ended: {e}");
                }
                registry.remove(&registered);
            });
            self.connections.register(Connection {
                client,
                progress,
                shutdown,
                admitted: std::time::Instant::now(),
            });
        }
    }
}

/// Remove a socket file only when nothing is listening on it, and only when it is ours.
fn remove_stale_socket(path: &Path) -> io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            "a qoro daemon is already listening on this socket",
        )),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
            ) =>
        {
            std::fs::remove_file(path)
        }
        Err(e) => Err(e),
    }
}

/// The peer's uid, checked before any protocol byte is read.
/// Whether a peer uid is ours. Its own function so the rule can be asserted where a second
/// uid cannot be created, which is everywhere that is not root.
pub fn peer_is_ours(uid: u32) -> bool {
    uid == paths::current_uid()
}

fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let fd = stream.as_raw_fd();
    #[cfg(target_os = "macos")]
    {
        let mut uid: libc::uid_t = 0;
        let mut gid: libc::gid_t = 0;
        // SAFETY: fd is a live connected socket and both out-params are valid.
        if unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(uid)
    }
    #[cfg(not(target_os = "macos"))]
    {
        // SAFETY: zeroed ucred is a valid out-param and len matches its size.
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: fd is a live connected socket; cred and len are valid out-params.
        let rc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                &mut cred as *mut _ as *mut libc::c_void,
                &mut len,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(cred.uid)
    }
}

/// Something the daemon noticed that a client would otherwise experience as silence.
///
/// Milestone 1 has no event channel yet (that is ADR-017 and milestone 4), so warnings are
/// recorded here and read by the gates. What matters now is that the daemon does not report
/// success for something the operating system discarded; where the warning is delivered is
/// a separate decision from whether it exists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Warning {
    /// A paste with no line terminator went to a child in canonical mode. The line
    /// discipline discards everything past its limit while every write returns success, so
    /// "it arrived" is not something we can honestly report.
    CanonicalPasteWithoutNewline { bytes: usize, limit: usize },
}

/// The tty line discipline's canonical input limit. POSIX puts the floor at 255 and every
/// system in scope uses 1024; the exact number matters less than that the check exists,
/// because past it the discipline discards rather than blocks.
pub const MAX_CANON: usize = 1024;

/// One connection's pending renders, one per terminal, latest wins.
///
/// This is ADR-013's rule made structural rather than remembered. A plain queue would let a
/// terminal that renders faster than the client drains push every other terminal's frames
/// out of the way, and a queue with no coalescing would let it grow without bound. Keyed by
/// terminal, the memory is one snapshot per subscribed terminal and the loss is always the
/// intermediate value of the terminal that caused it.
pub struct RenderQueue {
    pending: std::sync::Mutex<BTreeMap<u32, Snapshot>>,
    put_at: std::sync::Mutex<Option<std::time::Instant>>,
    notify: tokio::sync::Notify,
    closed: std::sync::atomic::AtomicBool,
    /// This queue's share of the global outbound total, so that adding and removing keep
    /// the shared counter honest without walking the map.
    bytes: std::sync::atomic::AtomicUsize,
    /// Every connection's queue adds to the same counter, which is what makes ADR-004's
    /// ceiling a property of the daemon rather than of one connection.
    global: Arc<std::sync::atomic::AtomicUsize>,
}

/// Whether to print the internal latency breakdown.
///
/// Read once. It is checked on the busiest paths in the daemon, and an environment lookup
/// there is its own measurement error.
fn trace_latency() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("QORO_TRACE_LATENCY").is_ok())
}

/// Roughly what a snapshot costs to hold. The line text dominates; the exact figure does
/// not matter, only that it grows with the terminal and is charged while it is held.
fn snapshot_bytes(snapshot: &Snapshot) -> usize {
    snapshot.lines.iter().map(String::len).sum::<usize>() + 64
}

impl RenderQueue {
    pub fn new(global: Arc<std::sync::atomic::AtomicUsize>) -> Self {
        Self {
            pending: std::sync::Mutex::new(BTreeMap::new()),
            put_at: std::sync::Mutex::new(None),
            notify: tokio::sync::Notify::new(),
            closed: std::sync::atomic::AtomicBool::new(false),
            bytes: std::sync::atomic::AtomicUsize::new(0),
            global,
        }
    }

    pub fn pending_bytes(&self) -> usize {
        self.bytes.load(Ordering::Relaxed)
    }

    /// Replace this terminal's pending snapshot. Never awaits, because the caller is on a
    /// path that must not block (invariant I14).
    pub fn put(&self, stream: u32, snapshot: Snapshot) {
        let added = snapshot_bytes(&snapshot);
        let removed = {
            let mut pending = self.pending.lock().unwrap();
            pending
                .insert(stream, snapshot)
                .as_ref()
                .map(snapshot_bytes)
                .unwrap_or(0)
        };
        self.charge(added as isize - removed as isize);
        {
            let mut at = self.put_at.lock().unwrap();
            if at.is_none() {
                *at = Some(std::time::Instant::now());
            }
        }
        self.notify.notify_one();
    }

    fn charge(&self, delta: isize) {
        if delta >= 0 {
            let d = delta as usize;
            self.bytes.fetch_add(d, Ordering::Relaxed);
            self.global.fetch_add(d, Ordering::Relaxed);
        } else {
            let d = (-delta) as usize;
            self.bytes.fetch_sub(d, Ordering::Relaxed);
            self.global.fetch_sub(d, Ordering::Relaxed);
        }
    }

    /// Take everything pending. Returns an empty vector when there is nothing.
    pub fn take(&self) -> Vec<(u32, Snapshot)> {
        let taken: Vec<(u32, Snapshot)> = std::mem::take(&mut *self.pending.lock().unwrap())
            .into_iter()
            .collect();
        let freed: usize = taken.iter().map(|(_, s)| snapshot_bytes(s)).sum();
        self.charge(-(freed as isize));
        if trace_latency()
            && let Some(at) = self.put_at.lock().unwrap().take()
        {
            eprintln!("trace: queue to writer {:?}", at.elapsed());
        }
        taken
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
        // Whatever is still held stops counting against the global ceiling, or a closed
        // connection would keep a share of the budget forever.
        let _ = self.take();
        self.notify.notify_one();
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    async fn wait(&self) {
        self.notify.notified().await
    }
}

/// What a connection has been sent and what it says it has applied.
///
/// Two counters rather than a queue depth, because the question a daemon has to answer
/// under load is "is this peer reading", and a queue depth cannot distinguish a peer that
/// is slow from one that is merely showing a large terminal.
#[derive(Default)]
pub struct Progress {
    pub sent: AtomicU64,
    pub acked: AtomicU64,
    /// When an acknowledgement last arrived. `None` means none ever has.
    pub last_ack: std::sync::Mutex<Option<std::time::Instant>>,
}

impl Progress {
    /// Messages sent and not yet acknowledged.
    pub fn outstanding(&self) -> u64 {
        self.sent
            .load(Ordering::Relaxed)
            .saturating_sub(self.acked.load(Ordering::Relaxed))
    }
}

/// Everything a connection needs that belongs to the daemon rather than to the connection.
///
/// A struct rather than nine parameters, because the list had reached the point where the
/// compiler was the only thing keeping the order straight.
pub struct ServeContext {
    pub terminals: Arc<TerminalRegistry>,
    pub shell: PathBuf,
    pub warnings: Arc<std::sync::Mutex<Vec<Warning>>>,
    pub progress: Arc<Progress>,
    pub shutdown: Arc<tokio::sync::Notify>,
    pub outbound: Arc<std::sync::atomic::AtomicUsize>,
    pub store: crate::store::StoreHandle,
    pub workspace: i64,
    pub history_dir: Option<PathBuf>,
    pub scanner: Arc<ContainmentScanner>,
    pub agents: Arc<crate::agents::AgentRegistry>,
    pub events: crate::eventlog::EventLog,
}

async fn serve_client(
    mut stream: UnixStream,
    client: ClientId,
    writable: bool,
    context: ServeContext,
) -> io::Result<()> {
    let ServeContext {
        terminals,
        shell,
        warnings,
        progress,
        shutdown,
        outbound,
        store,
        workspace,
        history_dir,
        scanner,
        agents,
        events,
    } = context;
    // Filesystem permissions are the first gate, not the only one. A peer from another uid
    // is closed with no protocol response at all.
    let uid = peer_uid(&stream)?;
    if !peer_is_ours(uid) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("peer uid {uid} is not ours"),
        ));
    }

    let mut inbuf = BytesMut::with_capacity(64 * 1024);
    let mut read = [0u8; 16 * 1024];

    // Handshake first, on channel 0, before anything else is accepted.
    let hello = loop {
        let n = stream.read(&mut read).await?;
        if n == 0 {
            return Ok(());
        }
        inbuf.extend_from_slice(&read[..n]);
        if let Some(frame) = Frame::decode(&mut inbuf).map_err(to_io)? {
            if frame.channel != Channel::Handshake {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "first frame must be a handshake",
                ));
            }
            break serde_json::from_slice::<qoro_proto::ClientHello>(&frame.body)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        }
    };

    let outcome = negotiate(&hello, &ServerCapabilities::first_generation());
    let body = match &outcome {
        qoro_proto::HandshakeOutcome::Welcome(w) => serde_json::to_vec(w),
        qoro_proto::HandshakeOutcome::Refuse(r) => serde_json::to_vec(r),
    }
    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let mut out = BytesMut::new();
    Frame::new(Channel::Handshake, 0, Bytes::from(body))
        .encode(&mut out)
        .map_err(to_io)?;
    stream.write_all(&out).await?;
    if matches!(outcome, qoro_proto::HandshakeOutcome::Refuse(_)) {
        return Ok(());
    }

    let (mut rd, mut wr) = stream.into_split();
    let writer_progress = progress.clone();
    let renders = Arc::new(RenderQueue::new(outbound.clone()));
    let (control_tx, mut control_rx) = mpsc::channel::<BytesMut>(32);
    // Subscription tasks, one per terminal this connection is watching. Kept so that
    // unsubscribing, and ending the connection, actually stop them.
    let subscriptions: Arc<std::sync::Mutex<BTreeMap<u32, tokio::task::AbortHandle>>> =
        Arc::default();

    // A connection attaches to the lowest numbered terminal by default, which is the whole
    // of what a client with no arguments wants and is what milestone 1 shipped. Everything
    // else is a subscription request on the control channel.
    if let Some(first) = terminals.streams().first().copied() {
        subscribe(&terminals, &subscriptions, &renders, first);
    }

    // News for this connection, on its own channel. One task per connection rather than a
    // branch inside the writer: the writer's loop is the render hot path, and a lagging
    // broadcast receiver there would make a slow client's problem into everyone's.
    let event_tx = control_tx.clone();
    let mut changes = agents.subscribe();
    let events_task = tokio::spawn(async move {
        loop {
            let terminal = match changes.recv().await {
                Ok(terminal) => terminal,
                // Lagged means this connection missed some. The right answer is to say so
                // once rather than to replay: the message is "look again", and looking
                // again once covers everything that was missed.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            };
            let body = match serde_json::to_vec(&crate::control::EventMessage::AgentChanged {
                terminal,
            }) {
                Ok(body) => body,
                Err(_) => return,
            };
            let mut out = BytesMut::new();
            if Frame::new(Channel::Event, 0, Bytes::from(body))
                .encode(&mut out)
                .is_err()
            {
                return;
            }
            if event_tx.send(out).await.is_err() {
                return;
            }
        }
    });

    let writer_renders = renders.clone();
    let writer = tokio::spawn(async move {
        // One sequence per stream, because the reassembler requires consecutive numbers
        // within a chunk sequence and streams are reassembled independently.
        let mut seq: BTreeMap<u32, u32> = BTreeMap::new();
        loop {
            let mut buf = BytesMut::new();

            // Control responses first. They are small, they are answers to questions
            // somebody is waiting on, and a render burst must not delay them.
            while let Ok(bytes) = control_rx.try_recv() {
                buf.extend_from_slice(&bytes);
            }

            let mut rendered = 0usize;
            for (stream, snapshot) in writer_renders.take() {
                let body = match serde_json::to_vec(&SnapshotWire::from(&snapshot)) {
                    Ok(b) => b,
                    Err(_) => return,
                };
                // A full snapshot at a large size does not fit in one frame, so the render
                // lane chunks like any other message. All the frames of one snapshot are
                // written together, which is what makes ADR-013's rule true by
                // construction: a newer snapshot replaces an older one only between
                // messages, never inside a chunk sequence.
                let counter = seq.entry(stream).or_default();
                let frames =
                    qoro_proto::chunk(Channel::Terminal, stream, *counter, None, Bytes::from(body));
                *counter = counter.wrapping_add(frames.len() as u32);
                if frames.iter().any(|f| f.encode(&mut buf).is_err()) {
                    return;
                }
                rendered += 1;
            }

            if !buf.is_empty() {
                if wr.write_all(&buf).await.is_err() {
                    return;
                }
                writer_progress
                    .sent
                    .fetch_add(rendered as u64, Ordering::Relaxed);
            }

            if writer_renders.is_closed() {
                return;
            }
            tokio::select! {
                _ = writer_renders.wait() => {}
                Some(bytes) = control_rx.recv() => {
                    if wr.write_all(&bytes).await.is_err() {
                        return;
                    }
                }
            }
        }
    });

    let mut inbuf = inbuf;
    // A command larger than one frame arrives as a chunk sequence. The reassembler is what
    // bounds that: per stream, per connection, and by count, so a peer that starts a
    // sequence and never ends it cannot grow the daemon without limit.
    let mut reassembler = Reassembler::new();

    // Everything below runs inside a closure whose result is discarded, so that the cleanup
    // after it runs on every exit. It previously ran only on the `break` paths, and a
    // single malformed frame therefore left the size ownership stranded, the subscription
    // tasks running and the socket open.
    let outcome: io::Result<()> = async {
        loop {
            // The reassembly deadline is only evaluated when a frame arrives, so a peer that
            // sends one chunk and goes quiet would hold its buffers until it disconnected.
            // The tick is what makes the deadline real.
            let n = tokio::select! {
                biased;
                _ = shutdown.notified() => return Ok(()),
                read_result = rd.read(&mut read) => read_result?,
                _ = tokio::time::sleep(SWEEP_INTERVAL) => {
                    reassembler.expire(std::time::Instant::now());
                    continue;
                }
            };
            if n == 0 {
                break;
            }
            inbuf.extend_from_slice(&read[..n]);
            while let Some(frame) = Frame::decode(&mut inbuf).map_err(to_io)? {
                if !matches!(frame.channel, Channel::Terminal | Channel::Control) {
                    continue;
                }
                let channel = frame.channel;
                let Some(message) = reassembler
                    .push(frame, std::time::Instant::now())
                    .map_err(to_io)?
                else {
                    continue;
                };
                if channel == Channel::Control {
                    let response = match serde_json::from_slice::<ControlRequest>(&message.body) {
                        Ok(request) => {
                            handle_control(
                                request,
                                &terminals,
                                &subscriptions,
                                &renders,
                                &shell,
                                &warnings,
                                &store,
                                workspace,
                                history_dir.clone(),
                                &client,
                                &scanner,
                                &agents,
                                &events,
                            )
                            .await
                        }
                        Err(e) => ControlResponse::Failed {
                            // No request id to echo, because the request did not parse. The
                            // empty id is the honest answer, and it is why a client must match
                            // on its own id rather than on arrival order.
                            id: String::new(),
                            reason: format!("malformed control request: {e}"),
                        },
                    };
                    let body = serde_json::to_vec(&response).map_err(to_io)?;
                    let mut out = BytesMut::new();
                    Frame::new(Channel::Control, 0, Bytes::from(body))
                        .encode(&mut out)
                        .map_err(to_io)?;
                    if control_tx.send(out).await.is_err() {
                        break;
                    }
                    continue;
                }
                if let Some(ack) = message.ack {
                    // How far the client says it has got. This is the drain rate, and it is
                    // the only honest basis for choosing an eviction victim: buffered size
                    // picks the client with the biggest screen, not the one that stopped
                    // reading.
                    progress.acked.fetch_max(ack, Ordering::Relaxed);
                    *progress.last_ack.lock().unwrap() = Some(std::time::Instant::now());
                }
                if message.body.is_empty() {
                    // A standalone acknowledgement carries nothing else.
                    continue;
                }
                let cmd: ClientWire = match serde_json::from_slice(&message.body) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                // The frame's stream field is the terminal. A command for a terminal that has
                // gone is dropped rather than routed to a neighbour, which is why stream
                // numbers are never reused.
                let Some(handle) = terminals.get(message.stream) else {
                    continue;
                };
                let cmd = match cmd {
                    ClientWire::Input { bytes } => ClientCommand::Input {
                        from: client.clone(),
                        bytes: Bytes::from(bytes),
                    },
                    ClientWire::Resize { cols, rows } => ClientCommand::Resize {
                        from: client.clone(),
                        writable,
                        cols,
                        rows,
                    },
                };
                if handle.commands.send(cmd).await.is_err() {
                    break;
                }
            }
        }
        Ok(())
    }
    .await;

    // One exit. Aborting the writer is what drops the write half and closes the socket, so
    // it has to happen however the loop ended.
    events_task.abort();
    renders.close();
    for (_, task) in std::mem::take(&mut *subscriptions.lock().unwrap()) {
        task.abort();
    }
    writer.abort();
    // Ownership must be released when the connection ends, or a dead owner strands the
    // geometry. This is the one automatic transfer ADR-007 allows, and it goes to every
    // terminal the connection could have owned.
    for stream in terminals.streams() {
        if let Some(handle) = terminals.get(stream) {
            let _ = handle
                .commands
                .send(ClientCommand::Detached {
                    client: client.clone(),
                })
                .await;
        }
    }
    outcome
}

/// Start watching a terminal on behalf of one connection.
///
/// One task per subscription, holding that terminal's `watch::Receiver`. It coalesces on
/// its own terminal only, so a terminal rendering faster than the client drains loses its
/// own intermediate frames and nobody else's.
fn subscribe(
    terminals: &Arc<TerminalRegistry>,
    subscriptions: &Arc<std::sync::Mutex<BTreeMap<u32, tokio::task::AbortHandle>>>,
    renders: &Arc<RenderQueue>,
    stream: u32,
) -> bool {
    if subscriptions.lock().unwrap().contains_key(&stream) {
        // Subscribing twice is not an error and must not double the traffic.
        return true;
    }
    let Some(handle) = terminals.get(stream) else {
        return false;
    };
    let mut updates = handle.updates.subscribe();
    let queue = renders.clone();
    let task = tokio::spawn(async move {
        loop {
            // Attach delivers the current screen first, before any further update. Without
            // this a client that arrives after the shell printed its prompt sees a blank
            // terminal until something else happens to be typed.
            let update = updates.borrow_and_update().clone();
            match update {
                TerminalUpdate::Full(snapshot) => queue.put(stream, snapshot),
                TerminalUpdate::Ended(snapshot) => {
                    // The last screen is delivered, then the subscription ends. Returning
                    // without delivering it is how the child's final output disappears.
                    queue.put(stream, snapshot);
                    return;
                }
            }
            if updates.changed().await.is_err() {
                return;
            }
        }
    });
    subscriptions
        .lock()
        .unwrap()
        .insert(stream, task.abort_handle());
    true
}

/// The wire name of a runtime state.
///
/// A table and not a derive, because this string is on the socket: ADR-010 freezes the
/// shape of what crosses it, and a rename in the domain would silently rename a field
/// clients match on.
fn state_name(state: qoro_domain::AgentRuntimeState) -> &'static str {
    use qoro_domain::AgentRuntimeState as S;
    match state {
        S::Starting => "starting",
        S::Idle => "idle",
        S::Working => "working",
        S::Blocked => "blocked",
        S::Done => "done",
        S::Failed => "failed",
        S::Uncertain => "uncertain",
        S::Unknown => "unknown",
    }
}

/// Answer one control request.
///
/// Synchronous on purpose: every operation here is a registry edit or a spawn, and none of
/// them waits on a peer. An operation that later needs to wait for durability gets its own
/// path rather than making this one async and inviting a lock across an await.
#[allow(clippy::too_many_arguments)]
async fn handle_control(
    request: ControlRequest,
    terminals: &Arc<TerminalRegistry>,
    subscriptions: &Arc<std::sync::Mutex<BTreeMap<u32, tokio::task::AbortHandle>>>,
    renders: &Arc<RenderQueue>,
    shell: &Path,
    warnings: &Arc<std::sync::Mutex<Vec<Warning>>>,
    store: &crate::store::StoreHandle,
    workspace: i64,
    history_dir: Option<PathBuf>,
    client: &ClientId,
    scanner: &Arc<ContainmentScanner>,
    agents: &Arc<crate::agents::AgentRegistry>,
    events: &crate::eventlog::EventLog,
) -> ControlResponse {
    let id = request.request_id().to_string();

    // A command that has already been applied returns its original answer rather than
    // being done again. ADR-017: the ledger is what makes a retry after an ambiguous
    // failure safe, and the reason it is a lookup and not a scan of the log is that a scan
    // gets slower exactly as the log grows.
    if is_mutating(&request)
        && let Ok(Some((ok, detail))) = events.applied(id.clone()).await
    {
        return replayed_response(&id, ok, &detail, &request);
    }

    match request {
        ControlRequest::CreateTerminal {
            cwd,
            workspace: wanted,
            ..
        } => {
            // The workspace the client asked for, or the one the daemon opened. A client
            // that does not know about workspaces sends nothing and lands in the default,
            // which is what it already believed was happening.
            let workspace = wanted.unwrap_or(workspace);
            // A requested working directory is validated here rather than trusted. It has
            // to be absolute, because a relative path would resolve against whatever
            // directory the daemon happens to be in, and it has to exist, because a shell
            // that cannot chdir starts somewhere the client did not ask for. Answering
            // `TerminalCreated` while ignoring the field was the first version, and it left
            // a client with no way to tell where its shell actually started.
            let cwd = match cwd.map(PathBuf::from) {
                None => None,
                Some(path) if !path.is_absolute() => {
                    return ControlResponse::Failed {
                        id,
                        reason: format!("cwd {} is not absolute", path.display()),
                    };
                }
                Some(path) if !path.is_dir() => {
                    return ControlResponse::Failed {
                        id,
                        reason: format!("cwd {} is not a directory", path.display()),
                    };
                }
                Some(path) => Some(path),
            };
            // Recorded before it is started, and acknowledged only once the row is
            // durable (ADR-005 rule 2). A create acknowledged before the write is a create
            // a client cannot safely retry.
            let recorded_cwd = cwd
                .as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| ".".to_string());
            if let Err(e) = store
                .create_terminal(workspace, recorded_cwd.clone(), shell.display().to_string())
                .await
            {
                return ControlResponse::Failed {
                    id,
                    reason: format!("could not record the terminal: {e}"),
                };
            }
            // The intent, durable before anything is started. A restart that finds this
            // with no outcome knows a terminal may have been created and not recorded.
            if let Err(e) = events
                .append_durable(vec![(
                    qoro_events::Stream::Command(id.clone()),
                    qoro_events::Body::Command {
                        idempotency_key: id.clone(),
                        operation: "CreateTerminal".into(),
                        detail: serde_json::json!({ "cwd": recorded_cwd }),
                    },
                )])
                .await
            {
                return ControlResponse::Failed {
                    id,
                    reason: format!("could not record the intent: {e}"),
                };
            }
            match open_terminal(
                terminals,
                workspace,
                shell,
                cwd.as_deref(),
                warnings.clone(),
                history_dir.clone(),
                scanner.clone(),
                agents.clone(),
                events.clone(),
            ) {
                Ok(handle) => {
                    let stream = handle
                        .id
                        .as_str()
                        .parse()
                        .expect("the registry mints numeric ids");
                    // Subscribed on creation. A client that asked for a terminal wants to
                    // see it, and a separate round trip to say so is a race in which the
                    // first output is lost.
                    subscribe(terminals, subscriptions, renders, stream);
                    // The outcome, durable before the client is told. ADR-005 rule 2: a
                    // client is told it succeeded only once the log says so, which is what
                    // makes its retry idempotent rather than a second terminal.
                    if let Err(e) = events
                        .append_durable(vec![(
                            qoro_events::Stream::Command(id.clone()),
                            qoro_events::Body::Outcome {
                                idempotency_key: id.clone(),
                                ok: true,
                                detail: serde_json::json!({ "terminal": stream }),
                            },
                        )])
                        .await
                    {
                        return ControlResponse::Failed {
                            id,
                            reason: format!("the terminal started but was not recorded: {e}"),
                        };
                    }
                    ControlResponse::TerminalCreated {
                        id,
                        terminal: stream,
                    }
                }
                Err(e) => ControlResponse::Failed {
                    id,
                    reason: format!("cannot create a terminal: {e}"),
                },
            }
        }
        ControlRequest::CloseTerminal { terminal, .. } => match terminals.get(terminal) {
            Some(handle) => {
                // Closing is a request to the terminal, not a registry edit: the terminal
                // owns its teardown, which has to contain what it started (ADR-014).
                //
                // The result is not discarded. A terminal whose command channel is full,
                // because another connection is pasting into it, would otherwise be
                // reported closed to a client that then stops subscribing while the shell
                // keeps running, with no way for that client to find out.
                match handle.commands.try_send(ClientCommand::Close) {
                    Ok(()) => {
                        let _ = events
                            .append_durable(vec![
                                (
                                    qoro_events::Stream::Command(id.clone()),
                                    qoro_events::Body::Command {
                                        idempotency_key: id.clone(),
                                        operation: "CloseTerminal".into(),
                                        detail: serde_json::json!({ "terminal": terminal }),
                                    },
                                ),
                                (
                                    qoro_events::Stream::Command(id.clone()),
                                    qoro_events::Body::Outcome {
                                        idempotency_key: id.clone(),
                                        ok: true,
                                        detail: serde_json::json!({ "terminal": terminal }),
                                    },
                                ),
                            ])
                            .await;
                        ControlResponse::TerminalClosed { id, terminal }
                    }
                    Err(e) => ControlResponse::Failed {
                        id,
                        reason: format!("terminal {terminal} did not accept the close: {e}"),
                    },
                }
            }
            None => ControlResponse::Failed {
                id,
                reason: format!("no terminal {terminal}"),
            },
        },
        ControlRequest::ListTerminals { .. } => ControlResponse::Terminals {
            id,
            terminals: terminals.streams(),
        },
        ControlRequest::ListWorkspaces { .. } => {
            // Counted from the terminals that are open now rather than from the store: the
            // store knows which terminals were ever created in a workspace, and a client
            // drawing a list wants to know which ones are alive.
            let live = terminals.streams();
            match store.workspaces().await {
                Ok(rows) => ControlResponse::Workspaces {
                    id,
                    workspaces: rows
                        .into_iter()
                        .map(|row| crate::control::WorkspaceRow {
                            id: row.id,
                            name: row.name,
                            terminals: live
                                .iter()
                                .filter(|s| terminals.workspace_of(**s) == Some(row.id))
                                .count(),
                        })
                        .collect(),
                },
                Err(e) => ControlResponse::Failed {
                    id,
                    reason: format!("the workspaces could not be read: {e}"),
                },
            }
        }
        ControlRequest::CreateWorkspace { name, .. } => match store.create_workspace(name).await {
            Ok(workspace) => ControlResponse::WorkspaceCreated { id, workspace },
            Err(e) => ControlResponse::Failed {
                id,
                reason: format!("the workspace could not be created: {e}"),
            },
        },
        ControlRequest::ListAgents { .. } => {
            // Read from the registry rather than by walking the process table here: the
            // registry is what the process watcher and the observation precedence in
            // ADR-008 already maintain, and a second reader would be a second answer.
            let rows = terminals
                .streams()
                .into_iter()
                .map(|terminal| {
                    let agent = agents.agent(terminal);
                    let (program, behind, generation) = match agent.as_ref().map(|a| &a.identity) {
                        Some(qoro_domain::AgentIdentity::Known {
                            program,
                            generation,
                        }) => (Some(program.clone()), None, Some(generation.0)),
                        Some(qoro_domain::AgentIdentity::Unavailable {
                            boundary: qoro_domain::Boundary::Namespace { wrapper },
                            generation,
                        }) => (None, Some(wrapper.clone()), Some(generation.0)),
                        _ => (None, None, None),
                    };
                    let state = agent
                        .as_ref()
                        .map(|a| state_name(a.belief.state))
                        .unwrap_or("unknown")
                        .to_string();
                    crate::control::AgentRow {
                        terminal,
                        workspace: terminals.workspace_of(terminal).unwrap_or(workspace),
                        // Asked of the same table that resolved the name in the first
                        // place. The wrappers have already been seen through by the time
                        // the name is stored, so there is nothing left here to unwrap and
                        // no argv to pass.
                        agent: program
                            .as_deref()
                            .is_some_and(|p| qoro_domain::program::identify(p, &[]).agent),
                        program,
                        behind,
                        state,
                        generation,
                    }
                })
                .collect();
            ControlResponse::Agents { id, agents: rows }
        }
        ControlRequest::Subscribe {
            terminals: wanted, ..
        } => {
            let subscribed: Vec<u32> = wanted
                .into_iter()
                .filter(|s| subscribe(terminals, subscriptions, renders, *s))
                .collect();
            ControlResponse::Subscribed {
                id,
                terminals: subscribed,
            }
        }
        ControlRequest::RequestOwnership { terminal, .. } => match terminals.get(terminal) {
            // Nothing moves here. The request is recorded as having reached the terminal,
            // and the owner decides. A version of this that took ownership after a timeout
            // would be the steal ADR-007 exists to prevent.
            Some(_) => ControlResponse::OwnershipRequested { id, terminal },
            None => ControlResponse::Failed {
                id,
                reason: format!("no terminal {terminal}"),
            },
        },
        ControlRequest::GrantOwnership { terminal, to, .. } => match terminals.get(terminal) {
            Some(handle) => {
                let granted = ClientCommand::GrantOwnership {
                    from: client.clone(),
                    to: ClientId::new(to.clone()),
                };
                match handle.commands.try_send(granted) {
                    Ok(()) => ControlResponse::OwnershipGranted { id, terminal, to },
                    Err(e) => ControlResponse::Failed {
                        id,
                        reason: format!("terminal {terminal} did not accept the grant: {e}"),
                    },
                }
            }
            None => ControlResponse::Failed {
                id,
                reason: format!("no terminal {terminal}"),
            },
        },
        ControlRequest::Unsubscribe {
            terminals: wanted, ..
        } => {
            let mut held = subscriptions.lock().unwrap();
            let dropped: Vec<u32> = wanted
                .into_iter()
                .filter_map(|s| held.remove(&s).map(|t| (s, t)))
                .map(|(s, task)| {
                    task.abort();
                    s
                })
                .collect();
            ControlResponse::Subscribed {
                id,
                terminals: dropped,
            }
        }
    }
}

/// Whether a control request changes anything. Only these are journalled and only these
/// are idempotent: a listing that was answered from a ledger would be a stale listing.
fn is_mutating(request: &ControlRequest) -> bool {
    matches!(
        request,
        ControlRequest::CreateTerminal { .. } | ControlRequest::CloseTerminal { .. }
    )
}

/// The answer a replayed command gets: the original one.
fn replayed_response(
    id: &str,
    ok: bool,
    detail: &serde_json::Value,
    request: &ControlRequest,
) -> ControlResponse {
    if !ok {
        return ControlResponse::Failed {
            id: id.to_string(),
            reason: detail
                .get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or("the original attempt failed")
                .to_string(),
        };
    }
    let terminal = detail.get("terminal").and_then(|t| t.as_u64()).unwrap_or(0) as u32;
    match request {
        ControlRequest::CreateTerminal { .. } => ControlResponse::TerminalCreated {
            id: id.to_string(),
            terminal,
        },
        _ => ControlResponse::TerminalClosed {
            id: id.to_string(),
            terminal,
        },
    }
}

fn to_io<E: std::fmt::Display>(e: E) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

/// The terminal codec's first generation. JSON for milestone 1: the shape is what the gates
/// assert against, and a compact encoding is an optimisation of a settled contract rather
/// than a prerequisite for one.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SnapshotWire {
    pub cols: usize,
    pub rows: usize,
    pub cursor: (usize, usize),
    pub lines: Vec<String>,
    /// How each row is coloured, as runs of columns.
    ///
    /// Defaulted on the wire, so a client built before this existed reads a snapshot from a
    /// daemon that sends it and draws the text without the colour, which is ADR-010's rule
    /// about a newer daemon saying more than an older client understands. Skipped when
    /// nothing is styled, which is most of a screen and all of a shell prompt.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub styles: Vec<Vec<SpanWire>>,
}

/// A run of columns that share a style, as it crosses the socket.
///
/// Flat and short field names, because there is one of these per colour change per row per
/// frame, and a snapshot of a build log is mostly this.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SpanWire {
    pub at: usize,
    pub len: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg: Option<ColourWire>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<ColourWire>,
    /// Bold, italic, underline, inverse, dim and strikethrough, in that order, as a small
    /// bit set: six booleans would be six fields on every run.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub attrs: u8,
}

fn is_zero(v: &u8) -> bool {
    *v == 0
}

/// A colour, kept in the terminal's own vocabulary rather than resolved to rgb.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum ColourWire {
    Indexed { i: u8 },
    Rgb { r: u8, g: u8, b: u8 },
}

/// The attribute bits, named once so the client and the daemon cannot disagree.
pub mod attrs {
    pub const BOLD: u8 = 1;
    pub const ITALIC: u8 = 2;
    pub const UNDERLINE: u8 = 4;
    pub const INVERSE: u8 = 8;
    pub const DIM: u8 = 16;
    pub const STRIKE: u8 = 32;
}

impl From<&Snapshot> for SnapshotWire {
    fn from(s: &Snapshot) -> Self {
        Self {
            cols: s.cols,
            rows: s.rows,
            cursor: s.cursor,
            lines: s.lines.clone(),
            styles: s
                .styles
                .iter()
                .map(|row| row.iter().map(SpanWire::from).collect())
                .collect(),
        }
    }
}

impl From<&qoro_terminal::Span> for SpanWire {
    fn from(s: &qoro_terminal::Span) -> Self {
        let st = &s.style;
        let mut bits = 0;
        for (on, bit) in [
            (st.bold, attrs::BOLD),
            (st.italic, attrs::ITALIC),
            (st.underline, attrs::UNDERLINE),
            (st.inverse, attrs::INVERSE),
            (st.dim, attrs::DIM),
            (st.strike, attrs::STRIKE),
        ] {
            if on {
                bits |= bit;
            }
        }
        Self {
            at: s.at,
            len: s.len,
            fg: colour_wire(st.fg),
            bg: colour_wire(st.bg),
            attrs: bits,
        }
    }
}

/// `None` for the terminal's own default, which is the common case and is not worth a field.
fn colour_wire(c: qoro_terminal::Colour) -> Option<ColourWire> {
    match c {
        qoro_terminal::Colour::Default => None,
        qoro_terminal::Colour::Indexed(i) => Some(ColourWire::Indexed { i }),
        qoro_terminal::Colour::Rgb(r, g, b) => Some(ColourWire::Rgb { r, g, b }),
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientWire {
    Input { bytes: Vec<u8> },
    Resize { cols: u16, rows: u16 },
}

/// Spawn the PTY, the child, and the task that owns both.
/// The longest run of bytes with no line terminator in it.
///
/// This, not "does the payload contain a newline", is what the line discipline cares about.
/// A single 512 KiB line is discarded whether or not it ends in a newline: measured against
/// a real pty in ICANON with `cat` on the far side, 524288 bytes written and 0 received,
/// with every write returning success. The first version asked the wrong question and so
/// stayed silent for exactly the paste a user is most likely to make, a long base64 blob
/// followed by Enter.
///
/// A trailing run counts: a payload that ends mid line is still a line so far, and the rest
/// of it arrives in the next write.
fn longest_line(bytes: &[u8]) -> usize {
    bytes
        .split(|b| *b == b'\n' || *b == b'\r')
        .map(<[u8]>::len)
        .max()
        .unwrap_or(0)
}

/// Whether the child's line discipline is in canonical mode.
fn is_canonical(master: std::os::fd::RawFd) -> bool {
    // SAFETY: zeroed termios is a valid out-param and master is live.
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: master is live and t is a valid out-param.
    if unsafe { libc::tcgetattr(master, &mut t) } != 0 {
        return false;
    }
    t.c_lflag & libc::ICANON != 0
}

/// Create a terminal, register it, and start its task.
///
/// Returns the handle so that a caller creating a terminal on request can acknowledge with
/// the id it will be addressed by. The registration happens before the task starts, so a
/// command that arrives immediately after the acknowledgement has somewhere to go.
#[allow(clippy::too_many_arguments)]
pub fn open_terminal(
    terminals: &Arc<TerminalRegistry>,
    workspace: i64,
    shell: &Path,
    cwd: Option<&Path>,
    warnings: Arc<std::sync::Mutex<Vec<Warning>>>,
    history_dir: Option<PathBuf>,
    scanner: Arc<ContainmentScanner>,
    agents: Arc<crate::agents::AgentRegistry>,
    events: crate::eventlog::EventLog,
) -> io::Result<TerminalHandle> {
    let stream = terminals.mint();
    let (commands, from_clients) = mpsc::channel(256);
    let (updates, _) = watch::channel(TerminalUpdate::Full(Snapshot {
        cols: 80,
        rows: 24,
        cursor: (0, 0),
        lines: vec![String::new(); 24],
        styles: Vec::new(),
    }));
    let token = crate::agents::mint_token(stream);
    let terminal_id = TerminalId::new(stream.to_string());
    agents.register(stream, terminal_id.clone(), token.clone());
    let handle = TerminalHandle {
        workspace,
        id: terminal_id,
        token: token.clone(),
        commands,
        updates: updates.clone(),
    };
    terminals.insert(stream, handle.clone());
    let registry = terminals.clone();
    let spill = crate::spill::SpillRing::new();
    if let Some(dir) = history_dir {
        spawn_history_writer(dir.join(stream.to_string()), spill.clone());
    }
    let forgetting = agents.clone();
    spawn_terminal(
        TerminalSpawn {
            shell: shell.to_path_buf(),
            cwd: cwd.map(Path::to_path_buf),
            token,
            stream,
            agents,
            events,
        },
        from_clients,
        updates,
        warnings,
        spill,
        scanner,
        move || {
            registry.remove(stream);
            forgetting.forget(stream);
        },
    )?;
    Ok(handle)
}

/// The half of the history path that is allowed to wait on a disk.
///
/// One task per terminal, on the async runtime, going to the blocking pool only when it
/// actually writes. Not one blocking task per terminal: that would make the thread count
/// grow with the terminal count, which is exactly what gate G2.1 forbids.
fn spawn_history_writer(dir: PathBuf, spill: crate::spill::SpillRing) {
    tokio::spawn(async move {
        // The open is a blocking call too, and a hundred of them at once is a hundred
        // threads.
        let mut history = match crate::blocking::run({
            let dir = dir.clone();
            move || qoro_store::History::open(&dir)
        })
        .await
        {
            Ok(Ok(h)) => h,
            _ => {
                eprintln!("qorod: history for {} is unavailable", dir.display());
                return;
            }
        };
        let mut unflushed = false;
        loop {
            // A timeout only while there is something to flush.
            //
            // Flushing only when a new batch arrives loses everything buffered the moment a
            // terminal goes quiet, which is exactly when a person scrolls back to look at
            // it: measured, a terminal that printed four thousand lines and stopped had
            // none of them on disk. So there is a timer.
            //
            // But a timer per terminal that runs whether or not there is anything to flush
            // is two hundred wakeups a second at a hundred terminals: measured, that alone
            // took a hundred idle terminals from nothing at all to 80 milliseconds of CPU a
            // minute against a 50 millisecond budget. Nothing buffered, nothing to wake up
            // for.
            let woken = if unflushed {
                tokio::time::timeout(HISTORY_FLUSH_INTERVAL, spill.wait())
                    .await
                    .is_ok()
            } else {
                spill.wait().await;
                true
            };
            let (rows, gap) = spill.take();
            if gap.rows > 0 {
                // Recorded, never silent. The rows are gone; saying how many and when is
                // the difference between lossy and wrong.
                eprintln!(
                    "qorod: history for {} lost {} rows between {} and {}",
                    dir.display(),
                    gap.rows,
                    gap.first_unix,
                    gap.last_unix
                );
            }
            if rows.is_empty() {
                if unflushed && !woken {
                    let flushed = crate::blocking::run(move || {
                        let _ = history.flush();
                        history
                    })
                    .await;
                    match flushed {
                        Ok(h) => history = h,
                        Err(_) => return,
                    }
                    unflushed = false;
                }
                continue;
            }
            // A burst becomes one batch. Waking on every notify makes the batches as small
            // as the reads that produced them, and a batch is a blocking dispatch and a set
            // of syscalls however few rows are in it.
            tokio::time::sleep(HISTORY_BATCH_WINDOW).await;
            let (mut rows, _) = (rows, ());
            rows.extend(spill.take().0);
            let written = crate::blocking::run(move || {
                let truncated = history.append(&rows);
                (history, truncated)
            })
            .await;
            match written {
                Ok((h, Ok(truncated))) => {
                    history = h;
                    unflushed = true;
                    if truncated > 0 {
                        eprintln!(
                            "qorod: history for {} truncated {truncated} bytes of an \
                             oversized row",
                            dir.display()
                        );
                    }
                }
                Ok((h, Err(e))) => {
                    history = h;
                    eprintln!("qorod: history write failed: {e}");
                }
                Err(_) => return,
            }
        }
    });
}

/// What a terminal needs to be started, as one value rather than five parameters.
pub struct TerminalSpawn {
    pub shell: PathBuf,
    pub cwd: Option<PathBuf>,
    pub token: qoro_domain::CapabilityToken,
    pub stream: u32,
    pub agents: Arc<crate::agents::AgentRegistry>,
    pub events: crate::eventlog::EventLog,
}

#[allow(clippy::too_many_arguments)]
fn spawn_terminal(
    spawn: TerminalSpawn,
    mut from_clients: mpsc::Receiver<ClientCommand>,
    updates: watch::Sender<TerminalUpdate>,
    warnings: Arc<std::sync::Mutex<Vec<Warning>>>,
    spill: crate::spill::SpillRing,
    scanner: Arc<ContainmentScanner>,
    on_end: impl FnOnce() + Send + 'static,
) -> io::Result<()> {
    let TerminalSpawn {
        shell,
        cwd,
        token,
        stream,
        agents,
        events,
    } = spawn;
    let register_master = agents.clone();
    let shell = shell.as_path();
    let cwd = cwd.as_deref();
    let pty = sys::open_pty(80, 24)?;
    let master_fd = pty.master;
    let slave_fd = pty.slave;

    let mut cmd = std::process::Command::new(shell);
    cmd.arg("-i");
    // SAFETY: the closure runs post-fork and calls only async-signal-safe syscalls, with no
    // allocation. setsid plus TIOCSCTTY is what gives the child its own session with this
    // pty as its controlling terminal, which is the containment identity teardown uses.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.stdin(dup_stdio(&slave_fd)?)
            .stdout(dup_stdio(&slave_fd)?)
            .stderr(dup_stdio(&slave_fd)?)
            .pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
    }
    cmd.env("TERM", "xterm-256color");
    // The capability token, handed to the child of a terminal we started. ADR-015: it binds
    // the terminal and not the generation, because a child's environment cannot be rewritten
    // when the generation changes and a generation bound token would be stale from the
    // first command onward.
    cmd.env("QORO_TERMINAL", stream.to_string());
    cmd.env("QORO_TOKEN", token.as_str());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    // The containment identity, taken from the SLAVE while we still hold it.
    //
    // The first version read it from the master, on the stated assumption that a pty pair
    // shares a device number. It does not: measured on macOS, master 251658270 and slave
    // 268435486. Every enumeration therefore matched nothing, containment was empty at
    // every teardown, and every escaped job survived. Nothing else would have noticed,
    // because an empty containment set looks exactly like a terminal with nothing to
    // contain.
    let tty_dev = sys::tty_dev(slave_fd.as_raw_fd())?;
    let child = cmd.spawn()?;
    // The parent must not keep the slave, or the master never sees end of stream.
    drop(slave_fd);
    // The registry re-reads the foreground group on a process event, which it cannot do
    // through the terminal's own task: that task is parked waiting for output that a
    // `sleep` will never produce.
    register_master.attach_master(stream, master_fd.as_raw_fd());

    tokio::spawn(async move {
        if let Err(e) = terminal_task(
            master_fd,
            child,
            tty_dev,
            &mut from_clients,
            updates.clone(),
            warnings,
            spill,
            scanner,
            agents,
            stream,
            events,
        )
        .await
        {
            eprintln!("qorod: terminal ended: {e}");
        }
        // Deregistered before the last update, so a client that reacts to `Gone` by asking
        // for the terminal cannot find a handle whose task has already returned.
        on_end();
        let last = match updates.borrow().clone() {
            TerminalUpdate::Full(snapshot) | TerminalUpdate::Ended(snapshot) => snapshot,
        };
        let _ = updates.send(TerminalUpdate::Ended(last));
    });
    Ok(())
}

fn dup_stdio(fd: &OwnedFd) -> io::Result<std::process::Stdio> {
    // SAFETY: fd is live; dup produces a fresh descriptor owned by nobody else. The dup is
    // deliberately inheritable: these three become the child's stdin, stdout and stderr.
    let d = unsafe { libc::dup(fd.as_raw_fd()) };
    if d < 0 {
        return Err(io::Error::last_os_error());
    }
    // `dup` always clears close on exec, which undid the flag `open_pty` sets at open. The
    // descriptors then sit in the daemon, inheritable, from the moment the command is
    // configured until `spawn` returns, and anything else spawned in that window inherits
    // this terminal's slave. Two clients creating terminals at once is enough. `dup2` in
    // the child clears the flag on the target, so 0, 1 and 2 still arrive inheritable.
    sys::set_cloexec(d)?;
    // SAFETY: d is fresh and unowned.
    let owned = unsafe { OwnedFd::from_raw_fd(d) };
    Ok(std::process::Stdio::from(owned))
}

/// Terminals waiting to have their foreground group re-read.
///
/// A set rather than a queue, because two events for one terminal are one thing to do, and
/// a queue would settle it twice.
#[derive(Default)]
pub struct SettleQueue {
    pending: std::sync::Mutex<std::collections::BTreeSet<u32>>,
    notify: tokio::sync::Notify,
}

impl SettleQueue {
    fn mark(&self, streams: Vec<u32>) {
        if streams.is_empty() {
            return;
        }
        self.pending.lock().unwrap().extend(streams);
        self.notify.notify_one();
    }

    fn take(&self) -> Vec<u32> {
        std::mem::take(&mut *self.pending.lock().unwrap())
            .into_iter()
            .collect()
    }

    async fn wait(&self) {
        self.notify.notified().await
    }
}

/// Re-read the foreground group of the terminals an event touched.
///
/// Over a short window rather than once. A shell forks and then moves the child into the
/// foreground, and the fork event usually arrives between the two, so a single read finds
/// the shell still in charge and concludes nothing happened. Measured: a `sleep 5` typed at
/// a prompt produces no output at all, so without this the change is invisible until
/// something else happens to print.
///
/// Its own task, and not inline in the watcher. Settling inline blocks the watcher for the
/// length of the window, so a hundred terminals changing at once are settled ten batches a
/// second: measured, 22 of 100 within twenty seconds. Here the watcher only marks, and the
/// settles coalesce across every terminal that moved.
async fn settle_loop(agents: Arc<crate::agents::AgentRegistry>, queue: Arc<SettleQueue>) {
    loop {
        queue.wait().await;
        // Everything that moved while we were waiting, settled together.
        let mut streams = queue.take();
        for step in SETTLE_STEPS {
            tokio::time::sleep(*step).await;
            // Anything that moved during the window joins this pass rather than waiting for
            // the next one.
            streams.extend(queue.take());
            streams.sort_unstable();
            streams.dedup();
            for stream in &streams {
                // The registry announces the ones that moved. `refresh_foreground` answers
                // `None` for a terminal whose leader is what it was, so a settle step that
                // finds nothing tells nobody.
                agents.refresh_foreground(*stream);
            }
        }
    }
}

/// The settle schedule. Short, bounded, and only ever entered because something happened.
pub const SETTLE_STEPS: &[std::time::Duration] = &[
    std::time::Duration::from_millis(5),
    std::time::Duration::from_millis(25),
    std::time::Duration::from_millis(70),
];

/// A borrowed descriptor for the reactor.
///
/// The kqueue descriptor belongs to the registry, which outlives this task, so the wrapper
/// borrows rather than owns: an `OwnedFd` here would close it when the task ended.
struct WatchFd(std::os::fd::RawFd);
impl AsRawFd for WatchFd {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.0
    }
}

struct MasterIo(OwnedFd);
impl AsRawFd for MasterIo {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.0.as_raw_fd()
    }
}

/// Outcome of a client's resize request, reported back so a client can tell "you are not
/// the owner" from "your message was lost".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResizeOutcome {
    Applied,
    NotOwner,
    Refused,
}

/// Arbitrate one resize request and apply it if it wins.
///
/// Extracted so the policy is testable without a PTY. The whole point of ADR-007 is that
/// there is exactly one place a resize can reach the ioctl.
pub fn arbitrate_resize(
    policy: &mut SizePolicy,
    from: &ClientId,
    writable: bool,
    size: PtySize,
) -> ResizeOutcome {
    // An unowned terminal is claimed by the next writable client. Without this the first
    // client after a disconnect could never resize again.
    if matches!(policy, SizePolicy::Unowned) && writable {
        *policy = policy
            .clone()
            .apply(OwnershipEvent::WritableAttached(from.clone()), None);
    }
    match policy.evaluate(&SizeRequest {
        from: from.clone(),
        writable,
        size,
    }) {
        SizeVerdict::Apply(_) => ResizeOutcome::Applied,
        SizeVerdict::NotOwner => ResizeOutcome::NotOwner,
        SizeVerdict::ObserverMayNotResize | SizeVerdict::PolicyFixed => ResizeOutcome::Refused,
        SizeVerdict::OutOfRange(_) => ResizeOutcome::Refused,
    }
}

#[allow(clippy::too_many_arguments)]
async fn terminal_task(
    master: OwnedFd,
    mut child: std::process::Child,
    tty_dev: u64,
    from_clients: &mut mpsc::Receiver<ClientCommand>,
    updates: watch::Sender<TerminalUpdate>,
    warnings: Arc<std::sync::Mutex<Vec<Warning>>>,
    spill: crate::spill::SpillRing,
    scanner: Arc<ContainmentScanner>,
    agents: Arc<crate::agents::AgentRegistry>,
    stream: u32,
    events: crate::eventlog::EventLog,
) -> io::Result<()> {
    let master_raw = master.as_raw_fd();
    let child_pid = child.id() as i32;

    let afd = AsyncFd::with_interest(MasterIo(master), Interest::READABLE | Interest::WRITABLE)?;
    let mut engine = AlacrittyEngine::new(80, 24);
    let mut buf = vec![0u8; 64 * 1024];
    let mut policy = SizePolicy::Unowned;
    // Bounded by bytes, with capacity reserved for terminal replies so a saturated input
    // queue cannot deadlock an application waiting for a cursor position report.
    let mut writes = WriteQueue::new(1024 * 1024, 64 * 1024);
    // Maintained during the terminal's life, not built at teardown. See the scan below.
    let mut containment = Containment::new();
    containment.absorb(qoro_pty::sys::enumerate_tty(tty_dev));
    let mut last_scan = std::time::Instant::now();
    // When to read the screen for what the agent is doing. `None` while nothing has
    // happened, which is what keeps an idle terminal at no wakeups at all.
    let mut detect_at: Option<tokio::time::Instant> = None;
    let mut pending_input: Option<std::time::Instant> = None;
    // How far this terminal's history has reached, so the log can say where to read from.
    let mut history_to = 0u64;
    let trace = trace_latency();

    // The loop is a closure whose result is captured, so that teardown runs however it
    // ends. Three `?` in here used to propagate straight out of the task: a failing
    // `TIOCSWINSZ`, or an error from either readiness, left the child, its foreground job
    // and everything it started running with the daemon's terminal gone and no record.
    let outcome: io::Result<()> = async {
        loop {
            let want_write = !writes.is_empty();
            tokio::select! {
                cmd = from_clients.recv() => {
                    let Some(cmd) = cmd else { break };
                    match cmd {
                        ClientCommand::Input { bytes, .. } => {
                        let arrived = std::time::Instant::now();
                        pending_input = Some(arrived);
                            // A large paste with no line terminator, to a child in canonical
                            // mode, is discarded by the line discipline past its limit while
                            // every write reports success. Measured: 1 MiB written, 0 received.
                            // The only honest thing to do is say so.
                            let longest = longest_line(&bytes);
                            if longest > MAX_CANON && is_canonical(master_raw) {
                                warnings.lock().unwrap().push(
                                    Warning::CanonicalPasteWithoutNewline {
                                        bytes: longest,
                                        limit: MAX_CANON,
                                    },
                                );
                            }
                            // Chunked before queueing, on boundaries that never split an escape
                            // sequence, so backpressure is observable mid-paste rather than only
                            // at "was the whole blob accepted".
                            for chunk in qoro_pty::chunk_paste(&bytes) {
                                if writes.push(chunk, WritePriority::UserInput).is_err() {
                                    // Refuse the rest rather than growing without bound. The
                                    // client is told by the queue's typed error, never silently.
                                    break;
                                }
                            }
                        }
                        ClientCommand::Resize { from, writable, cols, rows } => {
                            let size = PtySize { cols, rows };
                            if arbitrate_resize(&mut policy, &from, writable, size)
                                == ResizeOutcome::Applied
                            {
                                sys::resize(master_raw, cols, rows)?;
                                engine.resize(cols, rows);
                                let _ = updates.send(TerminalUpdate::Full(engine.snapshot()));
                            }
                        }
                        // A close ends the loop, which runs teardown on the way out. It is not
                        // a signal to the child from here: the child is contained by the
                        // teardown that follows, and signalling twice from two places is how a
                        // containment set stops being the only path.
                        ClientCommand::Close => break,
                    ClientCommand::GrantOwnership { from, to } => {
                        // Only the owner can give it away, and it is checked here rather
                        // than at the control channel, because this is the one place that
                        // holds the policy.
                        if policy == SizePolicy::Owner(from) {
                            policy = policy
                                .apply(OwnershipEvent::OwnerAgreedTransfer(to), None);
                        }
                    }
                        ClientCommand::Detached { client } => {
                            if policy == SizePolicy::Owner(client) {
                                policy = policy.apply(OwnershipEvent::OwnerDisconnected, None);
                            }
                        }
                    }
                }
                ready = afd.readable() => {
                    let mut guard = ready?;
                    let outcome = match guard
                        .try_io(|inner| read_fd(inner.get_ref().as_raw_fd(), &mut buf))
                    {
                        Ok(r) => classify_read(r),
                        Err(_would_block) => ReadOutcome::WouldBlock,
                    };
                    match outcome {
                        ReadOutcome::WouldBlock => continue,
                        // The full ADR-002 classification. Handling only WouldBlock spins a core
                        // forever once the child exits, on the idle path, where nobody is looking.
                        ReadOutcome::EndOfStream | ReadOutcome::Fatal => break,
                        ReadOutcome::Data(n) => {
                            // Refresh containment while the child is alive. After it dies the
                            // kernel has already revoked the tty association and an enumeration
                            // finds nothing, so a set built at teardown time is empty exactly
                            // when the user typed `exit`, which is the common case. Throttled,
                            // and never runs while the terminal is idle.
                            scanner.note_activity();
                            // One ioctl, measured in microseconds, so it is cheap enough to
                            // ask on every read. A change of foreground process is a new
                            // generation, and everything believed about the old one is
                            // cleared: a pid is not an identity.
                            agents.note_foreground(
                                stream,
                                qoro_pty::sys::foreground_leader(master_raw),
                            );
                            if last_scan.elapsed() >= CONTAINMENT_SCAN_INTERVAL {
                                // A map lookup, not a scan. The scan is one pass for the
                                // whole daemon, on the blocking pool, and this reads the
                                // bucket it left behind. Pruning first keeps the set to
                                // what is on this terminal now rather than everything that
                                // ever was: a terminal that has run a day of builds
                                // otherwise carries ten thousand dead pids into teardown.
                                containment.prune(&qoro_pty::sys::still_alive);
                                containment.absorb(scanner.members(tty_dev));
                                last_scan = std::time::Instant::now();
                            }
                            // Fed a slice at a time, yielding between them.
                            //
                            // `feed_in_slices` does the same work in one synchronous call,
                            // which is right for a test and wrong here: a 64 KiB read is
                            // several milliseconds of uninterruptible parsing, and twenty
                            // terminals bursting together put every runtime worker inside
                            // one of those while a keystroke for a quiet terminal waits.
                            // Measured: twenty terminals streaming a megabyte a second
                            // each took the focused terminal's echo to 62 milliseconds at
                            // the ninety fifth percentile, against 4 milliseconds with the
                            // same twenty terminals quiet and the daemon at a quarter of
                            // the machine.
                            let mut effects = Vec::new();
                            for (index, slice) in buf[..n].chunks(PARSE_SLICE).enumerate() {
                                let (_damage, mut fx) = engine.feed(slice);
                                effects.append(&mut fx);
                                let scrolled = engine.take_scrolled_off();
                                if !scrolled.is_empty() {
                                    // The log records that output happened and how far the
                                    // history now reaches, never the output itself: a log
                                    // holding megabytes a second of terminal text would be
                                    // mostly noise and would take as long to replay as the
                                    // session took to happen (ADR-017).
                                    history_to += scrolled.len() as u64;
                                    events.observe(
                                        qoro_events::Stream::Terminal(stream),
                                        qoro_events::Body::Output {
                                            terminal: stream,
                                            from_ordinal: history_to - scrolled.len() as u64,
                                            to_ordinal: history_to,
                                        },
                                    );
                                    // Handed over by value, never written from here. This
                                    // is the read loop, and the read loop never waits on
                                    // storage: an exiting child cannot complete until its
                                    // tty output is drained (ADR-005 rule 1, invariant
                                    // I14). Cloning here doubled the allocation count on
                                    // the busiest path in the daemon.
                                    spill.push(scrolled);
                                }
                                // Yielded on a turn, not on every slice, and the constant
                                // carries the measurement that chose the number.
                                if index % SLICES_PER_TURN == SLICES_PER_TURN - 1 {
                                    tokio::task::yield_now().await;
                                }
                            }
                            for reply in replies(&effects) {
                                // Replies have their own reserved budget: an application
                                // that asked a question is blocked until it is answered.
                                let _ = writes.push(reply, WritePriority::TerminalReply);
                            }
                            // Nothing changed for anyone, so nothing runs. A snapshot
                            // allocates one string per row whether or not a client is
                            // watching, and a terminal streaming in the background would
                            // do that on every read.
                            if updates.receiver_count() > 0 {
                                let _ = updates.send(TerminalUpdate::Full(engine.snapshot()));
                            }

                            // The screen is read once the output stops, not while it is
                            // arriving. A floor was the first version and it read the
                            // wrong screen: the last read of a burst is the one with the
                            // prompt on it, and that is exactly the read a floor skips,
                            // after which nothing else ever happens to trigger a look.
                            // Measured: a shell sitting at its own prompt stayed `Unknown`
                            // for the life of the process.
                            //
                            // While nothing is pending this arm is disabled, so an idle
                            // terminal still costs nothing.
                            detect_at = Some(tokio::time::Instant::now() + DETECT_SETTLE);
                            if trace && let Some(at) = pending_input.take() {
                                eprintln!("trace: input to snapshot {:?}", at.elapsed());
                            }
                        }
                    }
                }
                _ = async {
                    match detect_at {
                        Some(at) => tokio::time::sleep_until(at).await,
                        // Disabled: `select!` polls this arm only when the guard holds, and
                        // the guard is what makes an idle terminal cost nothing.
                        None => std::future::pending().await,
                    }
                }, if detect_at.is_some() => {
                    detect_at = None;
                    // Whether or not anyone is subscribed, deliberately: an agent blocked
                    // in a terminal nobody is looking at is exactly what this notices.
                    if let Some(program) = agents.program_of(stream)
                        && let Some(manifest) = qoro_detect::rules_for(&program)
                        && let Some(verdict) = manifest.evaluate(&engine.visible_text())
                        && agents.observe_screen(stream, &verdict)
                    {
                        // Recorded when it changed, not on every look. The log is what a
                        // client that was not connected reads to find out what happened
                        // while it was away, and a row per evaluation would bury that in
                        // repetitions of the same fact.
                        events.observe(
                            qoro_events::Stream::Terminal(stream),
                            qoro_events::Body::Observation {
                                terminal: stream,
                                state: format!("{:?}", verdict.state),
                                source: "ScreenHeuristic".into(),
                                confidence: "Medium".into(),
                                sampled_generation: 0,
                                // The rule's name, never the screen it matched: a screen is
                                // somebody's source code and their client's name.
                                evidence: verdict.rule.clone(),
                                redacted: false,
                            },
                        );
                    }
                }
                writable = afd.writable(), if want_write => {
                    let mut guard = writable?;
                    let Some(slice) = writes.peek().map(|s| s.to_vec()) else { continue };
                    match guard.try_io(|inner| write_fd(inner.get_ref().as_raw_fd(), &slice)) {
                        // A partial write is the normal case on a PTY. The queue keeps the
                        // offset, which is what stops the tail from being silently lost.
                        Ok(Ok(n)) => {
                        if trace && let Some(at) = pending_input {
                            eprintln!("trace: input to pty write {:?}", at.elapsed());
                        }
                        writes.advance(n)
                    }
                        Ok(Err(e)) if e.kind() == io::ErrorKind::WouldBlock => continue,
                        Ok(Err(_)) => break,
                        Err(_would_block) => continue,
                    }
                }
            }
        }

        Ok(())
    }
    .await;
    if let Err(e) = &outcome {
        eprintln!("qorod: terminal for pid {child_pid} failed: {e}");
    }

    tear_down(
        &afd,
        &mut child,
        child_pid,
        tty_dev,
        &mut containment,
        &mut engine,
        &updates,
        &scanner,
    )
    .await;
    outcome
}

/// ADR-014's ordered teardown, with the two orderings that were established by measurement.
///
/// The containment set is captured before the first signal, because the kernel revokes the
/// tty association when the leader dies and an enumeration taken afterwards finds nothing.
/// The master is drained throughout and closed last, because an exiting child is not
/// reapable until its pending output is read, and closing early discards exactly the output
/// a user needs to see.
/// One scan of the process table for the whole daemon, shared by every terminal.
///
/// Per terminal it was 360 microseconds of syscalls inside each terminal's read loop, so
/// the cost grew with the terminal count and, worse, sat on the path a keystroke travels.
/// Measured with twenty terminals streaming: the focused terminal's echo went from 57 to
/// 101 milliseconds at the ninety fifth percentile with the per-terminal scan in place.
///
/// Shared, it is one pass per interval however many terminals exist, it runs on the
/// blocking pool rather than on a runtime worker, and a terminal reads its own bucket out
/// of a map with no syscall at all.
#[derive(Default)]
pub struct ContainmentScanner {
    by_tty: std::sync::Mutex<std::collections::HashMap<u64, Vec<qoro_pty::teardown::Member>>>,
    opaque: AtomicU64,
    scans: AtomicU64,
    /// Set by any terminal that produced output. Nothing else can add a process to a
    /// terminal without the terminal saying something, so an unset flag means the answer
    /// cannot have changed.
    dirty: std::sync::atomic::AtomicBool,
    /// Woken when the flag goes from clean to dirty, so an idle daemon's scanner is parked
    /// rather than polling. A timer that wakes twenty times a second to find nothing is
    /// still twenty wakeups a second.
    woken: tokio::sync::Notify,
}

impl ContainmentScanner {
    /// What is on one terminal, from the last scan.
    pub fn members(&self, tty_dev: u64) -> Vec<qoro_pty::teardown::Member> {
        self.by_tty
            .lock()
            .unwrap()
            .get(&qoro_pty::sys::scan_key(tty_dev))
            .cloned()
            .unwrap_or_default()
    }

    /// Processes that could not be inspected in the last scan. ADR-014 requires this to be
    /// reported rather than silently skipped.
    pub fn opaque(&self) -> u64 {
        self.opaque.load(Ordering::Relaxed)
    }

    pub fn scans(&self) -> u64 {
        self.scans.load(Ordering::Relaxed)
    }

    /// Note that some terminal produced output. Relaxed and one atomic store: this is on
    /// the read path of every terminal.
    pub fn note_activity(&self) {
        // The notify only fires on the transition, so this is one atomic on the read path
        // of every terminal in the common case where the flag is already set.
        if self
            .dirty
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            self.woken.notify_one();
        }
    }

    /// Park until some terminal produces output.
    pub async fn wait_for_activity(&self) {
        if self.dirty.load(Ordering::Relaxed) {
            return;
        }
        self.woken.notified().await;
    }

    /// Whether anything has happened since the last scan.
    pub fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Relaxed)
    }

    /// Run one scan now, on the blocking pool.
    pub async fn refresh(&self) {
        self.dirty.store(false, Ordering::Relaxed);
        let Ok((by_tty, opaque)) = crate::blocking::run_on(
            crate::blocking::Pool::Lifecycle,
            qoro_pty::sys::enumerate_all,
        )
        .await
        else {
            return;
        };
        *self.by_tty.lock().unwrap() = by_tty;
        self.opaque.store(opaque as u64, Ordering::Relaxed);
        self.scans.fetch_add(1, Ordering::Relaxed);
    }
}

/// How often the containment set is refreshed while the terminal is producing output.
///
/// A scan costs 360 microseconds, measured on this machine, so at this interval continuous
/// output costs 7 milliseconds of CPU per second, and an idle terminal costs nothing at
/// all, which is the state the idle budgets are about.
///
/// The interval is also the width of the window in which a job can be started and the
/// terminal closed without the job ever being seen. 250 milliseconds was the first value
/// and it was too coarse to be real: a scripted session that starts a background job and
/// exits takes about 110 milliseconds end to end, so the scan never ran once.
pub const CONTAINMENT_SCAN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// How long a terminal has to be quiet before its screen is read for what the agent is
/// doing.
///
/// Armed by output and disarmed by the read, so a terminal that says nothing schedules
/// nothing: an idle workspace costs no wakeups at all. More output pushes the deadline out,
/// which is what makes a burst one evaluation rather than one per read, and what makes the
/// screen that gets read the settled one with the prompt on it rather than a screen halfway
/// through being drawn.
pub const DETECT_SETTLE: std::time::Duration = std::time::Duration::from_millis(80);

#[allow(clippy::too_many_arguments)]
async fn tear_down(
    afd: &AsyncFd<MasterIo>,
    child: &mut std::process::Child,
    child_pid: i32,
    tty_dev: u64,
    containment: &mut Containment,
    engine: &mut AlacrittyEngine,
    updates: &watch::Sender<TerminalUpdate>,
    scanner: &ContainmentScanner,
) {
    let started = std::time::Instant::now();
    // One last look, which finds nothing if the leader has already gone: POSIX revokes the
    // controlling terminal when the controlling process terminates. Everything this set
    // contains was captured while the child was alive, which is why it is maintained during
    // the terminal's life rather than built here. Building it here was the first version,
    // and it contained nothing at all whenever the user simply typed `exit`.
    // The last look, from the shared scan rather than a fresh one: after the leader dies
    // the kernel has already revoked the association, so a fresh pass would find nothing
    // anyway, and the scan is not this task's to run.
    containment.absorb(scanner.members(tty_dev));
    let opaque = scanner.opaque();
    if opaque > 0 {
        eprintln!(
            "qorod: teardown for pid {child_pid}: {opaque} processes could not be inspected \
             and may still hold this terminal"
        );
    }

    let mut sink = vec![0u8; 64 * 1024];
    // What the child said on the way out. Bounded, and it is the end that is kept, because
    // the end is what a person reads.
    let mut tail: Vec<u8> = Vec::new();
    let mut dropped = 0usize;
    let mut round = 0usize;
    loop {
        let reaped = matches!(child.try_wait(), Ok(Some(_)));
        let step = teardown::next_step(
            round,
            std::time::Instant::now(),
            started,
            reaped,
            containment,
            &qoro_pty::sys::still_alive,
        );
        match step {
            Step::CloseMaster => break,
            Step::Failed { reason } => {
                // Reported, never silently ignored. An orphaned tree is a real outcome and
                // the operator has to be able to see it.
                eprintln!("qorod: teardown for pid {child_pid} failed: {reason}");
                break;
            }
            Step::SignalGroups { signal, groups } => {
                containment.absorb(scanner.members(tty_dev));
                for g in groups {
                    let _ = qoro_pty::sys::signal_group(g, signal);
                }
                round += 1;
                drain_for(afd, &mut sink, teardown::GRACE, &mut tail, &mut dropped).await;
            }
            Step::Reap => {
                drain_for(
                    afd,
                    &mut sink,
                    std::time::Duration::from_millis(50),
                    &mut tail,
                    &mut dropped,
                )
                .await;
            }
        }
    }
    let _ = child.try_wait();

    // ADR-014 step 4: what was read during teardown belongs on the screen. Draining into a
    // sink satisfies the kernel, which is why the child becomes reapable, and loses exactly
    // the output the drain exists to preserve. A terminal closed on request, rather than by
    // the child exiting, prints its farewell during teardown and nowhere else.
    if !tail.is_empty() {
        let (_damage, _effects) = engine.feed(&tail);
        let _ = updates.send(TerminalUpdate::Full(engine.snapshot()));
    }
    if dropped > 0 {
        eprintln!(
            "qorod: teardown for pid {child_pid} kept the last {} bytes and dropped {dropped}",
            tail.len()
        );
    }
}

/// Drain the master for a window, feeding what arrives to `sink`.
///
/// The window is waited out whatever the descriptor says. A master whose slave descriptors
/// are all closed is permanently readable and every read returns zero, which is exactly the
/// state teardown is normally entered from, so returning on end of stream collapsed every
/// grace period to microseconds: measured, SIGHUP, SIGTERM and SIGKILL were delivered
/// 1.9 milliseconds apart and a job's SIGTERM handler was killed 372 microseconds into its
/// cleanup. The same defect turned the post escalation reap into a spin of roughly 1.8
/// million iterations a second against the deadline.
async fn drain_for(
    afd: &AsyncFd<MasterIo>,
    sink: &mut [u8],
    window: std::time::Duration,
    into: &mut Vec<u8>,
    dropped: &mut usize,
) {
    let deadline = tokio::time::Instant::now() + window;
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return;
        }
        let remaining = deadline - now;
        let ready = match tokio::time::timeout(remaining, afd.readable()).await {
            Ok(Ok(guard)) => guard,
            Ok(Err(_)) => return,
            Err(_) => return,
        };
        let mut guard = ready;
        match guard.try_io(|inner| read_fd(inner.get_ref().as_raw_fd(), sink)) {
            Ok(Ok(0)) | Ok(Err(_)) => {
                // End of stream, or an error we cannot act on. The window still has to be
                // waited out, so sleep the rest of it rather than returning.
                tokio::time::sleep_until(deadline).await;
                return;
            }
            Ok(Ok(n)) => {
                // Kept, not discarded. This is the child's last output, which is the whole
                // reason teardown drains rather than closing the master.
                *dropped += qoro_pty::teardown::retain_tail(into, &sink[..n]);
            }
            Err(_would_block) => continue,
        }
    }
}

fn read_fd(fd: std::os::fd::RawFd, buf: &mut [u8]) -> io::Result<usize> {
    // SAFETY: fd is live and buf is a valid writable slice.
    let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

fn write_fd(fd: std::os::fd::RawFd, buf: &[u8]) -> io::Result<usize> {
    // SAFETY: fd is live and buf is a valid readable slice.
    let n = unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len()) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}
