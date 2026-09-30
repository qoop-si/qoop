//! The daemon: one socket, many terminals, real PTYs.
//!
//! Focus lives on the connection, never in here: each client subscribes the
//! terminals it draws and types at the stream it addresses, so two clients
//! hold different terminals without a shared focus to diverge. There is no
//! daemon-global focused terminal to move, which is what keeps one client's
//! viewport from ever following another client's navigation.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
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
/// The image protocols the sniffer catches, re-exported where the wire and the
/// client need them without depending on the terminal crate directly.
pub use qoro_terminal::ImageProtocol;
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
    /// Publish the current screen, for a subscriber that arrived after the output.
    ///
    /// Output is published only while somebody watches (the idle-cost rule), so a
    /// terminal that worked with no subscribers holds a stale published screen.
    /// A new subscription asks for the live one rather than showing the stale one
    /// until the next output happens to arrive. Internal only: it carries no bytes
    /// for the child and takes none, so it is not a write path.
    Refresh,
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
    /// A completed image envelope (Kitty or iTerm2), verbatim. Sent unthrottled the
    /// moment it completes: pictures are rare, and holding one for the next snapshot
    /// would put it on the wrong screen. The render lane keeps only the newest per
    /// terminal, like snapshots, so a burst of images costs one envelope, not all.
    Image(StoredImage),
}

/// One image a terminal's child emitted, kept beside the grid.
///
/// Last one wins per terminal. The grid never holds it (the engine strips image
/// envelopes before parsing), so this is the only place it lives until a client draws
/// it. Bounded by the sniffer's own cap: at most a few megabytes per terminal.
#[derive(Clone, Debug)]
pub struct StoredImage {
    pub terminal: u32,
    /// Monotonic per terminal, from the sniffer. What a client deletes by.
    pub id: u64,
    pub protocol: qoro_terminal::ImageProtocol,
    /// The whole envelope, opening `ESC` to terminator, verbatim.
    pub bytes: Vec<u8>,
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

/// How many pictures per terminal may publish inside `FLOOD_WINDOW`.
///
/// Pictures bypass the snapshot throttle because holding one puts it on the wrong
/// screen, so a runaway child emitting them in a loop needs its own bound. Eight a
/// second is orders above any legitimate emitter (an agent pastes one picture, then
/// stops to talk about it) and far below line rate.
pub const MAX_IMAGES_PER_WINDOW: usize = 8;
/// The rolling window the flood guard counts in.
pub const FLOOD_WINDOW: std::time::Duration = std::time::Duration::from_secs(1);

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
pub struct ConnectionRegistry {
    inner: std::sync::Mutex<Vec<Connection>>,
    /// `qoro focus`, fanned out to every connection's event task. Bounded and lossy on
    /// purpose: a focus nobody drained in time is a click the person has moved past.
    focus: tokio::sync::broadcast::Sender<u32>,
    /// Rung when a connection comes, is welcomed or goes: what the daemon's leaving waits
    /// on, rather than a timer.
    changed: tokio::sync::Notify,
    /// Set, under `inner`'s lock, once the daemon has decided to go: nothing registers
    /// after it, so no connection is let in and then left behind by the exit.
    closed: std::sync::atomic::AtomicBool,
}

impl Default for ConnectionRegistry {
    fn default() -> Self {
        Self {
            inner: std::sync::Mutex::default(),
            focus: tokio::sync::broadcast::channel(16).0,
            changed: tokio::sync::Notify::new(),
            closed: std::sync::atomic::AtomicBool::new(false),
        }
    }
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
    /// The peer process, from the kernel rather than from anything it said.
    pid: Option<u32>,
    /// Whether the peer introduced itself as an interface, the one kind of client that
    /// shows a terminal to a person and so the one `qoro focus` is for.
    interface: std::sync::atomic::AtomicBool,
    /// The terminals this connection last said are news for its person (`News`).
    news: std::sync::Mutex<BTreeSet<u32>>,
    /// Whether its hello was answered with a welcome. Until then it is nobody yet.
    welcomed: std::sync::atomic::AtomicBool,
    /// Whether it said it only watches: a daemon with nothing awake leaves with it open.
    observer: std::sync::atomic::AtomicBool,
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

    /// Whether it was let in: not once the daemon is leaving.
    fn register(&self, connection: Connection) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if self.closed.load(Ordering::SeqCst) {
            return false;
        }
        inner.push(connection);
        drop(inner);
        self.changed.notify_one();
        true
    }

    /// Close to newcomers if nobody is here but watchers, in one step with `register`, so
    /// a connection is either counted here or refused there.
    fn close_if_unheld(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        let unheld = inner
            .iter()
            .all(|c| c.welcomed.load(Ordering::Relaxed) && c.observer.load(Ordering::Relaxed));
        if unheld {
            self.closed.store(true, Ordering::SeqCst);
        }
        unheld
    }

    /// A welcomed peer, and whether it only watches.
    fn mark_welcomed(&self, client: &ClientId, observer: bool) {
        for c in self.inner.lock().unwrap().iter() {
            if &c.client == client {
                c.observer.store(observer, Ordering::Relaxed);
                c.welcomed.store(true, Ordering::Relaxed);
            }
        }
        self.changed.notify_one();
    }

    /// Connections that keep a daemon with nothing awake from leaving: welcomed, and not
    /// only watching. An older client never says it only watches, so it counts.
    pub fn holders(&self) -> usize {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.welcomed.load(Ordering::Relaxed) && !c.observer.load(Ordering::Relaxed))
            .count()
    }

    /// Connections accepted whose hello has not been answered yet.
    pub fn pending(&self) -> usize {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .filter(|c| !c.welcomed.load(Ordering::Relaxed))
            .count()
    }

    fn mark_interface(&self, client: &ClientId) {
        for c in self.inner.lock().unwrap().iter() {
            if &c.client == client {
                c.interface.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Ask every attached interface to show `terminal`, and say which processes those are.
    pub fn focus(&self, terminal: u32) -> Vec<u32> {
        // No receiver is not an error: nothing is attached, and the empty answer says so.
        let _ = self.focus.send(terminal);
        self.inner
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.interface.load(Ordering::Relaxed))
            .filter_map(|c| c.pid)
            .collect()
    }

    /// Take a connection out, and say which terminals it held as news: their rows change.
    fn remove(&self, client: &ClientId) -> Vec<u32> {
        let mut inner = self.inner.lock().unwrap();
        let news = inner
            .iter()
            .filter(|c| &c.client == client)
            .flat_map(|c| c.news.lock().unwrap().clone())
            .collect();
        inner.retain(|c| &c.client != client);
        drop(inner);
        self.changed.notify_one();
        news
    }

    /// Replace what one connection counts as news, and say which terminals moved.
    fn set_news(&self, client: &ClientId, terminals: Vec<u32>) -> Vec<u32> {
        let wanted: BTreeSet<u32> = terminals.into_iter().collect();
        let inner = self.inner.lock().unwrap();
        let Some(connection) = inner.iter().find(|c| &c.client == client) else {
            return Vec::new();
        };
        let mut held = connection.news.lock().unwrap();
        let moved = held.symmetric_difference(&wanted).copied().collect();
        *held = wanted;
        moved
    }

    /// Every terminal some connection counts as news.
    pub fn news(&self) -> BTreeSet<u32> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .flat_map(|c| c.news.lock().unwrap().clone())
            .collect()
    }

    fn contains(&self, client: &ClientId) -> bool {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .any(|c| &c.client == client)
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
    /// What a person called this terminal, when they were asked.
    ///
    /// Held here and not in the store, because a terminal is a pty this daemon owns: when
    /// the daemon goes, so does the terminal, and a name that outlived its terminal would
    /// be a row in a table nothing can ever point at again.
    pub name: Option<String>,
    /// The agent's own id for its conversation, last reported by hook.
    /// Memory only, like the name: the store keeps a copy for resume, but
    /// the index reads this so every poll stays off the disk.
    pub session: Option<String>,
    /// The agent generation whose own hook reported `session`. `None` when the id came
    /// from the store on a restore: carried over, not yet said by the agent running now,
    /// which may be a different one.
    pub session_generation: Option<u64>,
    /// Where this terminal is, last known. Written by the terminal's own
    /// task whenever its throttled read sees a change, read by the index,
    /// so a repaint never waits on the store and never syscalls. Lags the
    /// shell by the containment scan interval at most, which is the bound
    /// the row documents rather than a promise of now.
    pub cwd: Option<String>,
    pub id: TerminalId,
    /// How many rows of this terminal have gone behind the screen and been written down.
    ///
    /// Stamped onto every snapshot, so a client that is reading back through them can keep
    /// its place while the terminal keeps printing. Read from here rather than asked for,
    /// because asking would be a round trip per frame.
    pub history: Arc<std::sync::atomic::AtomicU64>,
    /// The token this terminal's child was launched with. Held so that a later observation
    /// can be checked against it without asking the child.
    pub token: qoro_domain::CapabilityToken,
    pub commands: mpsc::Sender<ClientCommand>,
    pub updates: watch::Sender<TerminalUpdate>,
    /// The newest image this terminal's child emitted, if any. Written by the
    /// terminal's own task, read when a client subscribes late: the watch channel
    /// holds only the latest update, so a picture published while nobody watched
    /// would otherwise be gone before the first viewer arrives.
    pub last_image: Arc<std::sync::Mutex<Option<StoredImage>>>,
    /// Pictures dropped by the per-terminal flood guard. Counted, like every
    /// other discard: a missing picture must be sayable.
    pub image_flood_dropped: Arc<std::sync::atomic::AtomicU64>,
    /// What the engine has discarded, as of its last read that changed it. Written by the
    /// terminal's own task, which owns the engine, and read by the index.
    pub discards: Arc<std::sync::Mutex<qoro_terminal::Discards>>,
    /// The live pty size, packed cols in the high half and rows in the low half.
    /// Written on resize, read at teardown: when the last update was a picture
    /// (which carries no grid), the end still carries a blank screen of the
    /// right geometry rather than a guessed one.
    pub last_dims: Arc<std::sync::atomic::AtomicU64>,
    /// Which life of this terminal number this is. A number comes back when a tab is
    /// restored in the same daemon (a woken space), and a subscription made for the
    /// earlier life must not be taken for one to the new.
    pub incarnation: u64,
}

/// A fresh [`TerminalHandle::incarnation`], never repeated within a daemon.
fn next_incarnation() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// A connection's render subscriptions: the terminal life each was made for, and its task.
type Subscriptions = Arc<std::sync::Mutex<BTreeMap<u32, (u64, tokio::task::AbortHandle)>>>;

/// The daemon's terminals, addressed by the number that travels in a frame's `stream`
/// field. Stream 0 is reserved and is never a terminal, so a decoder that defaults a
/// missing field to zero fails loudly instead of writing into the first terminal.
#[derive(Default)]
pub struct TerminalRegistry {
    inner: std::sync::Mutex<BTreeMap<u32, TerminalHandle>>,
    next: AtomicU64,
    /// Each tab's split tree, as the clients recorded it. Kept here as well as on disk so
    /// an index can carry them without a read per listing.
    splits: std::sync::Mutex<Vec<crate::control::Pane>>,
    /// Held across a split change and its write, so two clients changing splits at
    /// once cannot land on disk in the other order from memory.
    split_write: tokio::sync::Mutex<()>,
}

impl TerminalRegistry {
    pub fn get(&self, stream: u32) -> Option<TerminalHandle> {
        self.inner.lock().unwrap().get(&stream).cloned()
    }

    /// The workspace a terminal belongs to, for an index that groups by one.
    pub fn workspace_of(&self, stream: u32) -> Option<i64> {
        self.inner.lock().unwrap().get(&stream).map(|h| h.workspace)
    }

    pub fn name_of(&self, stream: u32) -> Option<String> {
        self.inner.lock().unwrap().get(&stream)?.name.clone()
    }

    /// The agent's conversation id, last reported by hook, if any.
    pub fn session_of(&self, stream: u32) -> Option<String> {
        self.inner.lock().unwrap().get(&stream)?.session.clone()
    }

    /// Where the terminal is, last known to its own task, if any.
    pub fn cwd_of(&self, stream: u32) -> Option<String> {
        self.inner.lock().unwrap().get(&stream)?.cwd.clone()
    }

    /// Remember where the terminal is. Called by the terminal's own task,
    /// which is the single writer, so values arrive in the order they were
    /// read. False when there is no such terminal.
    fn set_cwd(&self, stream: u32, cwd: Option<String>) -> bool {
        match self.inner.lock().unwrap().get_mut(&stream) {
            Some(handle) => {
                handle.cwd = cwd;
                true
            }
            None => false,
        }
    }

    /// Remember a conversation id, and the agent generation that reported it (`None`
    /// for one read back from the store). False when there is no such terminal.
    fn set_session(&self, stream: u32, session: Option<String>, generation: Option<u64>) -> bool {
        match self.inner.lock().unwrap().get_mut(&stream) {
            Some(handle) => {
                handle.session = session;
                handle.session_generation = generation;
                true
            }
            None => false,
        }
    }

    /// Give a terminal a different name. False when there is no such terminal.
    fn rename(&self, stream: u32, name: Option<String>) -> bool {
        match self.inner.lock().unwrap().get_mut(&stream) {
            Some(handle) => {
                handle.name = name;
                true
            }
            None => false,
        }
    }

    pub fn streams(&self) -> Vec<u32> {
        self.inner.lock().unwrap().keys().copied().collect()
    }

    /// The split trees cut down to the terminals alive now, and only those with two or
    /// more left: a tree of one is a tab, not a split.
    pub fn live_splits(&self, live: &[u32]) -> Vec<crate::control::Pane> {
        self.splits
            .lock()
            .unwrap()
            .iter()
            .filter_map(|tree| tree.retain(&|t| live.contains(&t)))
            .filter(|tree| tree.leaves().len() > 1)
            .collect()
    }

    /// Replace every split tree at once.
    pub fn load_splits(&self, trees: Vec<crate::control::Pane>) {
        *self.splits.lock().unwrap() = trees;
    }

    /// Record one tab's tree and write the whole set down, answering once it is durable.
    ///
    /// Every tree sharing a terminal with this one goes first: that is how a tree moves
    /// under a new first pane, and a single leaf clears the tree that held it.
    pub async fn set_split(
        &self,
        tree: crate::control::Pane,
        store: &crate::store::StoreHandle,
    ) -> Result<(), crate::store::HandleError> {
        let _order = self.split_write.lock().await;
        let trees = {
            let mut splits = self.splits.lock().unwrap();
            let leaves = tree.leaves();
            splits.retain(|kept| !kept.leaves().iter().any(|t| leaves.contains(t)));
            if leaves.len() > 1 {
                splits.push(tree);
            }
            splits.clone()
        };
        store.set_splits(splits_json(&trees)).await
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
    /// The next terminal id. Minted by the caller rather than inside `open_terminal`,
    /// because the durable row is written under this number before the pty exists: one
    /// number for one terminal, in the registry and in the database.
    pub fn mint(&self) -> u32 {
        self.next.fetch_add(1, Ordering::Relaxed) as u32 + 1
    }

    /// Carry on minting above the ids a restart brought back.
    ///
    /// Without this the first new terminal after a restore takes the number of a terminal
    /// that is already on screen, and both rows point at one tab.
    pub fn mint_above(&self, id: u32) {
        self.next.fetch_max(id as u64, Ordering::Relaxed);
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
    /// Which terminals live connections currently watch. The idle reaper
    /// never closes a watched tab.
    watched: crate::idle::WatchCounts,
    /// What a terminal ending writes down, and which spaces are asleep, shared with every
    /// terminal and connection.
    books: Bookkeeping,
    /// Whether this daemon leaves when nothing is awake and nobody is attached. Set by the
    /// binary that runs one on its own (`qoro daemon`); a daemon embedded in something
    /// else, a test, stays until it is dropped.
    leaves: bool,
    /// What the last restart brought back, counted by kind. Kept so a gate, a
    /// client, or a startup line can read what survived without reopening tabs.
    restore_report: RestoreReport,
}

impl Daemon {
    /// Bind and restore what was awake. The caller must already hold the
    /// startup lock (ADR-012), which is what makes the stale-socket removal below safe.
    pub async fn bind(dir: &Path, shell: &Path) -> io::Result<Self> {
        let socket = paths::socket_path(dir);
        remove_stale_socket(&socket)?;
        let listener = UnixListener::bind(&socket)?;
        // Restricted right after bind. Between the two the socket has default permissions,
        // and what protects it then is the verified 0700 directory around it: nobody else
        // can reach a path inside it. A umask around the bind would race every other thread.
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
        // The idempotency ledger keeps a window (ADR-017), trimmed once a start: it grows by
        // what people and agents asked for, and without this it grew for as long as qoro was
        // installed. A retry is seconds behind the answer it missed, so the window is about
        // forensics on a reap, not about the retry.
        let _ = store.prune_applied().await;
        let agents = Arc::new(crate::agents::AgentRegistry::new()?);
        let scanner = Arc::new(ContainmentScanner::default());
        // One pass before the first terminal exists, so a terminal created immediately has
        // a set to absorb rather than an empty map.
        scanner.refresh().await;
        let stopping = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let books = Bookkeeping::new(store.clone(), stopping.clone());
        // Which spaces are asleep, read before anything is restored: their tabs stay down,
        // and their rows must keep their place from the first teardown on.
        match store.asleep().await {
            Ok(rows) => books
                .asleep
                .lock()
                .unwrap()
                .extend(rows.into_iter().map(|(workspace, _)| workspace)),
            // Not known, so every space is taken as asleep: nothing is restored, nothing is
            // lost, and a wake brings each back. Restoring them all instead would bring back
            // a sleeping space under its mark, where no agent write lands.
            Err(e) => {
                eprintln!("qorod: which spaces are asleep could not be read: {e}");
                if let Ok(spaces) = store.workspaces().await {
                    books
                        .asleep
                        .lock()
                        .unwrap()
                        .extend(spaces.into_iter().map(|space| space.id));
                }
            }
        }
        // Who is in each terminal, written down as it changes.
        //
        // Hung off the registry's own announcements rather than off the terminal's output,
        // and that is the whole of the fix for a restore that brought back every tab as a
        // bare shell. An agent is usually not known at the moment its process takes the
        // foreground: it is behind a runtime or a wrapper, and the identity is settled a
        // moment later by a probe. Written from the read path, which only fires when the
        // foreground leader changes, that later promotion was never seen and the column
        // stayed empty for every tab. Measured against a real Claude Code: the panel said
        // `claude` and the row said nothing.
        //
        // Armed before anything is restored, so an agent that comes back is recorded the
        // first time it is recognised rather than the first time it changes.
        spawn_agent_recorder(agents.clone(), terminals.clone(), books.clone());

        // What the last daemon was holding when it stopped. A person who closed qoro and
        // opened it again asked for their work back, not for an empty screen.
        let report = restore_report(&Restore {
            terminals: terminals.clone(),
            books: books.clone(),
            shell: shell.to_path_buf(),
            warnings: warnings.clone(),
            history_dir: Some(dir.join("history")),
            scanner: scanner.clone(),
            agents: agents.clone(),
        })
        .await;
        restore_splits(&terminals, &books).await;
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
            books,
            leaves: false,
            watched: crate::idle::WatchCounts::default(),
            restore_report: report,
        })
    }

    /// What the last restart brought back, counted by kind.
    ///
    /// A gate reads this rather than reopening tabs: the report is written once at
    /// bind, from the same rows the restore read.
    pub fn restore_report(&self) -> RestoreReport {
        self.restore_report.clone()
    }

    /// The durable metadata, so a gate can read what the daemon wrote rather than guess.
    pub fn store(&self) -> crate::store::StoreHandle {
        self.store.clone()
    }

    /// The workspace new terminals are recorded in.
    pub fn workspace(&self) -> i64 {
        self.workspace
    }

    /// Leave when nothing is awake and nobody is attached, as `qoro daemon` does (see
    /// `leave_when_idle`). Off unless asked for, because leaving is `process::exit`.
    pub fn leaving_when_idle(mut self) -> Self {
        self.leaves = true;
        self
    }

    /// Open one terminal in the default workspace, where the daemon is, before serving.
    ///
    /// A bind no longer opens one by itself: a daemon with nothing awake is one that is
    /// about to leave, and the client that started it says what it wants (`qoro` asks
    /// for a shell where the person is). This is for an embedder with no client, which
    /// is the tests.
    pub async fn open_first_terminal(&self) -> io::Result<u32> {
        let stream = self.terminals.mint();
        self.store
            .create_terminal(
                stream as i64,
                self.workspace,
                here().display().to_string(),
                self.shell.display().to_string(),
            )
            .await
            .map_err(io::Error::other)?;
        open_terminal(
            &self.terminals,
            stream,
            self.workspace,
            None,
            &self.shell,
            None,
            self.warnings.clone(),
            self.history_dir.clone(),
            self.scanner.clone(),
            self.agents.clone(),
            Some(self.books.clone()),
        )?;
        Ok(stream)
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
        if self.leaves {
            tokio::spawn(leave_when_idle(Leaving {
                terminals: self.terminals.clone(),
                connections: self.connections.clone(),
                agents: self.agents.clone(),
                books: self.books.clone(),
                socket: self.socket.clone(),
            }));
        }
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

        // The containment scan, once for the whole daemon. It only runs after a terminal
        // said something, so an idle daemon makes no syscalls at all, which is what gate
        // G2.2 asserts: the cost of being idle cannot depend on how many unrelated
        // processes the host happens to be running.
        let scanning = self.scanner.clone();
        tokio::spawn(async move {
            loop {
                // Parked until something happens, then rate limited. Nothing changed, so
                // nothing runs: a scan is a pass over the whole process table, and running
                // it on a timer regardless of activity made a hundred idle terminals cost
                // 1.09 seconds of CPU a minute against a 50 millisecond budget. A process
                // cannot join a terminal without that terminal saying something first.
                // No check for an empty terminal set here: skipping the scan also skipped
                // clearing the flag, and the loop woke every interval until a terminal
                // appeared. The last terminal's final output costs one scan, then this parks.
                scanning.wait_for_activity().await;
                tokio::time::sleep(CONTAINMENT_SCAN_INTERVAL).await;
                scanning.refresh().await;
            }
        });

        // Spaces that sleep by themselves. Spawned only when `[sleep] idle_minutes` is set:
        // off (the default) means no task and zero wakeups, which is what keeps ADR-009's
        // macOS literal-zero-idle claim intact. On, it parks while nothing is awake and
        // otherwise wakes on an adaptive tick to put to sleep a space idle past the time.
        if std::env::var_os(crate::idle::IDLE_TIMEOUT_ENV).is_some() {
            eprintln!(
                "qorod: {} is no longer read: idle spaces sleep after `[sleep] idle_minutes` in \
                 config.toml",
                crate::idle::IDLE_TIMEOUT_ENV
            );
        }
        let minutes = crate::config::read().idle_minutes;
        if minutes == 0 {
            eprintln!("qorod: idle sleep: off");
        } else {
            eprintln!("qorod: idle sleep: after {minutes}m");
            tokio::spawn(idle_sleep_loop(
                Restore {
                    terminals: self.terminals.clone(),
                    books: self.books.clone(),
                    shell: self.shell.clone(),
                    warnings: self.warnings.clone(),
                    history_dir: self.history_dir.clone(),
                    scanner: self.scanner.clone(),
                    agents: self.agents.clone(),
                },
                self.watched.clone(),
                std::time::Duration::from_secs(minutes.saturating_mul(60)),
            ));
        }

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
                    let touched = watched_agents.apply_events(crate::agents::now_unix());
                    ready.clear_ready();
                    settle_queue.mark(touched);
                }
            } else {
                let mut interval = qoro_pty::procwatch::PROBE_FAST;
                loop {
                    tokio::time::sleep(interval).await;
                    let touched = watched_agents.apply_events(crate::agents::now_unix());
                    let changed = !touched.is_empty();
                    // The probe platform re-reads every terminal, because it has no event
                    // to tell it which one moved.
                    settle_queue.mark(watched_agents.streams());
                    interval = qoro_pty::procwatch::next_probe(interval, changed);
                }
            }
        });

        // Ring the bridge's doorbell, if there is a bridge: "a session just started, look
        // again". A connect and a close, nothing written, errors ignored,
        // each on its own task so a wedged listener can never hold up the accept loop. Rung
        // here and not at bind, because bind is still restoring terminals and a watcher that
        // arrived then would wait in the backlog.
        if let Some(runtime) = self
            .socket
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
        {
            // The notch's watch needs no ring: it waits on the runtime directory itself.
            let bell = paths::bridge_socket(runtime);
            tokio::spawn(async move {
                // Held open until the listener closes it, so it can read our uid (macOS
                // cannot once the peer is gone). Nothing is written.
                let _ = tokio::time::timeout(std::time::Duration::from_secs(1), async {
                    if let Ok(mut s) = tokio::net::UnixStream::connect(bell).await {
                        let _ = tokio::io::AsyncReadExt::read(&mut s, &mut [0u8; 1]).await;
                    }
                })
                .await;
            });
        }

        let mut next_client = 0u64;
        loop {
            // One failed accept is one connection, not the daemon: `?` here ended `serve`
            // and took every terminal with it on the first EMFILE. A peer that gave up
            // mid-accept is skipped; anything else waits a beat so a persistent error
            // cannot spin.
            let stream = match self.listener.accept().await {
                Ok((stream, _)) => stream,
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::ConnectionAborted | io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                Err(e) => {
                    eprintln!("qorod: accept failed: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                }
            };
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
            let workspace = self.workspace;
            let served_history = self.history_dir.clone();
            let served_scanner = self.scanner.clone();
            let served_agents = self.agents.clone();
            let served_watched = self.watched.clone();
            let served_books = self.books.clone();
            // Registered before the task exists, so the task's own `remove` on exit always
            // finds it: registering after the spawn let a connection that closed at once
            // remove nothing and then stay listed, counted against the budget forever.
            // Refused once this daemon has decided to go: its client starts the next one.
            let admitted = self.connections.register(Connection {
                client,
                progress,
                shutdown,
                admitted: std::time::Instant::now(),
                pid: peer_pid(&stream),
                interface: std::sync::atomic::AtomicBool::new(false),
                news: std::sync::Mutex::default(),
                welcomed: std::sync::atomic::AtomicBool::new(false),
                observer: std::sync::atomic::AtomicBool::new(false),
            });
            let announce = self.agents.clone();
            if !admitted {
                drop(stream);
                continue;
            }
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
                        workspace,
                        history_dir: served_history,
                        scanner: served_scanner,
                        agents: served_agents,
                        watched: served_watched,
                        connections: registry.clone(),
                        books: served_books,
                    },
                )
                .await;
                if let Err(e) = result {
                    eprintln!("qorod: client ended: {e}");
                }
                // What it held as news is news no more: every index looks again.
                for terminal in registry.remove(&registered) {
                    announce.announce(terminal);
                }
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

/// The peer's pid, which is how `qoro focus` names the interfaces it moved: a launcher
/// raises the window the process runs in. `None` when the kernel will not say.
fn peer_pid(stream: &UnixStream) -> Option<u32> {
    let fd = stream.as_raw_fd();
    #[cfg(target_os = "macos")]
    {
        let mut pid: libc::pid_t = 0;
        let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
        // SAFETY: fd is a live connected unix socket; pid and len are valid out-params
        // and len is the size of pid.
        let rc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_LOCAL,
                libc::LOCAL_PEERPID,
                &mut pid as *mut _ as *mut libc::c_void,
                &mut len,
            )
        };
        (rc == 0 && pid > 0).then_some(pid as u32)
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
        (rc == 0 && cred.pid > 0).then_some(cred.pid as u32)
    }
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

/// The most warnings kept: plenty for a gate to find the one it caused, and a ceiling for a
/// daemon that runs for weeks with nothing in production reading them.
const MAX_WARNINGS: usize = 64;

/// Record a warning, dropping the oldest past `MAX_WARNINGS`.
fn warn(warnings: &std::sync::Mutex<Vec<Warning>>, warning: Warning) {
    let mut warnings = warnings.lock().unwrap();
    if warnings.len() >= MAX_WARNINGS {
        warnings.remove(0);
    }
    warnings.push(warning);
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
    /// One image per terminal at most, replaced by the next. Never coalesced with
    /// snapshots: a picture is not a screen, and dropping it as a stale screen would
    /// lose the only copy on its way to the client.
    images: std::sync::Mutex<BTreeMap<u32, StoredImage>>,
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
            images: std::sync::Mutex::new(BTreeMap::new()),
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

    /// Replace this terminal's pending image. Never awaits, like `put`: the caller
    /// is the subscription task, which must not block.
    pub fn put_image(&self, stream: u32, image: StoredImage) {
        let added = image.bytes.len();
        let removed = {
            let mut images = self.images.lock().unwrap();
            images
                .insert(stream, image)
                .as_ref()
                .map(|old| old.bytes.len())
                .unwrap_or(0)
        };
        self.charge(added as isize - removed as isize);
        self.notify.notify_one();
    }

    /// Take every pending image, oldest terminal first. Returns empty when none.
    pub fn take_images(&self) -> Vec<(u32, StoredImage)> {
        let taken: Vec<(u32, StoredImage)> = std::mem::take(&mut *self.images.lock().unwrap())
            .into_iter()
            .collect();
        let freed: usize = taken.iter().map(|(_, i)| i.bytes.len()).sum();
        self.charge(-(freed as isize));
        taken
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
        // Whatever is still held stops counting against the global ceiling, or a closed
        // connection would keep a share of the budget forever.
        let _ = self.take();
        let _ = self.take_images();
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
    pub workspace: i64,
    pub history_dir: Option<PathBuf>,
    pub scanner: Arc<ContainmentScanner>,
    pub agents: Arc<crate::agents::AgentRegistry>,
    /// Which terminals live connections watch. Incremented on subscribe,
    /// decremented on unsubscribe and disconnect; the idle reaper reads it.
    pub watched: crate::idle::WatchCounts,
    /// Every live connection, so a stop can wait for the one that asked to hang up.
    pub connections: Arc<ConnectionRegistry>,
    /// What a terminal ending writes down, and which spaces are asleep.
    pub books: Bookkeeping,
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
        workspace,
        history_dir,
        scanner,
        agents,
        watched,
        connections,
        books,
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

    // Handshake first, on channel 0, before anything else is accepted. Bounded like any
    // other message in flight: a peer that connects and says nothing held its socket and
    // this task forever, even after the connection budget evicted it.
    let handshake = tokio::time::timeout(qoro_proto::reassembly::SEQUENCE_DEADLINE, async {
        loop {
            let n = stream.read(&mut read).await?;
            if n == 0 {
                return Ok(None);
            }
            inbuf.extend_from_slice(&read[..n]);
            if let Some(frame) = Frame::decode(&mut inbuf).map_err(to_io)? {
                if frame.channel != Channel::Handshake {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "first frame must be a handshake",
                    ));
                }
                return serde_json::from_slice::<qoro_proto::ClientHello>(&frame.body)
                    .map(Some)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e));
            }
        }
    })
    .await;
    let hello = match handshake {
        Ok(Ok(Some(hello))) => hello,
        Ok(Ok(None)) => return Ok(()),
        Ok(Err(e)) => return Err(e),
        Err(_) => {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "no handshake within the deadline",
            ));
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
    // Known as an interface, and listening for `qoro focus`, before the welcome goes out:
    // a focus sent the moment the interface is in must neither miss it nor leave its pid
    // out of the answer.
    let mut focus = connections.focus.subscribe();
    if hello.client_id == "qoro-tui" && matches!(outcome, qoro_proto::HandshakeOutcome::Welcome(_))
    {
        connections.mark_interface(&client);
    }
    if matches!(outcome, qoro_proto::HandshakeOutcome::Welcome(_)) {
        let observer =
            hello.extra.get(crate::client::OBSERVER) == Some(&serde_json::Value::Bool(true));
        connections.mark_welcomed(&client, observer);
    }
    stream.write_all(&out).await?;
    if matches!(outcome, qoro_proto::HandshakeOutcome::Refuse(_)) {
        return Ok(());
    }

    let (mut rd, mut wr) = stream.into_split();
    let writer_progress = progress.clone();
    let renders = Arc::new(RenderQueue::new(outbound.clone()));
    let (control_tx, mut control_rx) = mpsc::channel::<BytesMut>(32);
    // Which history search of this connection may still run. Each one takes the next
    // number and stops at its next chunk once the number has moved on: a newer search from
    // the same person makes the older one moot, and a connection that is gone makes every
    // one of them moot, so neither keeps a blocking thread reading disk for nobody.
    let searching = Arc::new(AtomicU64::new(0));
    // Subscription tasks, one per terminal this connection is watching. Kept so that
    // unsubscribing, and ending the connection, actually stop them.
    let subscriptions: Subscriptions = Arc::default();

    // A connection attaches to the lowest numbered terminal by default, which is the whole
    // of what a client with no arguments wants and is what milestone 1 shipped. Everything
    // else is a subscription request on the control channel.
    if let Some(first) = terminals.streams().first().copied() {
        subscribe(&terminals, &subscriptions, &renders, &watched, first);
    }

    // News for this connection, on its own channel. One task per connection rather than a
    // branch inside the writer: the writer's loop is the render hot path, and a lagging
    // broadcast receiver there would make a slow client's problem into everyone's.
    let event_tx = control_tx.clone();
    let mut changes = agents.subscribe();
    let events_task = tokio::spawn(async move {
        loop {
            let event = tokio::select! {
                event = pump_agent_event(&mut changes) => event,
                // Matched here rather than in the branch pattern: a pattern that fails
                // disables its branch until an agent event ends the select, and on a quiet
                // session that is every later focus lost. Focuses missed while lagging are
                // clicks already past.
                shown = focus.recv() => match shown {
                    Ok(terminal) => Some(crate::control::EventMessage::Focus { terminal }),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
            };
            let Some(event) = event else {
                return;
            };
            let body = match serde_json::to_vec(&event) {
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
    let writer_terminals = terminals.clone();
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
                let mut wire = SnapshotWire::from(&snapshot);
                // What is behind this terminal's screen, as of now. The renderer has the
                // stream, and the registry has the count.
                wire.behind = writer_terminals
                    .get(stream)
                    .map(|h| h.history.load(std::sync::atomic::Ordering::Relaxed))
                    .unwrap_or(0);
                let body = match serde_json::to_vec(&wire) {
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

            // Pictures travel after the screens of the same batch, on the same
            // channel and stream, sharing its sequence numbers. A client that
            // predates images fails the ImageWire parse and skips them, which is
            // ADR-010's rule: a newer daemon may say more than an older client
            // understands.
            for (stream, image) in writer_renders.take_images() {
                let wire = ImageWire::from(&image);
                let body = match serde_json::to_vec(&wire) {
                    Ok(b) => b,
                    Err(_) => continue,
                };
                let counter = seq.entry(stream).or_default();
                let frames =
                    qoro_proto::chunk(Channel::Terminal, stream, *counter, None, Bytes::from(body));
                *counter = counter.wrapping_add(frames.len() as u32);
                if frames.iter().any(|f| f.encode(&mut buf).is_err()) {
                    return;
                }
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
            // The tick is what makes the deadline real, and it runs only while a sequence is
            // open: with nothing to expire, an idle connection (a phone's watcher, an
            // attached client) wakes the daemon for nothing once a second.
            let open = reassembler.pending_sequences() > 0;
            let n = tokio::select! {
                biased;
                _ = shutdown.notified() => return Ok(()),
                read_result = rd.read(&mut read) => read_result?,
                _ = tokio::time::sleep(SWEEP_INTERVAL), if open => {
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
                        // Spawned off this loop and answered through the connection's
                        // outbound channel, the way events are: a search step is a
                        // tenth of a second of disk, and the next keystroke on this
                        // connection must not wait behind it.
                        Ok(ControlRequest::SearchScrollback {
                            id,
                            terminal,
                            query,
                            from,
                            backward,
                        }) => {
                            let dir = history_of(&terminals, history_dir.as_ref(), terminal);
                            let step = searching.fetch_add(1, Ordering::SeqCst) + 1;
                            let searching = searching.clone();
                            let tx = control_tx.clone();
                            tokio::spawn(async move {
                                let response = match dir {
                                    Ok(dir) => {
                                        search_scrollback(
                                            id,
                                            terminal,
                                            query,
                                            from,
                                            backward,
                                            dir,
                                            move || searching.load(Ordering::Relaxed) != step,
                                        )
                                        .await
                                    }
                                    Err(reason) => ControlResponse::Failed { id, reason },
                                };
                                let Ok(body) = serde_json::to_vec(&response) else {
                                    return;
                                };
                                let mut out = BytesMut::new();
                                if Frame::new(Channel::Control, 0, Bytes::from(body))
                                    .encode(&mut out)
                                    .is_ok()
                                {
                                    let _ = tx.send(out).await;
                                }
                            });
                            continue;
                        }
                        // A sleep, a wake, or anything waiting on a space that is changing
                        // takes seconds: answered from a task of its own, like a search
                        // step, so typing into every other tab on this connection goes on.
                        Ok(request) => match dispatch(&request, &books, &terminals, workspace) {
                            Dispatch::Apart => {
                                let tx = control_tx.clone();
                                let terminals = terminals.clone();
                                let subscriptions = subscriptions.clone();
                                let renders = renders.clone();
                                let shell = shell.clone();
                                let warnings = warnings.clone();
                                let history_dir = history_dir.clone();
                                let client = client.clone();
                                let scanner = scanner.clone();
                                let agents = agents.clone();
                                let watched = watched.clone();
                                let connections = connections.clone();
                                let books = books.clone();
                                tokio::spawn(async move {
                                    let response = handle_control(
                                        request,
                                        &terminals,
                                        &subscriptions,
                                        &renders,
                                        &shell,
                                        &warnings,
                                        workspace,
                                        history_dir,
                                        &client,
                                        &scanner,
                                        &agents,
                                        &watched,
                                        &connections,
                                        &books,
                                        None,
                                    )
                                    .await;
                                    let Ok(body) = serde_json::to_vec(&response) else {
                                        return;
                                    };
                                    let mut out = BytesMut::new();
                                    if Frame::new(Channel::Control, 0, Bytes::from(body))
                                        .encode(&mut out)
                                        .is_ok()
                                    {
                                        let _ = tx.send(out).await;
                                    }
                                });
                                continue;
                            }
                            Dispatch::Inline(held) => {
                                handle_control(
                                    request,
                                    &terminals,
                                    &subscriptions,
                                    &renders,
                                    &shell,
                                    &warnings,
                                    workspace,
                                    history_dir.clone(),
                                    &client,
                                    &scanner,
                                    &agents,
                                    &watched,
                                    &connections,
                                    &books,
                                    held,
                                )
                                .await
                            }
                        },
                        Err(e) => ControlResponse::Failed {
                            // The typed request did not parse, but its JSON may still name
                            // an id: an op from a newer client is the usual case, and a
                            // refusal nobody can match is one every client has to guess at.
                            id: crate::control::raw_request_id(&message.body),
                            reason: unparsable(&e.to_string()),
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
                    ClientWire::Input {
                        bytes,
                        from_terminal,
                        from_token,
                    } => {
                        // A CLI send/prompt from inside one terminal to
                        // another records the wait: the waiter is vetoed
                        // while the target is busy. Verified by
                        // `verify_waiter`; anything unverified is ignored.
                        if let (Some(waiter), Some(token)) = (from_terminal, from_token)
                            && verify_waiter(&agents, &terminals, waiter, &token, message.stream)
                        {
                            agents.note_expectation(
                                waiter,
                                message.stream,
                                crate::agents::now_unix(),
                            );
                        }
                        ClientCommand::Input {
                            from: client.clone(),
                            bytes: Bytes::from(bytes),
                        }
                    }
                    ClientWire::Resize { cols, rows } => ClientCommand::Resize {
                        from: client.clone(),
                        writable,
                        cols,
                        rows,
                    },
                };
                // A terminal whose task has ended is one stale address, not the end of
                // this read: breaking here left every later frame of the read in the
                // buffer until the peer happened to send more.
                if handle.commands.send(cmd).await.is_err() {
                    continue;
                }
            }
        }
        Ok(())
    }
    .await;

    // One exit. Aborting the writer is what drops the write half and closes the socket, so
    // it has to happen however the loop ended.
    events_task.abort();
    searching.fetch_add(1, Ordering::SeqCst);
    renders.close();
    // This connection stops watching: drop its subscriptions from the
    // daemon-wide watch counts the idle reaper reads, and stop the tasks.
    let held = std::mem::take(&mut *subscriptions.lock().unwrap());
    watched.unwatch_all(&held.keys().copied().collect());
    for (_, (_, task)) in held {
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
///
/// A new subscription also asks the terminal to publish its live screen. Output is
/// published only while watched, so without this a subscriber that arrives after a quiet
/// spell of work would sit on the last published screen, which can be the blank one from
/// terminal birth, until something else happens to print.
fn subscribe(
    terminals: &Arc<TerminalRegistry>,
    subscriptions: &Subscriptions,
    renders: &Arc<RenderQueue>,
    watched: &crate::idle::WatchCounts,
    stream: u32,
) -> bool {
    let Some(handle) = terminals.get(stream) else {
        return false;
    };
    {
        let mut held = subscriptions.lock().unwrap();
        match held.get(&stream) {
            Some((life, task)) if *life == handle.incarnation && !task.is_finished() => {
                // Subscribing twice is not an error and must not double the traffic: no
                // second task. It still asks for the live screen, once, because a client
                // that subscribes wants one, and one that had dropped its copy would
                // otherwise wait on a quiet terminal for a screen that never comes.
                drop(held);
                let _ = handle.commands.try_send(ClientCommand::Refresh);
                return true;
            }
            Some(_) => {
                // Made for an earlier life of this number, or already over: kept, it would
                // answer for the new terminal with a task nobody feeds, and the pane would
                // stay blank. Dropped, and its watch count with it.
                if let Some((_, task)) = held.remove(&stream) {
                    task.abort();
                    watched.unwatch(stream);
                }
            }
            None => {}
        }
    }
    let mut updates = handle.updates.subscribe();
    let queue = renders.clone();
    // A picture published while nobody watched lives on in the handle, so a late
    // subscriber still sees it. Seeded before the loop, replaced by anything newer.
    // A duplicate delivery is harmless: the client draws by image id.
    let last_image = handle.last_image.clone();
    let mut sent_image: Option<u64> = None;
    if let Some(image) = last_image.lock().unwrap().clone() {
        sent_image = Some(image.id);
        queue.put_image(stream, image);
    }
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
                TerminalUpdate::Image(image) => {
                    sent_image = Some(image.id);
                    queue.put_image(stream, image);
                }
            }
            // Heal the watch channel's latest-wins loss: an Image overwritten by a
            // Full between two wakes never reaches the match above, but it is still
            // in the handle. Forwarded here, at most once per picture.
            if let Some(image) = last_image.lock().unwrap().clone()
                && sent_image != Some(image.id)
            {
                sent_image = Some(image.id);
                queue.put_image(stream, image);
            }
            if updates.changed().await.is_err() {
                return;
            }
        }
    });
    if let Some((_, older)) = subscriptions
        .lock()
        .unwrap()
        .insert(stream, (handle.incarnation, task.abort_handle()))
    {
        // Another subscribe landed between the check above and here: one task per
        // terminal, and the count it added goes with it.
        older.abort();
        watched.unwatch(stream);
    }
    // A new watcher for the daemon-wide counts the idle reaper reads: a
    // watched tab is never reaped while somebody looks at it.
    watched.watch(stream);
    // The borrowed screen above can predate the subscription: anything the terminal
    // printed while nobody watched was never published. Ask for the live one; the
    // render lane keeps only the newest per terminal, so this supersedes the stale
    // borrow rather than doubling the traffic.
    let _ = handle.commands.try_send(ClientCommand::Refresh);
    true
}

/// Conversation titles, read out of the agent's own storage: opencode's
/// database for `ses_` ids, Claude's transcript for everything else.
///
/// Best effort and cached: the hook already reports the session id, and the
/// title sits in the agent's storage next to that id, so the panel can name
/// the conversation the way the agent does. A rename arrives on its own
/// because entries go stale after a minute. Anything unreadable (no database,
/// no transcript, unknown id, locked) reads as absent, never as an error: the
/// panel falls back to the shortened id.
fn agent_title(session: Option<&str>) -> Option<String> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};
    type Titles = HashMap<String, (Option<String>, Instant)>;
    static CACHE: OnceLock<Mutex<Titles>> = OnceLock::new();
    const TTL: Duration = Duration::from_secs(60);
    let id = session.filter(|s| !s.trim().is_empty())?;
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(cache) = cache.lock()
        && let Some((title, at)) = cache.get(id)
        && at.elapsed() < TTL
    {
        return title.clone();
    }
    let title = if id.starts_with("ses_") {
        opencode_session_title(id)
    } else {
        claude_session_title(id)
    };
    if let Ok(mut cache) = cache.lock() {
        // Bounded: entries only refresh, so evict the stale tail past this.
        if cache.len() > 512 {
            cache.retain(|_, (_, at)| at.elapsed() < TTL);
        }
        cache.insert(id.to_string(), (title.clone(), Instant::now()));
    }
    title
}

/// One title out of opencode's database, or nothing.
fn opencode_session_title(id: &str) -> Option<String> {
    opencode_db_path().and_then(|db| opencode_session_title_at(&db, id))
}

/// Where opencode keeps its database, when this machine has one.
///
/// XDG first, then the plain home layout this machine's own opencode uses,
/// then the macOS application-support layout other installs use. First hit
/// wins; nothing found reads as absent downstream.
fn opencode_db_path() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        candidates.push(PathBuf::from(xdg).join("opencode/opencode.db"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(&home).join(".local/share/opencode/opencode.db"));
        candidates
            .push(PathBuf::from(&home).join("Library/Application Support/OpenCode/opencode.db"));
        candidates
            .push(PathBuf::from(&home).join("Library/Application Support/opencode/opencode.db"));
    }
    candidates.into_iter().find(|db| db.is_file())
}

/// One title out of the database at this path, or nothing. The id is
/// opencode's own session key, so no directory matching is needed; a first
/// line, trimmed, so a stored newline never reaches a thirty column row.
///
/// Synchronous on the control path on purpose: it runs at most once per
/// session per minute (the cache above), and a single indexed primary-key
/// read is cheaper than a thread hop.
fn opencode_session_title_at(db: &Path, id: &str) -> Option<String> {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    let title: Option<String> = conn
        .query_row("SELECT title FROM session WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .ok()?;
    title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .and_then(|t| t.lines().next())
        .map(str::to_string)
}

/// One title out of Claude's transcript for this session id, or nothing.
fn claude_session_title(id: &str) -> Option<String> {
    claude_transcript(id).and_then(|path| claude_session_title_at(&path))
}

/// `<projects>/<slug>/<id>.jsonl`: the id is the file name, so one stat per
/// project directory finds it. `CLAUDE_CONFIG_DIR` first, as Claude reads it.
/// An id that could climb out of the directory is refused up front.
fn claude_transcript(id: &str) -> Option<PathBuf> {
    if id.len() > 128 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }
    let mut roots = Vec::new();
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        roots.push(PathBuf::from(dir).join("projects"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".claude/projects"));
    }
    let file = format!("{id}.jsonl");
    roots
        .iter()
        .filter_map(|root| std::fs::read_dir(root).ok())
        .flat_map(|dirs| dirs.flatten())
        .map(|slug| slug.path().join(&file))
        .find(|path| path.is_file())
}

/// The title Claude last wrote into this transcript: a `/rename` title
/// (`custom-title`) over the generated one (`ai-title`). Claude re-appends
/// both every few turns, so the tail carries the current pair and a
/// transcript of any size costs one bounded read.
///
/// ponytail: tail window only; a session whose last title sits further back
/// than this reads as untitled until Claude writes the pair again.
fn claude_session_title_at(path: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL: u64 = 1 << 20;
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut tail = Vec::new();
    file.take(TAIL).read_to_end(&mut tail).ok()?;
    let tail = String::from_utf8_lossy(&tail);
    let (mut custom, mut generated) = (None, None);
    for line in tail.lines().rev() {
        if custom.is_some() {
            break;
        }
        if !line.contains("-title\"") {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let (slot, key) = match value.get("type").and_then(|t| t.as_str()) {
            Some("custom-title") => (&mut custom, "customTitle"),
            Some("ai-title") if generated.is_none() => (&mut generated, "aiTitle"),
            _ => continue,
        };
        *slot = value
            .get(key)
            .and_then(|t| t.as_str())
            .map(str::trim)
            .and_then(|t| t.lines().next())
            .filter(|t| !t.is_empty())
            .map(str::to_string);
    }
    custom.or(generated)
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

/// The idle reaper: close terminals idle past the configured timeout.
///
/// One task for the whole daemon, spawned only when armed. It parks while
/// the terminal set is empty (zero wakeups: creation announces, which ends
/// the wait) and while stopping, and otherwise wakes on an adaptive tick.
/// Each pass snapshots eligibility for every terminal, applies waiter-edge
/// vetoes against that snapshot (never pass order), then puts to sleep each
/// space all of whose tabs are ready, the way a person would ask for it.
async fn idle_sleep_loop(
    into: Restore,
    watched: crate::idle::WatchCounts,
    timeout: std::time::Duration,
) {
    // One startup delay under 30 seconds, from the wall clock rather than a
    // new dependency, so a fleet armed together does not sleep in one herd.
    let jitter = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() % 30)
        .unwrap_or(0);
    tokio::time::sleep(std::time::Duration::from_secs(jitter as u64)).await;
    let tick = crate::idle::reap_tick(timeout);
    let timeout_secs = timeout.as_secs().min(i64::MAX as u64) as i64;
    let mut changes = into.agents.subscribe();
    loop {
        if into
            .books
            .stopping
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            break;
        }
        if into.agents.streams().is_empty() {
            // Parked: creation registers and announces, which ends this wait. A lagged
            // wait is a nudge like any other; a closed channel cannot happen while this
            // task holds the registry, but break rather than spin if it ever does.
            match changes.recv().await {
                Ok(_) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
        tokio::time::sleep(tick).await;
        if into
            .books
            .stopping
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            break;
        }
        idle_sleep_pass(&into, &watched, timeout_secs).await;
    }
}

/// One pass: every space whose tabs have all been idle and unwatched past the timeout goes
/// to sleep, through the same sleep a person asks for, which vets each tab again in full.
async fn idle_sleep_pass(into: &Restore, watched: &crate::idle::WatchCounts, timeout_secs: i64) {
    // Edges older than a day are over by definition; swept over all edges,
    // every pass, before anything evaluates.
    into.agents.sweep_expectations(crate::agents::now_unix());
    for (space, all) in idle_spaces(into, watched, timeout_secs) {
        // Asked again for this space alone, just before it sleeps: the spaces before it took
        // seconds each, and in them a person may have come back to this one.
        if !all || idle_spaces(into, watched, timeout_secs).get(&space) != Some(&true) {
            continue;
        }
        match sleep_space(into, space).await {
            Ok(ended) => eprintln!("qorod: idle sleep: space {space} asleep, {ended} tabs ended"),
            // Said once per pass and tried again on the next: a refusal here is a tab that
            // is not what it looked like from its screen alone.
            Err(why) => eprintln!("qorod: idle sleep: {why}"),
        }
    }
}

/// Every space with a live tab, and whether all of its tabs are idle past the timeout,
/// unwatched and not held by a wait.
fn idle_spaces(
    into: &Restore,
    watched: &crate::idle::WatchCounts,
    timeout_secs: i64,
) -> BTreeMap<i64, bool> {
    let agents = &into.agents;
    let now = crate::agents::now_unix();
    let streams = agents.streams();
    // Snapshot eligibility for every live terminal at pass start. Edge
    // vetoes evaluate against this map, so the outcome never depends on
    // the order the pass happens to visit terminals in.
    let mut eligible: BTreeMap<u32, bool> = BTreeMap::new();
    for stream in &streams {
        if agents.is_closed(*stream) {
            eligible.insert(*stream, false);
            continue;
        }
        let Some(snap) = agents.reap_snapshot(*stream) else {
            eligible.insert(*stream, false);
            continue;
        };
        // Always seeded at spawn in production; `None` is only reachable
        // in unit tests that register without spawning.
        let Some(since) = snap.idle_since else {
            eligible.insert(*stream, false);
            continue;
        };
        let idle_secs = now.saturating_sub(since);
        let foreground = crate::idle::classify_program(snap.program.as_deref(), snap.agent);
        let verdict = crate::idle::eligible_for_reap(
            snap.state,
            foreground,
            snap.boundary,
            snap.setup_gate,
            watched.watched(*stream),
            idle_secs,
            timeout_secs,
        ) && !agents.shield_fresh(*stream, now, timeout_secs);
        eligible.insert(*stream, verdict);
    }
    let edges: Vec<(u32, u32)> = agents
        .expectation_edges()
        .into_iter()
        .filter(|(_, target)| !agents.is_closed(*target))
        .collect();
    let ready = crate::idle::apply_edge_vetoes(&eligible, &edges);
    // A space sleeps whole or not at all: every one of its tabs has to be ready.
    let mut spaces: BTreeMap<i64, bool> = BTreeMap::new();
    for stream in &streams {
        let Some(space) = into.terminals.workspace_of(*stream) else {
            continue;
        };
        let this = ready.get(stream).copied().unwrap_or(false);
        let all = spaces.entry(space).or_insert(true);
        *all = *all && this;
    }
    spaces
}

/// Whether a `from_terminal` claim on an incoming Input binds to its
/// sender: the claimed terminal is live in this daemon, is not the target
/// itself, and the presented token equals the claimed terminal's stored
/// token (hook-grade binding: per-terminal unguessable nonces, equality
/// only, no generation since a CLI cannot know it). Anything unverified is
/// ignored, fail closed. An edge only ever keeps a tab awake, at most for
/// the edge TTL: the pre-feature default, never a close.
fn verify_waiter(
    agents: &Arc<crate::agents::AgentRegistry>,
    terminals: &Arc<TerminalRegistry>,
    waiter: u32,
    token: &str,
    target: u32,
) -> bool {
    waiter != target
        && terminals.get(waiter).is_some()
        && agents.token_of(waiter).is_some_and(|t| t.as_str() == token)
}

/// Write down that a close happened, so a retry of the same request finds it applied
/// rather than reporting a terminal that is already gone.
async fn record_close(store: &crate::store::StoreHandle, id: &str, terminal: u32) {
    // Best effort. Refusing a close because its answer could not be written down would
    // leave a tab the client cannot get rid of; a retry of an unrecorded close is answered
    // from the terminal instead, which is the better failure for a close.
    let _ = store
        .record_applied(
            id.to_string(),
            true,
            serde_json::json!({ "terminal": terminal }),
        )
        .await;
}

/// Where a request is answered: on the connection's read loop, holding already the lock of
/// the space it changes when it changes one; or in a task of its own.
enum Dispatch {
    Inline(Option<tokio::sync::OwnedMutexGuard<()>>),
    Apart,
}

/// A sleep or a wake always goes apart. A tab made or closed, or a space closed, stays on
/// the read loop only when its space is awake and its lock is free this instant, and then
/// the lock is taken here and handed over: the read loop never waits on a space lock,
/// because typing into every other tab on the connection waits with it.
fn dispatch(
    request: &ControlRequest,
    books: &Bookkeeping,
    terminals: &TerminalRegistry,
    default: i64,
) -> Dispatch {
    let take = |workspace: i64| {
        if books.is_asleep(workspace) {
            return Dispatch::Apart;
        }
        match books.lock_of(workspace).try_lock_owned() {
            Ok(guard) => Dispatch::Inline(Some(guard)),
            Err(_) => Dispatch::Apart,
        }
    };
    match request {
        ControlRequest::SleepWorkspace { .. } | ControlRequest::WakeWorkspace { .. } => {
            Dispatch::Apart
        }
        ControlRequest::CreateTerminal { workspace, .. } => take(workspace.unwrap_or(default)),
        ControlRequest::CloseTerminal { terminal, .. } => match terminals.workspace_of(*terminal) {
            Some(workspace) => take(workspace),
            None => Dispatch::Apart,
        },
        ControlRequest::CloseWorkspace { workspace, .. } => take(*workspace),
        _ => Dispatch::Inline(None),
    }
}

/// Answer one control request.
///
/// `held` is the lock of the space the request changes, when `dispatch` already took it;
/// an arm that needs one and was handed none takes it itself, which only happens off the
/// read loop.
#[allow(clippy::too_many_arguments)]
async fn handle_control(
    request: ControlRequest,
    terminals: &Arc<TerminalRegistry>,
    subscriptions: &Subscriptions,
    renders: &Arc<RenderQueue>,
    shell: &Path,
    warnings: &Arc<std::sync::Mutex<Vec<Warning>>>,
    workspace: i64,
    history_dir: Option<PathBuf>,
    client: &ClientId,
    scanner: &Arc<ContainmentScanner>,
    agents: &Arc<crate::agents::AgentRegistry>,
    watched: &crate::idle::WatchCounts,
    connections: &Arc<ConnectionRegistry>,
    books: &Bookkeeping,
    held: Option<tokio::sync::OwnedMutexGuard<()>>,
) -> ControlResponse {
    let id = request.request_id().to_string();
    let store = &books.store;
    let stopping = &books.stopping;
    // What a sleep, a wake and a tab made in a sleeping space need, in one value.
    let restore = || Restore {
        terminals: terminals.clone(),
        books: books.clone(),
        shell: shell.to_path_buf(),
        warnings: warnings.clone(),
        history_dir: history_dir.clone(),
        scanner: scanner.clone(),
        agents: agents.clone(),
    };

    // A command that has already been applied returns its original answer rather than
    // being done again. ADR-017: the ledger is what makes a retry after an ambiguous
    // failure safe, and the reason it is a lookup and not a scan is that a scan gets
    // slower exactly as the table grows.
    //
    // A create looks under its space's lock instead, below, so it never answers from a row
    // whose shell another attempt is still starting.
    //
    // A lookup that fails is treated as "not applied" rather than refused. A create is
    // still safe: its own write carries the answer and fails on a key already answered. A
    // close refused because the ledger could not be read would leave a shell the client
    // cannot get rid of, which is the failure worth avoiding (ADR-005 rule 1).
    if matches!(request, ControlRequest::CloseTerminal { .. })
        && let Ok(Some((ok, detail))) = store.applied(id.clone()).await
    {
        return replayed_response(&id, ok, &detail, false);
    }

    match request {
        ControlRequest::CreateTerminal {
            cwd,
            workspace: wanted,
            name,
            ..
        } => {
            // The workspace the client asked for, or the one the daemon opened. A client
            // that does not know about workspaces sends nothing and lands in the default,
            // which is what it already believed was happening.
            let workspace = wanted.unwrap_or(workspace);
            // Held for the whole create, so a sleep of this space waits for the new tab
            // and then vets it, rather than missing it. A space asleep wakes first: a tab
            // made in it must not leave it half awake.
            let _held = match held {
                Some(guard) => guard,
                None => books.lock_of(workspace).lock_owned().await,
            };
            // Asked under the lock: an earlier attempt at this create in this space has
            // either started its shell and answered, or undone itself, by now.
            if let Ok(Some((ok, detail))) = store.applied(id.clone()).await {
                return replayed_response(&id, ok, &detail, true);
            }
            if books.is_asleep(workspace)
                && let Err(reason) = wake_locked(&restore(), workspace).await
            {
                return ControlResponse::Failed { id, reason };
            }
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
                // A path the client chose, so probed with a deadline (ADR-011): a hung mount
                // answers in two seconds instead of holding a runtime thread. A person is
                // waiting, so it never queues behind stuck probes: a dead mount elsewhere
                // cannot stop tabs opening in every other directory. Never spawned into
                // unchecked, because the spawn's `chdir` runs on this runtime thread.
                Some(path) => match probe_dir_asked(&path).await {
                    Ok(true) => Some(path),
                    Ok(false) => {
                        return ControlResponse::Failed {
                            id,
                            reason: format!("cwd {} is not a directory", path.display()),
                        };
                    }
                    Err(e) => {
                        return ControlResponse::Failed {
                            id,
                            reason: format!("cwd {} could not be checked: {e}", path.display()),
                        };
                    }
                },
            };
            // Recorded before it is started, and acknowledged only once the row is
            // durable (ADR-005 rule 2). A create acknowledged before the write is a create
            // a client cannot safely retry.
            // The daemon's own directory when the client named none, spelled out rather
            // than left as ".": a restart resolves it against wherever the next daemon
            // happens to start, which is not where this shell was.
            let recorded_cwd = cwd
                .as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| here().display().to_string());
            // Minted before the row is written, because the row is keyed by it. One
            // number for one terminal, in the registry and in the database.
            let stream = terminals.mint();
            // The row, its name and the answer are one transaction, so a client is never
            // told `Failed` about a shell that is running, and a daemon killed after this
            // point comes back with the tab and the answer that names it (ADR-005).
            if let Err(e) = store
                .create_terminal_answering(
                    id.clone(),
                    stream as i64,
                    workspace,
                    recorded_cwd.clone(),
                    shell.display().to_string(),
                    name.clone(),
                )
                .await
            {
                // A deadline does not cancel the write, so this may be our own commit
                // landing late, which is carried on with. Or another attempt at this create,
                // in another space, got there first: its answer is this one's too.
                match store.applied(id.clone()).await {
                    Ok(Some((true, detail)))
                        if detail.get("terminal").and_then(|t| t.as_u64())
                            == Some(u64::from(stream)) => {}
                    Ok(Some((ok, detail))) => {
                        return replayed_response(&id, ok, &detail, true);
                    }
                    _ => {
                        // Undone in case it still lands, so a late commit leaves neither a
                        // tab for the next start nor an answer naming one.
                        let _ = store.abandon_create(id.clone(), stream as i64).await;
                        return ControlResponse::Failed {
                            id,
                            reason: format!("could not record the terminal: {e}"),
                        };
                    }
                }
            }
            match open_terminal(
                terminals,
                stream,
                workspace,
                name,
                shell,
                cwd.as_deref(),
                warnings.clone(),
                history_dir.clone(),
                scanner.clone(),
                agents.clone(),
                Some(books.clone()),
            ) {
                Ok(_) => {
                    // Subscribed on creation. A client that asked for a terminal wants to
                    // see it, and a separate round trip to say so is a race in which the
                    // first output is lost.
                    subscribe(terminals, subscriptions, renders, watched, stream);
                    ControlResponse::TerminalCreated {
                        id,
                        terminal: stream,
                    }
                }
                Err(e) => {
                    // The same: a spawn that failed leaves no tab behind to come back,
                    // and no answer naming it, so a retry tries again.
                    let _ = store.abandon_create(id.clone(), stream as i64).await;
                    ControlResponse::Failed {
                        id,
                        reason: format!("cannot create a terminal: {e}"),
                    }
                }
            }
        }
        ControlRequest::CloseTerminal { terminal, .. } => {
            // Under its space's lock, taken before anything is read, so a sleep deciding
            // which rows to keep never sees this tab half closed, and this close never acts
            // on what it saw before a sleep or a wake moved the tab.
            let mut held = held;
            if let Some(space) = terminals.workspace_of(terminal) {
                let guard = match held.take() {
                    Some(guard) => guard,
                    None => books.lock_of(space).lock_owned().await,
                };
                if let Some(handle) = terminals.get(terminal) {
                    // Closing is a request to the terminal, not a registry edit: the
                    // terminal owns its teardown, which has to contain what it started
                    // (ADR-014).
                    //
                    // An explicit close wins over a space going to sleep: its row
                    // closes rather than being kept, and idle sleep leaves the tab
                    // alone while it tears down. Marked before asking, so a teardown
                    // that runs before this handler's next line still reads it; taken
                    // back if the ask is refused. Only a mark this request set is taken
                    // back: a repeat close refused while an accepted one tears down must
                    // not undo that one.
                    let marked = agents.mark_closed(terminal);
                    // The result is not discarded. A terminal whose command channel is
                    // full, because another connection is pasting into it, would otherwise
                    // be reported closed to a client that then stops subscribing while the
                    // shell keeps running, with no way for that client to find out.
                    return match handle.commands.try_send(ClientCommand::Close) {
                        Ok(()) => {
                            record_close(store, &id, terminal).await;
                            ControlResponse::TerminalClosed { id, terminal }
                        }
                        Err(e) => {
                            if marked {
                                agents.take_closed(terminal);
                            }
                            ControlResponse::Failed {
                                id,
                                reason: format!(
                                    "terminal {terminal} did not accept the close: {e}"
                                ),
                            }
                        }
                    };
                }
                // Gone while the lock was waited for: asleep now, or closed. Let go, so
                // the sleeping close below can take the lock of the space it is in.
                drop(guard);
            }
            // No live terminal: a tab of a sleeping space, closed for good without waking
            // it. Anything else is refused.
            match close_sleeping_tab(books, history_dir.as_deref(), terminal).await {
                Ok(true) => {
                    record_close(store, &id, terminal).await;
                    return ControlResponse::TerminalClosed { id, terminal };
                }
                Ok(false) => {}
                Err(reason) => return ControlResponse::Failed { id, reason },
            }
            ControlResponse::Failed {
                id,
                reason: format!("no terminal {terminal}"),
            }
        }
        ControlRequest::Scrollback {
            terminal, up, rows, ..
        } => {
            let dir = match history_of(terminals, history_dir.as_ref(), terminal) {
                Ok(dir) => dir,
                // A tab of a sleeping space has no terminal but keeps its history, which
                // is what a client shows of it before deciding to wake it.
                Err(reason) => {
                    match sleeping_history_of(books, history_dir.as_ref(), terminal).await {
                        Some(dir) => dir,
                        None => return ControlResponse::Failed { id, reason },
                    }
                }
            };
            // Capped, because the count comes from a client and a client that asks for a
            // hundred million rows would be asking the daemon to allocate them.
            let rows = rows.min(MAX_SCROLLBACK_ROWS);
            // On the store pool with a deadline, like every other read of a disk
            // (ADR-011): a hung filesystem under the history must not hang the daemon, and
            // a raw blocking thread per request is not a bound.
            let read = crate::blocking::run_within(crate::store::STORE_DEADLINE, move || {
                let history = qoro_store::History::open(&dir)?;
                let total = history.len();
                // Not below the oldest row still kept: past the cap the oldest segments
                // are gone, and a read from below them answers from the first kept row
                // while claiming to start where it was asked.
                let from = total
                    .saturating_sub(up as u64)
                    .max(history.first_ordinal()?);
                let lines = history.read(from, rows)?;
                Ok::<_, qoro_store::StoreError>((total, from, lines))
            })
            .await;
            match read {
                Ok(Ok((total, from, lines))) => ControlResponse::Scrollback {
                    id,
                    terminal,
                    from,
                    total,
                    lines,
                },
                Ok(Err(e)) => ControlResponse::Failed {
                    id,
                    reason: format!("the scrollback could not be read: {e}"),
                },
                _ => ControlResponse::Failed {
                    id,
                    reason: "the scrollback did not answer in time".to_string(),
                },
            }
        }
        // The connection loop spawns these rather than coming here; answered inline for any
        // other caller, which has no newer search to be superseded by.
        ControlRequest::SearchScrollback {
            terminal,
            query,
            from,
            backward,
            ..
        } => match history_of(terminals, history_dir.as_ref(), terminal) {
            Ok(dir) => search_scrollback(id, terminal, query, from, backward, dir, || false).await,
            Err(reason) => ControlResponse::Failed { id, reason },
        },
        ControlRequest::Shutdown { .. } => {
            // Set before anything is closed. Every terminal is about to end, and a
            // terminal ending after this is the daemon going rather than a person closing
            // a tab: recorded as closed, none of them would come back.
            stopping.store(true, std::sync::atomic::Ordering::SeqCst);
            let live = terminals.streams();
            for stream in &live {
                if let Some(handle) = terminals.get(*stream) {
                    // Asked rather than killed, so each terminal runs the teardown that
                    // contains what it started (ADR-014). `try_send` because a terminal
                    // whose queue is full is one being pasted into, and waiting on it
                    // would hold up the shutdown of every other one.
                    let _ = handle.commands.try_send(ClientCommand::Close);
                }
            }
            let terminals = terminals.clone();
            let connections = connections.clone();
            let asker = client.clone();
            let counted = live.len();
            tokio::spawn(async move {
                // The answer goes out on the connection this handler returns to, so the
                // exit waits for the terminals to actually go and for the asker to hang
                // up, which it does once it has read the answer: with no terminals the
                // first check used to pass at once and the process could end before the
                // answer was written, failing `qoro stop` on a stop that worked. It gives
                // up on both at the deadline: a stop that leaves a daemon running because
                // one shell ignored a hangup is not a stop.
                let deadline = tokio::time::Instant::now() + SHUTDOWN_PATIENCE;
                while tokio::time::Instant::now() < deadline
                    && (!terminals.streams().is_empty() || connections.contains(&asker))
                {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                crate::store::settle_owed(deadline).await;
                std::process::exit(0);
            });
            ControlResponse::Stopping {
                id,
                terminals: counted,
            }
        }
        ControlRequest::ListTerminals { .. } => ControlResponse::Terminals {
            id,
            terminals: terminals.streams(),
        },
        ControlRequest::ListWorkspaces { .. } => {
            // Counted from the terminals that are open now rather than from the store: the
            // store knows which terminals were ever created in a workspace, and a client
            // drawing a list wants to know which ones are alive.
            let live = terminals.streams();
            let rows = match store.workspaces().await {
                Ok(rows) => rows,
                Err(e) => {
                    return ControlResponse::Failed {
                        id,
                        reason: format!("the workspaces could not be read: {e}"),
                    };
                }
            };
            let mut workspaces = Vec::with_capacity(rows.len());
            for row in rows {
                let asleep = books.is_asleep(row.id);
                // A sleeping space's tabs come from its rows: nothing of it is running.
                let sleeping = if asleep {
                    store
                        .open_terminals(row.id)
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .map(|tab| crate::control::SleepingTab {
                            terminal: tab.id as u32,
                            title: tab.title,
                            session: agent_title(tab.agent_session.as_deref()),
                            agent: tab.agent,
                            cwd: tab.cwd,
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                workspaces.push(crate::control::WorkspaceRow {
                    id: row.id,
                    name: row.name,
                    terminals: live
                        .iter()
                        .filter(|s| terminals.workspace_of(**s) == Some(row.id))
                        .count(),
                    asleep,
                    sleeping,
                    // The default is where a client that names no workspace lands, and
                    // `CloseWorkspace` refuses it (below), so a client that is told so
                    // does not draw a close for a space that cannot be closed.
                    closable: row.id != workspace,
                });
            }
            ControlResponse::Workspaces { id, workspaces }
        }
        ControlRequest::RenameTerminal { terminal, name, .. } => {
            // A name is trimmed here rather than at the client, because every client would
            // otherwise have to remember to, and a terminal called `"  "` is a terminal
            // with a blank tab.
            let name = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
            if terminals.rename(terminal, name.clone()) {
                // Written down before the answer, the way a name given at birth is:
                // a renamed tab that is stopped before a background write would come
                // back under its old name. Clearing writes NULL rather than leaving
                // the old name behind.
                match name {
                    Some(name) => {
                        let _ = store.set_title(terminal as i64, name).await;
                    }
                    None => {
                        let _ = store.clear_title(terminal as i64).await;
                    }
                }
                // The index is what draws a name, so a rename that nobody is told about is
                // a rename nobody sees until the next unrelated change.
                agents.announce(terminal);
                ControlResponse::Renamed { id }
            } else {
                ControlResponse::Failed {
                    id,
                    reason: format!("there is no terminal {terminal}"),
                }
            }
        }
        ControlRequest::RenameWorkspace {
            workspace, name, ..
        } => {
            let name = name.trim().to_string();
            if name.is_empty() {
                return ControlResponse::Failed {
                    id,
                    reason: "a workspace needs a name".to_string(),
                };
            }
            match store.rename_workspace(workspace, name).await {
                Ok(0) => ControlResponse::Failed {
                    id,
                    reason: format!("there is no workspace {workspace}"),
                },
                Ok(_) => ControlResponse::Renamed { id },
                Err(e) => ControlResponse::Failed {
                    id,
                    reason: format!("the workspace could not be renamed: {e}"),
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
        ControlRequest::CloseWorkspace {
            workspace: target, ..
        } => {
            // The default is where a client that names no workspace lands, so it is never
            // closable: dropping it would turn every later default creation into a
            // foreign-key refusal until the daemon restarts and opens another.
            if target == workspace {
                return ControlResponse::Failed {
                    id,
                    reason: format!(
                        "workspace {target} is this session's default and cannot be closed"
                    ),
                };
            }
            // Counted from the store, not the live registry: a row that is open but not
            // running (a stopped or sleeping tab) is still a tab, and the delete below
            // cascades to it. An unreadable count refuses rather than guessing zero.
            let _held = match held {
                Some(guard) => guard,
                None => books.lock_of(target).lock_owned().await,
            };
            let closing = books.closing.lock().unwrap().clone();
            let live = match store.open_terminals(target).await {
                // A tab whose close is on its way to the store is gone already.
                Ok(rows) => rows
                    .iter()
                    .filter(|row| !closing.contains(&(row.id as u32)))
                    .count(),
                Err(e) => {
                    return ControlResponse::Failed {
                        id,
                        reason: format!("the workspace could not be checked: {e}"),
                    };
                }
            };
            if live > 0 {
                return ControlResponse::Failed {
                    id,
                    reason: format!(
                        "workspace {target} still holds {live} tab{}, running or not \
                         (stopped, asleep or not restored): close them first",
                        if live == 1 { "" } else { "s" }
                    ),
                };
            }
            match store.delete_workspace(target).await {
                Ok(0) => ControlResponse::Failed {
                    id,
                    reason: format!("there is no workspace {target}"),
                },
                Ok(_) => {
                    // Its sleep mark went with the row; the mirror follows.
                    books.asleep.lock().unwrap().remove(&target);
                    ControlResponse::WorkspaceClosed {
                        id,
                        workspace: target,
                    }
                }
                Err(e) => ControlResponse::Failed {
                    id,
                    reason: format!("the workspace could not be closed: {e}"),
                },
            }
        }
        ControlRequest::SleepWorkspace {
            workspace: target, ..
        } => match sleep_space(&restore(), target).await {
            Ok(ended) => ControlResponse::WorkspaceAsleep {
                id,
                workspace: target,
                terminals: ended,
            },
            Err(reason) => ControlResponse::Failed { id, reason },
        },
        ControlRequest::WakeWorkspace {
            workspace: target, ..
        } => match wake_space(&restore(), target).await {
            Ok((woken, report)) => ControlResponse::WorkspaceAwake {
                id,
                workspace: woken,
                terminals: report.brought_back,
                resumed: report.agents_resumed,
            },
            Err(reason) => ControlResponse::Failed { id, reason },
        },
        ControlRequest::ListAgents { .. } => {
            // What the attached interfaces count as news, read once for every row.
            let news = connections.news();
            // Titles come out of the agents' own storage: a directory walk, a megabyte of
            // transcript per Claude tab, a database for opencode, on a disk the user
            // chose. All of them at once on the probe pool with a deadline (ADR-011),
            // rather than on this runtime thread during the call a sidebar polls. Not
            // counted against a prefix, because no one path is being asked about, and
            // refused at once when the pool is full rather than queued behind stuck
            // probes. A title not read in time is absent, and the panel shows the short id.
            let sessions: Vec<(u32, Option<String>)> = terminals
                .streams()
                .into_iter()
                .map(|t| (t, terminals.session_of(t)))
                .collect();
            let titles: BTreeMap<u32, String> =
                crate::blocking::try_probe(PROBE_DEADLINE, move || {
                    sessions
                        .into_iter()
                        .filter_map(|(t, s)| Some((t, agent_title(s.as_deref())?)))
                        .collect()
                })
                .await
                .unwrap_or_default();
            // Read from the registry rather than by walking the process table here: the
            // registry is what the process watcher and the observation precedence in
            // ADR-008 already maintain, and a second reader would be a second answer.
            // One read of the registry for the rows and the splits both, so a terminal
            // that closes in between cannot be in one and not the other.
            let live = terminals.streams();
            let rows: Vec<crate::control::AgentRow> = live
                .iter()
                .copied()
                .map(|terminal| {
                    let agent = agents.agent(terminal);
                    let (program, behind, generation, is_agent) =
                        match agent.as_ref().map(|a| &a.identity) {
                            Some(qoro_domain::AgentIdentity::Known {
                                program,
                                generation,
                                agent,
                            }) => (Some(program.clone()), None, Some(generation.0), *agent),
                            Some(qoro_domain::AgentIdentity::Unavailable {
                                boundary: qoro_domain::Boundary::Namespace { wrapper },
                                generation,
                            }) => (None, Some(wrapper.clone()), Some(generation.0), false),
                            _ => (None, None, None, false),
                        };
                    let state = agent
                        .as_ref()
                        .map(|a| state_name(a.belief.state))
                        .unwrap_or("unknown")
                        .to_string();
                    let agent_session = terminals.session_of(terminal);
                    let agent_title = titles.get(&terminal).cloned();
                    // The explanation rides the row: what holds the belief,
                    // and what the screen last matched. A client that does
                    // not know these fields ignores them; `qoro agents
                    // explain` reads them.
                    let (source, rule, manifest) = agent
                        .as_ref()
                        .map(|a| {
                            (
                                Some(format!("{:?}", a.belief.source)),
                                a.screen_rule.clone(),
                                a.screen_manifest.clone(),
                            )
                        })
                        .unwrap_or((None, None, None));
                    // Arrival-stamped by the daemon, read off the registry:
                    // no timestamp until the first accepted observation, no
                    // cwd until the terminal's own task has seen one.
                    let last_active_at_unix = agent.as_ref().and_then(|a| a.last_active_at_unix);
                    // The foreground job's group, which a client on this machine samples for
                    // its cost; one syscall on the master already held open.
                    let pgid = agents
                        .master_of(terminal)
                        .and_then(qoro_pty::sys::foreground_pgrp)
                        .map(|group| group.0)
                        // Linux answers zero when nothing holds the foreground, and zero is
                        // every kernel thread's group: no group rather than theirs.
                        .filter(|group| *group > 0);
                    // What it is doing, only while it is doing something: a calm agent
                    // has no activity, whatever its last tool was.
                    let activity = matches!(
                        state.as_str(),
                        "working" | "starting" | "blocked" | "failed"
                    )
                    .then(|| agents.activity(terminal))
                    .flatten();
                    let cwd = terminals.cwd_of(terminal);
                    // Read before the row moves the strings: the mapping only
                    // borrows, and the literal below owns.
                    let readiness = crate::control::readiness_of(
                        program.as_deref(),
                        behind.as_deref(),
                        &state,
                        agent.as_ref().is_some_and(|a| a.setup_gate),
                    )
                    .to_string();
                    crate::control::AgentRow {
                        news: news.contains(&terminal),
                        terminal,
                        name: terminals.name_of(terminal),
                        workspace: terminals.workspace_of(terminal).unwrap_or(workspace),
                        // Asked of the same table that resolved the name in the first
                        // place. The wrappers have already been seen through by the time
                        // the name is stored, so there is nothing left here to unwrap and
                        // no argv to pass.
                        //
                        // A terminal behind a namespace counts too, and it is the case that
                        // matters most: an agent started with `docker run` is one this
                        // daemon cannot read at all, so it can never be named, and a panel
                        // that listed only what it could name would leave the one terminal
                        // nobody can see into off the screen entirely. The row says
                        // `behind docker` rather than claiming a program, so it promises
                        // only what is known. The cost is a row for a `docker ps` that is
                        // not an agent, which is a row too many rather than a terminal
                        // missing.
                        agent: is_agent || behind.is_some(),
                        program,
                        behind,
                        state,
                        generation,
                        readiness,
                        source,
                        rule,
                        manifest,
                        agent_session,
                        agent_title,
                        last_active_at_unix,
                        cwd,
                        slept_at_unix: None,
                        activity,
                        pgid,
                        // Present once anything was thrown away, so the ordinary row
                        // does not grow by a block of zeros.
                        discards: terminals
                            .get(terminal)
                            .map(|handle| {
                                let engine = *handle.discards.lock().unwrap();
                                crate::control::Discards {
                                    control_strings: engine.control_strings,
                                    control_string_bytes: engine.control_string_bytes,
                                    images: engine.images,
                                    image_bytes: engine.image_bytes,
                                    evicted_unobserved: engine.evicted_unobserved,
                                    images_flooded: handle
                                        .image_flood_dropped
                                        .load(std::sync::atomic::Ordering::Relaxed),
                                }
                            })
                            .filter(|discards| *discards != crate::control::Discards::default()),
                    }
                })
                .collect();
            // A space still waking has only some of its tabs back: a tree cut to those would
            // be taken as the whole split. Its trees come with the index after the wake.
            let settled: Vec<u32> = live
                .iter()
                .copied()
                .filter(|t| {
                    !terminals
                        .workspace_of(*t)
                        .is_some_and(|w| books.is_asleep(w))
                })
                .collect();
            ControlResponse::Agents {
                id,
                agents: rows,
                splits: terminals.live_splits(&settled),
            }
        }
        // A tree naming one terminal twice would come back on every index as a tab
        // showing one terminal in two tiles.
        ControlRequest::SetSplit { tree, .. }
            if tree.leaves().len()
                != tree
                    .leaves()
                    .into_iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len() =>
        {
            ControlResponse::Failed {
                id,
                reason: "a split names one terminal twice".to_string(),
            }
        }
        ControlRequest::SetSplit { tree, .. } => match terminals.set_split(tree, store).await {
            Ok(()) => ControlResponse::SplitSaved { id },
            Err(e) => ControlResponse::Failed {
                id,
                reason: format!("the split could not be recorded: {e}"),
            },
        },
        ControlRequest::Focus { terminal, .. } => match terminals.get(terminal) {
            Some(_) => ControlResponse::Focused {
                id,
                clients: connections.focus(terminal),
            },
            None => ControlResponse::Failed {
                id,
                reason: format!("no terminal {terminal}"),
            },
        },
        ControlRequest::News {
            terminals: news, ..
        } => {
            for terminal in connections.set_news(client, news) {
                agents.announce(terminal);
            }
            ControlResponse::NewsNoted { id }
        }
        ControlRequest::Subscribe {
            terminals: wanted, ..
        } => {
            let subscribed: Vec<u32> = wanted
                .into_iter()
                .filter(|s| subscribe(terminals, subscriptions, renders, watched, *s))
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
        ControlRequest::ReportAgentState {
            terminal,
            token,
            state,
            visible_blocker,
            visible_idle,
            session,
            event,
            label,
            ..
        } => {
            // An arrival, like output: admitted, applied, and announced when it
            // changed something, but never journalled. The wire clock is
            // deliberately not read here at all: it is advisory, and the
            // sequence the observation carries is assigned inside
            // `observe_hook` from the daemon's own counter.
            let Some(reported) = crate::control::parse_reported_state(&state) else {
                return ControlResponse::Failed {
                    id,
                    reason: format!("unknown agent state {state:?}"),
                };
            };
            // The terminal id is the stream number as a string: the same binding
            // `open_terminal` registered the capability under.
            let terminal_id = TerminalId::new(terminal.to_string());
            let token = qoro_domain::CapabilityToken::new(token);
            match agents.observe_hook(
                terminal,
                &terminal_id,
                &token,
                reported,
                visible_blocker,
                visible_idle,
                crate::agents::now_unix(),
            ) {
                Err(_) => ControlResponse::Failed {
                    id,
                    reason: format!("the report for terminal {terminal} was refused"),
                },
                Ok(_) => {
                    // A focus event is display only: recorded after admission,
                    // so a forged token cannot write it, and kept out of
                    // the belief, the push payload and the store. An absent
                    // or out-of-set kind means "a state report with nothing
                    // to show": no row, still a success.
                    if let Some(kind) = event.filter(|k| crate::control::parse_focus_event(k)) {
                        let at_unix = crate::agents::now_unix();
                        let before = agents.activity(terminal);
                        agents.push_focus_event(
                            terminal,
                            crate::control::FocusEvent {
                                kind,
                                label: crate::control::truncate_focus_label(
                                    label.as_deref().unwrap_or(""),
                                ),
                                at_unix,
                            },
                        );
                        // The row carries the activity, so a new one is news even when the
                        // state held: without this a run of tools inside one `working`
                        // would leave the sidebar naming the first of them. Announced after
                        // the push even when the state changed too, since that announce went
                        // out before the label was recorded; a client coalesces the two. A
                        // repeat of the same label is not news and announces nothing.
                        if agents.activity(terminal) != before {
                            agents.announce(terminal);
                        }
                    }
                    // The agent's own id for its conversation, recorded here and nowhere
                    // else: this is the point at which the report has been admitted, so a
                    // forged token cannot write one. It is the difference between coming
                    // back to this conversation and coming back to whichever one in the
                    // directory happens to be newest, which is not the same thing the
                    // moment a person has two agents in one repository.
                    //
                    // Awaited before the answer, under the store deadline, rather than
                    // spawned: a stop that lands between the answer and a background
                    // write would lose the conversation the tab was having.
                    if let Some(session) = session.filter(|s| !s.trim().is_empty()) {
                        let generation = agents.agent(terminal).map(|a| a.belief.generation.0);
                        terminals.set_session(terminal, Some(session.clone()), generation);
                        let _ = store.set_agent_session(terminal as i64, session).await;
                    }
                    ControlResponse::AgentStateReported { id, terminal }
                }
            }
        }
        ControlRequest::ReportAgentSession {
            terminal,
            token,
            session,
            ..
        } => {
            // Session identity without a state claim: admitted like any other
            // report, then written to the same store. The belief is untouched
            // on purpose: saying which conversation this is says nothing
            // about what the agent is doing, and an index that moved on a
            // session report would be an action with no observation behind it.
            if session.trim().is_empty() {
                return ControlResponse::Failed {
                    id,
                    reason: "empty session id: there is nothing to record".into(),
                };
            }
            let terminal_id = TerminalId::new(terminal.to_string());
            let token = qoro_domain::CapabilityToken::new(token);
            let Some(generation) = agents.agent(terminal).map(|a| a.belief.generation) else {
                return ControlResponse::Failed {
                    id,
                    reason: format!("the report for terminal {terminal} was refused"),
                };
            };
            if agents
                .admit(terminal, &terminal_id, Some(&token), generation)
                .is_err()
            {
                return ControlResponse::Failed {
                    id,
                    reason: format!("the report for terminal {terminal} was refused"),
                };
            }
            terminals.set_session(terminal, Some(session.clone()), Some(generation.0));
            let _ = store.set_agent_session(terminal as i64, session).await;
            ControlResponse::AgentStateReported { id, terminal }
        }
        ControlRequest::ListFocusEvents {
            terminal, last_n, ..
        } => {
            // Answered from daemon memory, never from disk: events are
            // arrivals like state reports. Capped, because the count comes
            // from a client and a client that asks for a hundred million
            // rows would be asking the daemon to allocate them.
            if terminals.get(terminal).is_none() {
                return ControlResponse::Failed {
                    id,
                    reason: format!("there is no terminal {terminal}"),
                };
            }
            let last_n = last_n.min(crate::control::MAX_FOCUS_EVENTS);
            let all = agents.focus_events(terminal);
            let from = all.len().saturating_sub(last_n);
            ControlResponse::FocusEvents {
                id,
                terminal,
                events: all[from..].to_vec(),
            }
        }
        // Another machine is the interface's business, not this daemon's. Reaching here
        // means a client sent it straight to a daemon, and that is said rather than ignored.
        ControlRequest::ConnectRemote { .. } | ControlRequest::InstallRemote { .. } => {
            ControlResponse::Failed {
                id,
                reason: "remote machines are reached by the interface, not by the daemon"
                    .to_string(),
            }
        }
        ControlRequest::ReloadAgentManifests { .. } => {
            // Answered live from the files, never journalled: a replayed answer
            // would lie about what the directory says now. Good files replace
            // their agent's rules; bad files keep the previous ones and are
            // reported per file, so one typo cannot unload every override.
            let outcome = qoro_detect::reload_overrides(&override_dir());
            ControlResponse::ManifestsReloaded {
                id,
                loaded: outcome.loaded,
                errors: outcome
                    .errors
                    .into_iter()
                    .map(|(file, reason)| format!("{file}: {reason}"))
                    .collect(),
            }
        }
        ControlRequest::Unsubscribe {
            terminals: wanted, ..
        } => {
            let mut held = subscriptions.lock().unwrap();
            let dropped: Vec<u32> = wanted
                .into_iter()
                .filter_map(|s| held.remove(&s).map(|(_, t)| (s, t)))
                .map(|(s, task)| {
                    task.abort();
                    watched.unwatch(s);
                    s
                })
                .collect();
            ControlResponse::Subscribed {
                id,
                terminals: dropped,
            }
        }
        ControlRequest::HostColors { fg, bg, .. } => {
            set_host_colors(fg, bg);
            ControlResponse::HostColorsNoted { id }
        }
    }
}

/// The outer terminal's foreground and background, as the latest client reported them:
/// a presence bit over two packed rgb triples, so every terminal task reads it with one
/// load before it feeds its engine and no lock sits on the read path.
static HOST_COLORS: AtomicU64 = AtomicU64::new(0);

const HOST_COLORS_SET: u64 = 1 << 48;

fn set_host_colors(fg: [u8; 3], bg: [u8; 3]) {
    let pack = |[r, g, b]: [u8; 3]| (u64::from(r) << 16) | (u64::from(g) << 8) | u64::from(b);
    HOST_COLORS.store(
        HOST_COLORS_SET | (pack(fg) << 24) | pack(bg),
        Ordering::Relaxed,
    );
}

/// What a program asking `OSC 10 ?` or `OSC 11 ?` is told, once any client has said.
fn host_colors() -> Option<([u8; 3], [u8; 3])> {
    let packed = HOST_COLORS.load(Ordering::Relaxed);
    let unpack = |v: u64| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    (packed & HOST_COLORS_SET != 0).then(|| (unpack(packed >> 24), unpack(packed)))
}

/// The answer a replayed create or close gets: the original one. Only those two are
/// recorded and replayed; a listing answered from a ledger would be a stale listing.
fn replayed_response(
    id: &str,
    ok: bool,
    detail: &serde_json::Value,
    created: bool,
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
    if created {
        ControlResponse::TerminalCreated {
            id: id.to_string(),
            terminal,
        }
    } else {
        ControlResponse::TerminalClosed {
            id: id.to_string(),
            terminal,
        }
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
    /// Every link target the runs point at, once each.
    ///
    /// A table rather than a target per run, because a program chooses its links: one
    /// four kilobyte address repeated per cell made a snapshot of more than eight
    /// megabytes, past what any client reassembles, and the session could no longer be
    /// attached to. The engine also caps what goes in here (`qoro_terminal::MAX_LINKS`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
    /// Whether the program in this terminal asked for the mouse, and whether it is on the
    /// alternate screen.
    ///
    /// What a client needs to know to decide whose the wheel is. Defaulted on the wire for
    /// the same reason the styles are: an older client reading a newer daemon's snapshot
    /// gets a screen it understands and one fewer thing it can do.
    #[serde(default)]
    pub modes: ModesWire,
    /// How many rows have gone behind this terminal's screen and been written down.
    ///
    /// What lets a client reading back through them hold its place while the terminal
    /// keeps printing: without it the view is a distance from a bottom that keeps moving,
    /// and a person reading is dragged along by the output they are trying to read past.
    #[serde(default)]
    pub behind: u64,
}

/// What the program in a terminal has asked its terminal to be, as it crosses the socket.
///
/// All but one of what the engine tracks. Bracketed paste is the daemon's
/// business alone, because it is the daemon that writes to the child; the
/// others are the client's, because they decide what the client draws and
/// what it does with a wheel of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModesWire {
    #[serde(default)]
    pub alt_screen: bool,
    #[serde(default)]
    pub mouse_reporting: bool,
    /// The program asked for application cursor keys, so an arrow is `SS3 A` and not
    /// `CSI A`.
    ///
    /// Here because a client sends arrows of its own: a wheel over a full screen program
    /// is turned into cursor keys, and the wrong spelling of one is a key the program does
    /// not recognise. Measured against a real `less`, which takes `\x1bOB` and ignores
    /// `\x1b[B` outright.
    #[serde(default)]
    pub application_cursor: bool,
    /// Whether the program wants its cursor seen. True when the daemon never
    /// said: an older daemon's snapshot predates the field, and its cursor was
    /// always shown, so missing reads as shown rather than as hidden. Skipped
    /// on the wire when shown, which is most frames of most programs. The
    /// reverse mix, an older client on a newer daemon, ignores the field and
    /// shows the cursor always: one stale cursor rather than none.
    #[serde(default = "cursor_visible_default", skip_serializing_if = "is_shown")]
    pub cursor_visible: bool,
    /// The program asked to hear when its window gains or loses the focus (`CSI ? 1004 h`).
    /// A client passes the outer terminal's focus on only then, and tells a pane it lost
    /// the keyboard when another pane takes it. Absent means not asked, which is what an
    /// older daemon's snapshot meant, and costs nothing on the wire for most programs.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub focus_events: bool,
}

/// A missing visibility is a visible cursor, for the reason on the field.
fn cursor_visible_default() -> bool {
    true
}

/// Shown is the common case, and the wire stays the size it always was for it.
fn is_shown(visible: &bool) -> bool {
    *visible
}

impl Default for ModesWire {
    /// Visible, for the same reason a missing field reads as visible.
    fn default() -> Self {
        Self {
            alt_screen: false,
            mouse_reporting: false,
            application_cursor: false,
            cursor_visible: true,
            focus_events: false,
        }
    }
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
    /// Where the run points, when the program linked it with `OSC 8`: its place in the
    /// snapshot's `links`. Absent on almost every run, and an older client ignores it and
    /// draws the text unlinked (ADR-010).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<u32>,
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

/// One image on the wire, as it crosses the socket.
///
/// Deliberately shaped so it parses as neither a snapshot nor anything older: it
/// carries none of `cols`/`rows`/`cursor`/`lines`, so a client that only knows
/// snapshots fails the parse and skips it (ADR-010). The envelope rides base64
/// because it may hold any byte; the sniffer's cap plus a third still fits the
/// reassembly bound.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ImageWire {
    pub v: u32,
    /// Monotonic per terminal, from the sniffer. What a client deletes by.
    pub id: u64,
    pub protocol: String,
    pub data: String,
}

impl ImageWire {
    pub fn protocol_name(protocol: qoro_terminal::ImageProtocol) -> &'static str {
        match protocol {
            qoro_terminal::ImageProtocol::Kitty => "kitty",
            qoro_terminal::ImageProtocol::ITerm2 => "iterm2",
            qoro_terminal::ImageProtocol::Sixel => "sixel",
        }
    }

    pub fn parse_protocol(name: &str) -> Option<qoro_terminal::ImageProtocol> {
        match name {
            "kitty" => Some(qoro_terminal::ImageProtocol::Kitty),
            "iterm2" => Some(qoro_terminal::ImageProtocol::ITerm2),
            "sixel" => Some(qoro_terminal::ImageProtocol::Sixel),
            _ => None,
        }
    }

    /// The envelope back out, or `None` when the peer sent what this version cannot
    /// read. Never panics on hostile input: over-long, wrong version and bad base64
    /// all read as absent, and the placeholder is what the client draws instead.
    pub fn decode(&self) -> Option<StoredImage> {
        use base64::Engine as _;
        if self.v != 1 {
            return None;
        }
        let protocol = Self::parse_protocol(&self.protocol)?;
        // Base64 inflates by a third: reject before allocating the decoded bytes.
        // A body inside the reassembly cap can still decode past the image cap.
        if self.data.len() > qoro_terminal::MAX_IMAGE_BYTES / 3 * 4 + 4 {
            return None;
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&self.data)
            .ok()?;
        if bytes.len() > qoro_terminal::MAX_IMAGE_BYTES {
            return None;
        }
        // Terminal is the frame's stream, filled in by the decoder, not the peer.
        Some(StoredImage {
            terminal: 0,
            id: self.id,
            protocol,
            bytes,
        })
    }
}

impl From<&StoredImage> for ImageWire {
    fn from(image: &StoredImage) -> Self {
        use base64::Engine as _;
        Self {
            v: 1,
            id: image.id,
            protocol: Self::protocol_name(image.protocol).to_string(),
            data: base64::engine::general_purpose::STANDARD.encode(&image.bytes),
        }
    }
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
        let mut links = Vec::new();
        let styles = s
            .styles
            .iter()
            .map(|row| row.iter().map(|span| span_wire(span, &mut links)).collect())
            .collect();
        Self {
            cols: s.cols,
            rows: s.rows,
            cursor: s.cursor,
            lines: s.lines.clone(),
            styles,
            links,
            behind: 0,
            modes: ModesWire {
                alt_screen: s.modes.alt_screen,
                mouse_reporting: s.modes.mouse_reporting,
                application_cursor: s.modes.application_cursor,
                cursor_visible: s.modes.cursor_visible,
                focus_events: s.modes.focus_events,
            },
        }
    }
}

/// One run as it crosses the socket, its link named by its place in `links`, which gains the
/// target the first time a run points at it.
fn span_wire(s: &qoro_terminal::Span, links: &mut Vec<String>) -> SpanWire {
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
    let link = st.link.as_deref().map(|uri| {
        let at = links
            .iter()
            .position(|known| known == uri)
            .unwrap_or_else(|| {
                links.push(uri.to_string());
                links.len() - 1
            });
        at as u32
    });
    SpanWire {
        at: s.at,
        len: s.len,
        fg: colour_wire(st.fg),
        bg: colour_wire(st.bg),
        attrs: bits,
        link,
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
    Input {
        bytes: Vec<u8>,
        /// Which terminal the sender runs in, if it runs in one. Filled by
        /// the CLI from `QORO_TERMINAL`/`QORO_TOKEN` for send/prompt, so the
        /// daemon keeps the waiter awake while its target is busy.
        /// Defaulted: older clients send no claim and get today's behavior.
        /// TUI typing and attach never fill it: only CLI send/prompt mints
        /// edges.
        #[serde(default)]
        from_terminal: Option<u32>,
        /// The claimed terminal's token, binding the claim to its sender
        /// (same strength as hook admission). Verified by equality, ignored
        /// when absent or wrong: a wait edge only ever keeps a tab awake,
        /// and a forgery without the token cannot mint one.
        #[serde(default)]
        from_token: Option<String>,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
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
    stream: u32,
    workspace: i64,
    name: Option<String>,
    shell: &Path,
    cwd: Option<&Path>,
    warnings: Arc<std::sync::Mutex<Vec<Warning>>>,
    history_dir: Option<PathBuf>,
    scanner: Arc<ContainmentScanner>,
    agents: Arc<crate::agents::AgentRegistry>,
    books: Option<Bookkeeping>,
) -> io::Result<TerminalHandle> {
    let (commands, from_clients) = mpsc::channel(256);
    let (updates, _) = watch::channel(TerminalUpdate::Full(Snapshot {
        cols: 80,
        rows: 24,
        cursor: (0, 0),
        lines: vec![String::new(); 24],
        styles: Vec::new(),
        modes: Default::default(),
    }));
    let token = crate::agents::mint_token(stream);
    let terminal_id = TerminalId::new(stream.to_string());
    agents.register(stream, terminal_id.clone(), token.clone());
    // Creation counts as activity on both clocks: a tab that never produces
    // an accepted observation ages from here rather than living forever.
    agents.note_spawn(stream, crate::agents::now_unix());
    let behind = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let handle = TerminalHandle {
        name,
        session: None,
        session_generation: None,
        // Seeded with where the shell starts: what was asked for, or the
        // daemon's own directory. The task writes the registry only when its
        // throttled read sees a change from there, so an unseeded tab that
        // never moves would stay unknown for good.
        cwd: Some(
            cwd.map_or_else(here, Path::to_path_buf)
                .display()
                .to_string(),
        ),
        workspace,
        history: behind.clone(),
        id: terminal_id,
        token: token.clone(),
        commands,
        updates: updates.clone(),
        last_image: Arc::new(std::sync::Mutex::new(None)),
        image_flood_dropped: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        discards: Arc::default(),
        last_dims: Arc::new(std::sync::atomic::AtomicU64::new((80u64 << 32) | 24u64)),
        incarnation: next_incarnation(),
    };
    terminals.insert(stream, handle.clone());
    let registry = terminals.clone();
    let spill = crate::spill::SpillRing::new();
    let history = history_dir.map(|dir| dir.join(stream.to_string()));
    if let Some(dir) = &history {
        let (done, finished) = watch::channel(());
        if let Some(books) = &books {
            books.writers.lock().unwrap().insert(stream, finished);
        }
        spawn_history_writer(
            dir.clone(),
            spill.clone(),
            behind.clone(),
            handle.commands.downgrade(),
            done,
        );
    }
    let forgetting = agents.clone();
    let unregister = agents.clone();
    let spawned = spawn_terminal(
        TerminalSpawn {
            shell: shell.to_path_buf(),
            cwd: cwd.map(Path::to_path_buf),
            token,
            stream,
            agents,
            terminals: terminals.clone(),
            books: books.clone(),
        },
        from_clients,
        updates,
        warnings,
        spill,
        scanner,
        move || {
            // An explicit close is read once here: it wins over a space going to sleep.
            let closed = forgetting.take_closed(stream);
            // Decided here, before the tab leaves the registry and while its workspace can
            // still be read: a daemon going, or a space going to sleep, keeps the row open
            // so the tab comes back. Anything else is a close, marked as under way until
            // the store has it, so a wake in between does not reopen it.
            let keeps = !closed
                && books
                    .as_ref()
                    .is_some_and(|b| b.keeps_row(registry.workspace_of(stream)));
            if !keeps && let Some(books) = &books {
                books.closing.lock().unwrap().insert(stream);
            }
            registry.remove(stream);
            forgetting.forget(stream);
            // A tab whose process ended is a tab the person is done with, so it stops
            // coming back. A daemon going down is the other case and writes nothing, which
            // is exactly why the next start finds its terminals still open.
            if let Some(books) = books
                && !keeps
            {
                let owed = crate::store::Owed::new();
                tokio::spawn(async move {
                    let _owed = owed;
                    let closed = books.store.close_terminal(stream as i64).await;
                    books.closing.lock().unwrap().remove(&stream);
                    // History lives as long as the tab can come back, and a closed row
                    // never does. Only after the close landed: a row still open is
                    // restored with its history. The writer may still be on its last
                    // flush; it writes to unlinked files, which is harmless.
                    if closed.is_ok()
                        && let Some(dir) = history
                    {
                        let _ = crate::blocking::run(move || std::fs::remove_dir_all(dir)).await;
                    }
                });
            }
        },
    );
    // Nothing started, so nothing stays listed: left in, the tab was a ghost with no
    // process behind it. The store row is the caller's call, because a restore that
    // failed for a moment must not close a tab for good. The writer ends on its own
    // once the ring handed to the failed spawn is dropped.
    if let Err(e) = spawned {
        terminals.remove(stream);
        unregister.forget(stream);
        return Err(e);
    }
    Ok(handle)
}

/// Record which agent is in each terminal, as the registry decides it.
///
/// One task for the daemon rather than one per terminal, because the registry announces
/// every change through one channel and this is the only reader that needs all of them.
fn spawn_agent_recorder(
    agents: Arc<crate::agents::AgentRegistry>,
    terminals: Arc<TerminalRegistry>,
    books: Bookkeeping,
) {
    let mut changes = agents.subscribe();
    tokio::spawn(async move {
        // What was last written for each terminal, so a rename or a state change, which
        // are announced through the same channel, does not write a row that says what the
        // row already says.
        let mut written: BTreeMap<u32, Option<String>> = BTreeMap::new();
        loop {
            match changes.recv().await {
                Ok(stream) => {
                    record_stream(&agents, &terminals, &books, &mut written, stream).await;
                }
                // Announcements are news, not a ledger, but a reader that fell behind
                // cannot know which terminals changed: re-read every open one rather
                // than waiting for the next announcement, which may never come.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    for stream in terminals.streams() {
                        record_stream(&agents, &terminals, &books, &mut written, stream).await;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });
}

/// Write down what is in one terminal, unless the row already says it.
///
/// Nothing is recorded once the daemon is going. Stopping kills every child,
/// so every agent is announced as gone on the way out, and recording that would
/// erase the one thing the next start needs: measured, a session with a real
/// Claude Code in it came back as a bare shell because the row was cleared
/// between the stop and the exit.
async fn record_stream(
    agents: &Arc<crate::agents::AgentRegistry>,
    terminals: &TerminalRegistry,
    books: &Bookkeeping,
    written: &mut BTreeMap<u32, Option<String>>,
    stream: u32,
) {
    if books.stopping.load(std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    // A tab that has already left says nothing worth writing: closed, its row is closed
    // anyway; asleep, the row must keep the agent it had. Nor does one whose space is
    // going to sleep, for the same reason.
    let Some(workspace) = terminals.workspace_of(stream) else {
        written.remove(&stream);
        return;
    };
    if books.is_asleep(workspace) {
        return;
    }
    let now = agent_program(agents, stream);
    if written.get(&stream).cloned().flatten() == now {
        return;
    }
    // An agent that has gone is written down as gone: a person who quit theirs
    // quit it, and starting it again on the next open would undo that. The map
    // moves only once the write lands: a failed write with an updated map would
    // skip the retry and every later reconciliation with it.
    if books
        .store
        .set_agent(stream as i64, now.clone(), None)
        .await
        .is_ok()
    {
        written.insert(stream, now);
    }
}

/// One step of the per-connection news pump: the next event to send.
///
/// A lagged receiver missed some announcements, and the missed ones are gone from
/// the channel. The answer is a resync rather than silence: the message is "look
/// again", and looking again once covers everything that was missed, including
/// the case where the missed change was the last one for a while. `None` ends
/// the pump; every other outcome is something the client should hear.
async fn pump_agent_event(
    changes: &mut tokio::sync::broadcast::Receiver<u32>,
) -> Option<crate::control::EventMessage> {
    match changes.recv().await {
        Ok(terminal) => Some(crate::control::EventMessage::AgentChanged { terminal }),
        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
            Some(crate::control::EventMessage::Resync)
        }
        Err(tokio::sync::broadcast::error::RecvError::Closed) => None,
    }
}

/// What one look at a screen tells the belief, or nothing when it should be left alone.
///
/// A known agent whose screen matches no rule reads as idle, never as its last state.
/// Without this a turn whose end face has no rule yet (or a manifest with no idle rule at
/// all) keeps its `working` belief forever, which is the index showing working for agents
/// that finished long ago. The fallback is deliberately weak evidence: it carries no
/// visible flag, so it settles a screen-held belief of the same source but never overrides
/// a hook-held state it cannot see. Only a positively seen composer (an explicit idle rule
/// with `visible_idle`) clears that.
///
/// A winning `skip_state_update` rule is not silence: it is a pager or a menu over the
/// past, and it says the last reading stands, so it leaves the belief alone.
fn screen_verdict(reading: qoro_detect::Reading, known: bool) -> Option<qoro_detect::Verdict> {
    match reading {
        qoro_detect::Reading::Verdict(v) => Some(v),
        qoro_detect::Reading::Skip { .. } => None,
        qoro_detect::Reading::Nothing if known => {
            Some(qoro_detect::Verdict {
                state: qoro_domain::AgentRuntimeState::Idle,
                rule: "known_agent_idle_fallback".into(),
                visible_blocker: false,
                visible_idle: false,
                visible_working: false,
                // An idle fallback is the absence of any dialog, so it clears a gate
                // rather than holding one.
                setup_gate: false,
            })
        }
        _ => None,
    }
}

/// The agent in a terminal, if what is in it is an agent at all.
fn agent_program(agents: &Arc<crate::agents::AgentRegistry>, stream: u32) -> Option<String> {
    match agents.agent(stream)?.identity {
        qoro_domain::AgentIdentity::Known {
            program,
            agent: true,
            ..
        } => Some(program),
        _ => None,
    }
}

/// Where a terminal starts when nobody said.
///
/// The daemon's own directory, resolved now rather than recorded as a dot: a dot is
/// resolved against whoever reads it, and the next daemon reads it from somewhere else.
fn here() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}

/// Read where a terminal's shell is, and write it to the registry and the store when it
/// moved. The index reads the registry, not the store: one synchronous write from the
/// single writer, ordered with every later read by construction, and live even for a
/// daemon told not to persist.
fn note_cwd(
    child_pid: i32,
    last_cwd: &mut Option<PathBuf>,
    terminals: &TerminalRegistry,
    stream: u32,
    books: &Option<Bookkeeping>,
) {
    let Some(now) = qoro_pty::sys::cwd_of(qoro_pty::teardown::Pid(child_pid)) else {
        return;
    };
    if last_cwd.as_deref() == Some(now.as_path()) {
        return;
    }
    // The first read of a terminal that started without a directory of its own is where it
    // started, not a move: written, it replaced a restored tab's real directory, which had
    // only been slow to answer, with the daemon's.
    let first = last_cwd.is_none();
    *last_cwd = Some(now.clone());
    // The live index is told either way: it is what a new tab or split opened from here
    // follows, and it has to say where the shell is.
    terminals.set_cwd(stream, Some(now.display().to_string()));
    if first {
        return;
    }
    if let Some(books) = books.clone() {
        tokio::spawn(async move {
            let _ = books
                .store
                .set_cwd(stream as i64, now.display().to_string())
                .await;
        });
    }
}

/// Bring back the split trees, cut down to the terminals that came back or are asleep.
///
/// Written back at once, because a dead leaf must not outlive a restart: ids can repeat
/// after one (a deleted workspace takes its rows, highest ids included, with it), and a
/// stale leaf would pull an unrelated new terminal into somebody's tab. A sleeping tab is
/// not a dead leaf: its row is open and its number is kept, and its space's splits have to
/// be there when it wakes. While the daemon runs ids only grow past every open row, so the
/// listing's own cut is enough from here on.
async fn restore_splits(terminals: &TerminalRegistry, books: &Bookkeeping) {
    let store = &books.store;
    let stored: Vec<crate::control::Pane> = store
        .splits()
        .await
        .unwrap_or_default()
        .iter()
        .filter_map(|json| serde_json::from_str(json).ok())
        .collect();
    terminals.load_splits(stored.clone());
    // Every open row, not only the ones that came back: a tab that failed to reopen this
    // start is still a tab, and its split is still its own.
    let Ok(spaces) = store.workspaces().await else {
        return;
    };
    let mut keep = terminals.streams();
    for space in spaces {
        match store.open_terminals(space.id).await {
            Ok(rows) => keep.extend(rows.iter().map(|row| row.id as u32)),
            // Not read, so not known to be dead: nothing is cut this start.
            Err(_) => return,
        }
    }
    let kept = terminals.live_splits(&keep);
    terminals.load_splits(kept.clone());
    if kept != stored {
        let _ = store.set_splits(splits_json(&kept)).await;
    }
}

/// Split trees as the rows the store keeps.
fn splits_json(trees: &[crate::control::Pane]) -> Vec<String> {
    trees
        .iter()
        .filter_map(|tree| serde_json::to_string(tree).ok())
        .collect()
}

/// Everything bringing a session back needs.
/// What a terminal writes down, and whether writing it down still means anything.
#[derive(Clone)]
pub struct Bookkeeping {
    pub store: crate::store::StoreHandle,
    /// Set when the daemon is on its way out. A terminal that ends after this is not a tab
    /// a person closed: recording it as closed is exactly what would stop it coming back.
    pub stopping: Arc<std::sync::atomic::AtomicBool>,
    /// The workspaces asleep, as the store's `asleep` table says, held here because a
    /// terminal's teardown decides whether its row stays open synchronously and cannot
    /// wait on the store. Changed only under that workspace's lock, with the table.
    pub asleep: Arc<std::sync::Mutex<BTreeSet<i64>>>,
    /// Tabs closed for good whose close has not reached the store yet. A wake must not
    /// bring one back in the moment between the tab going and its row saying so.
    pub closing: Arc<std::sync::Mutex<BTreeSet<u32>>>,
    /// Each terminal's history writer, which lets go of its end when it has written its
    /// last row. A tab reopened on the same number waits for it: two writers on one
    /// directory would number their rows over each other.
    pub writers: Arc<std::sync::Mutex<std::collections::HashMap<u32, watch::Receiver<()>>>>,
    /// One lock per workspace, held by whatever changes whether it is asleep: a sleep, a
    /// wake, a tab made or closed in it, the workspace closing. It and `writers` keep an
    /// entry for every id they were asked about: a few bytes each, for the daemon's life.
    locks: Arc<std::sync::Mutex<std::collections::HashMap<i64, Arc<tokio::sync::Mutex<()>>>>>,
}

impl Bookkeeping {
    pub fn new(
        store: crate::store::StoreHandle,
        stopping: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self {
            store,
            stopping,
            asleep: Arc::default(),
            closing: Arc::default(),
            writers: Arc::default(),
            locks: Arc::default(),
        }
    }

    /// Whether a terminal ending now keeps its row open: the daemon is going, or its
    /// workspace is going to sleep.
    fn keeps_row(&self, workspace: Option<i64>) -> bool {
        self.stopping.load(std::sync::atomic::Ordering::SeqCst)
            || workspace.is_some_and(|w| self.is_asleep(w))
    }

    pub fn is_asleep(&self, workspace: i64) -> bool {
        self.asleep.lock().unwrap().contains(&workspace)
    }

    /// The lock of one workspace.
    pub fn lock_of(&self, workspace: i64) -> Arc<tokio::sync::Mutex<()>> {
        self.locks
            .lock()
            .unwrap()
            .entry(workspace)
            .or_default()
            .clone()
    }

    /// Wait until the history writer of a terminal number has finished, or `within` runs
    /// out. True when it has (or there never was one).
    async fn writer_done(&self, stream: u32, within: std::time::Duration) -> bool {
        let Some(mut done) = self.writers.lock().unwrap().get(&stream).cloned() else {
            return true;
        };
        tokio::time::timeout(within, async move { while done.changed().await.is_ok() {} })
            .await
            .is_ok()
    }
}

pub struct Restore {
    pub terminals: Arc<TerminalRegistry>,
    pub books: Bookkeeping,
    pub shell: PathBuf,
    pub warnings: Arc<std::sync::Mutex<Vec<Warning>>>,
    pub history_dir: Option<PathBuf>,
    pub scanner: Arc<ContainmentScanner>,
    pub agents: Arc<crate::agents::AgentRegistry>,
}

/// What a restart brought back, counted by kind rather than as one number.
///
/// A single count hides the failures that matter: a tab that came back as a bare
/// shell where a conversation was, or a tab that moved directory because its own
/// is gone. The report names each, so a person sees what survived and what fell
/// back without opening every tab to find out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RestoreReport {
    /// Workspaces read.
    pub spaces: usize,
    /// Terminal rows recorded as open.
    pub terminals: usize,
    /// Tabs reopened.
    pub brought_back: usize,
    /// Tabs whose directory was gone, started where the daemon is instead.
    pub cwd_fallback: usize,
    /// Tabs where the agent's own resume command was typed back in.
    pub agents_resumed: usize,
    /// Tabs that named an agent but had no resume command for it, so a shell.
    pub agents_shell_fallback: usize,
    /// Tabs with no agent recorded, restored as plain shells.
    pub plain_shells: usize,
}

impl RestoreReport {
    /// One line for the startup log: what survived and what fell back.
    pub fn summary(&self) -> String {
        format!(
            "restore: {}/{} panes, {} agents resumed, {} agent shells, {} plain shells, {} moved directory",
            self.brought_back,
            self.terminals,
            self.agents_resumed,
            self.agents_shell_fallback,
            self.plain_shells,
            self.cwd_fallback,
        )
    }
}

/// Bring back the terminals the last daemon was holding, and say how many.
///
/// What comes back is the tab: its workspace, its place in the order, the directory it was
/// in and the name it was given. What does not come back is whatever was running in it.
/// The saved command line is read and deliberately not run: a tab that was halfway through
/// a deploy, a migration or an `rm` would run it again on every start, and a person who
/// typed it once did not ask for that.
///
/// The one exception is an agent, and it is an exception because an agent has a
/// conversation rather than an effect. A tab that had a known agent in it gets that agent's
/// own resume command typed into its shell, from the agent's manifest, so the conversation
/// carries on where it stopped. The command is typed rather than execed so the person can
/// see what ran, and so the tab is still a shell when the agent exits.
pub async fn restore(into: &Restore) -> usize {
    restore_report(into).await.brought_back
}

/// Bring back the terminals the last daemon was holding, counted by kind.
///
/// The same work as [`restore`], with the categories kept instead of folded into one
/// number. See [`RestoreReport`] for why the split matters.
pub async fn restore_report(into: &Restore) -> RestoreReport {
    let mut report = RestoreReport::default();
    // Above every id ever written, not only the ones coming back: a closed tab keeps its
    // row, and a new tab minted onto its number fails the insert.
    if let Ok(max) = into.books.store.max_terminal_id().await {
        into.terminals
            .mint_above(u32::try_from(max).unwrap_or(u32::MAX));
    }
    let Ok(spaces) = into.books.store.workspaces().await else {
        return report;
    };
    report.spaces = spaces.len();
    // Every open row, for the history sweep below, and whether every list was read: a
    // failed read must never make an open row look like it has no history to keep. A
    // sleeping space's rows are open rows: its history is kept like any other's.
    let mut open = std::collections::BTreeSet::new();
    let mut read_all = true;
    let mut listed = Vec::new();
    for space in spaces {
        let Ok(rows) = into.books.store.open_terminals(space.id).await else {
            read_all = false;
            continue;
        };
        for row in &rows {
            let stream = row.id as u32;
            open.insert(stream);
            // The registry has to keep minting above what came back, or the next new
            // terminal takes the number of one already on screen, or of one asleep.
            into.terminals.mint_above(stream);
        }
        if !into.books.is_asleep(space.id) {
            listed.push((space.id, rows));
        }
    }
    // A directory that is gone is not a reason to lose the tab. The shell starts where the
    // daemon is instead, which is visible and recoverable, unlike a tab that silently did
    // not come back. Every directory is asked about before any tab opens, at once, because
    // this runs before the socket is served: see `existing_dirs`.
    let dirs = existing_dirs(
        listed
            .iter()
            .flat_map(|(_, rows)| rows.iter().map(|row| PathBuf::from(&row.cwd))),
    )
    .await;
    for (space, rows) in listed {
        restore_rows(into, space, rows, &dirs, &mut report).await;
    }
    // History no open row owns: tabs closed before history was deleted with them, tabs
    // slept in the last run, crash leftovers. Before any new id is minted, so a new tab
    // handed a number whose row a closed space removed does not open the old history.
    if read_all && let Some(dir) = into.history_dir.clone() {
        let _ = crate::blocking::run(move || sweep_history(&dir, &open)).await;
    }
    report
}

/// Reopen one workspace's rows: the same numbers, names and directories, and each agent's
/// own resume command typed into its shell. Shared by a restart and by a wake.
async fn restore_rows(
    into: &Restore,
    space: i64,
    rows: Vec<qoro_store::TerminalRow>,
    dirs: &std::collections::BTreeSet<PathBuf>,
    report: &mut RestoreReport,
) {
    for row in rows {
        // A stop that came in meanwhile has already asked every open tab to close, and a
        // tab opened after that would be one it never asks.
        if into
            .books
            .stopping
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            break;
        }
        report.terminals += 1;
        let stream = row.id as u32;
        // The row keeps its directory either way.
        let cwd = PathBuf::from(&row.cwd);
        let cwd = dirs.contains(&cwd).then_some(cwd);
        let opened = open_terminal(
            &into.terminals,
            stream,
            space,
            row.title.clone(),
            &into.shell,
            cwd.as_deref(),
            into.warnings.clone(),
            into.history_dir.clone(),
            into.scanner.clone(),
            into.agents.clone(),
            Some(into.books.clone()),
        );
        let Ok(handle) = opened else {
            continue;
        };
        report.brought_back += 1;
        // The DB row kept the conversation id (COALESCE preserves it),
        // but `open_terminal` starts memory at `None`. Repopulate it so
        // `agents --json` and `history` work right after a restart, before
        // the next hook report. Fresh registry, no token to admit with,
        // so this is a restore not a claim; first hook corrects staleness.
        if let Some(session) = row.agent_session.as_deref()
            && !session.trim().is_empty()
        {
            into.terminals
                .set_session(stream, Some(session.to_string()), None);
        }
        let resume = row
            .agent
            .as_deref()
            .and_then(|agent| qoro_detect::resume_argv(agent, row.agent_session.as_deref()));
        // The resume command is typed into the shell, and the send is what does
        // it: a tab whose send failed holds a shell, not a resumed agent, and the
        // report says so rather than counting an intent as a recovery.
        let mut sent = false;
        if let Some(argv) = resume {
            let mut line = shell_line(&argv);
            line.push('\r');
            sent = handle
                .commands
                .send(ClientCommand::Input {
                    from: ClientId::new("qoro-restore"),
                    bytes: Bytes::from(line),
                })
                .await
                .is_ok();
        }
        match (&row.agent, sent) {
            (Some(_), true) => {
                report.agents_resumed += 1;
            }
            (Some(_), false) => {
                report.agents_shell_fallback += 1;
            }
            (None, _) => {
                report.plain_shells += 1;
            }
        }
        // A row whose directory is gone started where the daemon is instead. Counted
        // here, after the open succeeded, so a tab that never opened is missing from
        // both halves of the summary rather than half reported as moved.
        if cwd.is_none() {
            report.cwd_fallback += 1;
        }
    }
}

/// Why one terminal cannot go to sleep, or what its row has to say when it does.
///
/// A tab can sleep only if it can come back as it is: a shell at its prompt, or a known
/// agent that is idle or done, has told us its conversation in this run of it, and can be
/// resumed into that conversation by id. Anything still running in it, in front or in the
/// background, would be lost, or would outlive the sleep; either is a refusal.
fn vet_for_sleep(
    into: &Restore,
    stream: u32,
    table: &[qoro_pty::sys::ProcRow],
) -> Result<qoro_store::Checkpoint, String> {
    use crate::idle::ForegroundKind as K;
    use qoro_domain::AgentRuntimeState as S;
    let snap = into
        .agents
        .reap_snapshot(stream)
        .ok_or("it has not been read yet")?;
    if snap.boundary {
        return Err("qoro cannot see what runs in it".into());
    }
    if snap.setup_gate {
        return Err("it is waiting on its setup".into());
    }
    let state = format!("{:?}", snap.state).to_lowercase();
    let master = into.agents.master_of(stream);
    let (agent, agent_session) =
        match crate::idle::classify_program(snap.program.as_deref(), snap.agent) {
            K::Absent | K::Shell => {
                if matches!(snap.state, S::Starting | S::Working | S::Blocked) {
                    return Err(format!("it is {state}"));
                }
                if master.is_some_and(crate::idle::group_holds_task) {
                    return Err("a command is running in it".into());
                }
                (None, None)
            }
            K::Agent => {
                let name = agent_program(&into.agents, stream)
                    .or(snap.program.clone())
                    .unwrap_or_default();
                if !matches!(snap.state, S::Idle | S::Done) {
                    return Err(format!("{name} is {state}"));
                }
                let handle = into.terminals.get(stream).ok_or("it has gone")?;
                let session = handle
                    .session
                    .clone()
                    .filter(|_| handle.session_generation == Some(snap.generation.0))
                    .ok_or(format!(
                        "{name} has not said which conversation it is in (`qoro hooks install`)"
                    ))?;
                if !qoro_detect::resumes_by_id(&name) {
                    return Err(format!(
                        "{name} cannot be resumed into a given conversation"
                    ));
                }
                (Some(name), Some(session))
            }
            K::Other => {
                return Err(format!(
                    "{} is running in it",
                    snap.program.as_deref().unwrap_or("a command")
                ));
            }
        };
    let (leader, tty) = into
        .agents
        .leader_of(stream)
        .ok_or("its process is not known")?;
    let foreground = master
        .and_then(qoro_pty::sys::foreground_pgrp)
        .map(|group| group.0);
    let blockers = crate::idle::sleep_blockers(table, leader, foreground, tty);
    if !blockers.is_empty() {
        return Err(format!("{} running in the background", blockers.join(", ")));
    }
    Ok(qoro_store::Checkpoint {
        terminal: stream as i64,
        agent,
        agent_session,
    })
}

/// Put one workspace to sleep. Answers how many tabs ended. See
/// `ControlRequest::SleepWorkspace`.
async fn sleep_space(into: &Restore, workspace: i64) -> Result<usize, String> {
    let lock = into.books.lock_of(workspace);
    let _held = lock.lock().await;
    if into
        .books
        .stopping
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        return Err("qoro is stopping".into());
    }
    let live = |into: &Restore| -> Vec<u32> {
        into.terminals
            .streams()
            .into_iter()
            .filter(|s| into.terminals.workspace_of(*s) == Some(workspace))
            .collect()
    };
    // Asleep already: nothing to vet, and whatever of it is still running is a sleep that
    // did not finish, ended now.
    if into.books.is_asleep(workspace) {
        let streams = live(into);
        return end_tabs(into, &streams).await.map(|()| 0);
    }
    let known = into
        .books
        .store
        .workspaces()
        .await
        .map_err(|e| format!("the spaces could not be read: {e}"))?;
    if !known.iter().any(|space| space.id == workspace) {
        return Err(format!("there is no space {workspace}"));
    }
    let streams = live(into);
    if streams.is_empty() {
        return Err(format!("nothing in space {workspace} is running"));
    }
    // A tab waiting on another, or waited on by one, is part of a conversation between
    // agents that a sleep would cut in half, and the wait itself is not written anywhere.
    let edges: Vec<(u32, u32)> = into
        .agents
        .expectation_edges()
        .into_iter()
        .filter(|(_, target)| !into.agents.is_closed(*target))
        .collect();
    if let Some((waiter, target)) = edges
        .iter()
        .find(|(waiter, target)| streams.contains(waiter) || streams.contains(target))
    {
        return Err(format!(
            "space {workspace} stays awake: tab {waiter} is waiting on tab {target}"
        ));
    }
    // Read now, not from the last scan: a job started a moment ago is exactly the one a
    // sleep must not miss.
    let table =
        crate::blocking::run_within(crate::store::STORE_DEADLINE, qoro_pty::sys::process_table)
            .await
            .map_err(|_| "the process table could not be read".to_string())?;
    let vet = |into: &Restore| -> Result<Vec<qoro_store::Checkpoint>, String> {
        let mut checkpoints = Vec::new();
        let mut refused = Vec::new();
        for stream in &streams {
            match vet_for_sleep(into, *stream, &table) {
                Ok(checkpoint) => checkpoints.push(checkpoint),
                Err(why) => refused.push(format!("tab {stream}: {why}")),
            }
        }
        if refused.is_empty() {
            Ok(checkpoints)
        } else {
            Err(format!(
                "space {workspace} stays awake: {}",
                refused.join("; ")
            ))
        }
    };
    let checkpoints = vet(into)?;
    let written = into
        .books
        .store
        .sleep_workspace(workspace, checkpoints.clone())
        .await;
    // Looked at again after the write, the whole vet: a command typed, an agent that
    // started working, a new conversation in a tab, in the moment the write took, would be
    // put down mid-thought or brought back into the wrong conversation.
    let still = vet(into);
    if written.is_err() || still.as_ref() != Ok(&checkpoints) {
        // A write that timed out still lands later; taking the mark off after it is what
        // keeps the table and this daemon from disagreeing about an awake space.
        let _ = into.books.store.wake_workspace(workspace).await;
        return Err(match (written, still) {
            (Err(e), _) => format!("the sleep could not be written down: {e}"),
            (_, Err(why)) => why,
            _ => format!("space {workspace} stays awake: a tab changed while it was written down"),
        });
    }
    into.books.asleep.lock().unwrap().insert(workspace);
    end_tabs(into, &streams).await?;
    Ok(streams.len())
}

/// Ask each tab to end, and wait for it and its history to be gone, under one deadline for
/// them all. A tab still running at the end is said by number; the space stays marked
/// asleep, so asking again finishes it.
async fn end_tabs(into: &Restore, streams: &[u32]) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + SHUTDOWN_PATIENCE;
    for stream in streams {
        if let Some(handle) = into.terminals.get(*stream) {
            let _ =
                tokio::time::timeout_at(deadline, handle.commands.send(ClientCommand::Close)).await;
        }
    }
    let deadline = tokio::time::Instant::now() + SHUTDOWN_PATIENCE;
    while streams.iter().any(|s| into.terminals.get(*s).is_some())
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    if let Some(stuck) = streams.iter().find(|s| into.terminals.get(**s).is_some()) {
        return Err(format!(
            "tab {stuck} did not end in time; asking again finishes it"
        ));
    }
    for stream in streams {
        if !into.books.writer_done(*stream, SHUTDOWN_PATIENCE).await {
            eprintln!("qorod: the history of tab {stream} was still being written at its sleep");
        }
    }
    Ok(())
}

/// Close a tab of a sleeping space for good: its row closed and its history removed, and
/// the space left asleep. False when no sleeping space holds the tab.
async fn close_sleeping_tab(
    books: &Bookkeeping,
    history_dir: Option<&Path>,
    terminal: u32,
) -> Result<bool, String> {
    let asleep: Vec<i64> = books.asleep.lock().unwrap().iter().copied().collect();
    for workspace in asleep {
        let lock = books.lock_of(workspace);
        let _held = lock.lock().await;
        // Read under the lock: a wake that got there first has taken the tab back.
        if !books.is_asleep(workspace) {
            continue;
        }
        let rows = books
            .store
            .open_terminals(workspace)
            .await
            .map_err(|e| format!("the sleeping tabs could not be read: {e}"))?;
        if !rows.iter().any(|row| row.id == terminal as i64) {
            continue;
        }
        books
            .store
            .close_terminal(terminal as i64)
            .await
            .map_err(|e| format!("tab {terminal} could not be closed: {e}"))?;
        if let Some(dir) = history_dir.map(|dir| dir.join(terminal.to_string())) {
            let _ = crate::blocking::run(move || std::fs::remove_dir_all(dir)).await;
        }
        return Ok(true);
    }
    Ok(false)
}

/// Wake one workspace, or the one that went to sleep last. Answers which, and what came
/// back. See `ControlRequest::WakeWorkspace`.
async fn wake_space(
    into: &Restore,
    workspace: Option<i64>,
) -> Result<(i64, RestoreReport), String> {
    let workspace = match workspace {
        Some(workspace) => workspace,
        None => into
            .books
            .store
            .asleep()
            .await
            .map_err(|e| format!("the sleeping spaces could not be read: {e}"))?
            .first()
            .map(|(workspace, _)| *workspace)
            .ok_or("nothing is asleep")?,
    };
    let lock = into.books.lock_of(workspace);
    let _held = lock.lock().await;
    wake_locked(into, workspace)
        .await
        .map(|report| (workspace, report))
}

/// The wake itself, with the workspace's lock already held.
async fn wake_locked(into: &Restore, workspace: i64) -> Result<RestoreReport, String> {
    if into
        .books
        .stopping
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        return Err("qoro is stopping".into());
    }
    if !into.books.is_asleep(workspace) {
        // Awake here; the table is told so as well, which is what brings the two back
        // together after a sleep whose write timed out and landed late.
        let _ = into.books.store.wake_workspace(workspace).await;
        return Ok(RestoreReport::default());
    }
    // A tab of a sleep that has not finished is still ending: waited out, because reopening
    // its row now would race its teardown into closing it for good.
    let deadline = tokio::time::Instant::now() + SHUTDOWN_PATIENCE;
    let ending = || {
        into.terminals
            .streams()
            .into_iter()
            .find(|s| into.terminals.workspace_of(*s) == Some(workspace))
    };
    while ending().is_some() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    if let Some(stuck) = ending() {
        return Err(format!(
            "tab {stuck} is still going to sleep; try again in a moment"
        ));
    }
    let rows = into
        .books
        .store
        .open_terminals(workspace)
        .await
        .map_err(|e| format!("the space's tabs could not be read: {e}"))?;
    // Read after the store answered: a close that landed meanwhile is already in the set.
    let closing = into.books.closing.lock().unwrap().clone();
    let rows: Vec<qoro_store::TerminalRow> = rows
        .into_iter()
        .filter(|row| {
            let stream = row.id as u32;
            into.terminals.get(stream).is_none() && !closing.contains(&stream)
        })
        .collect();
    for row in &rows {
        if !into
            .books
            .writer_done(row.id as u32, std::time::Duration::from_secs(30))
            .await
        {
            return Err(format!(
                "tab {} is still writing down its history; try again in a moment",
                row.id
            ));
        }
    }
    let dirs = existing_dirs(rows.iter().map(|row| PathBuf::from(&row.cwd))).await;
    let mut report = RestoreReport::default();
    let first = rows.first().map(|row| row.id as u32);
    restore_rows(into, workspace, rows, &dirs, &mut report).await;
    // The mark comes off last: until then a late write about the tabs that slept still
    // meets it, and the rows read above are what came back.
    let unmarked = into.books.store.wake_workspace(workspace).await;
    into.books.asleep.lock().unwrap().remove(&workspace);
    // The index its splits were held out of is asked for again, now whole.
    if let Some(first) = first {
        into.agents.announce(first);
    }
    unmarked
        .map_err(|e| format!("the space is up, but its sleep mark could not be taken off: {e}"))?;
    Ok(report)
}

/// Close every descriptor the daemon was started with past stdin, stdout and stderr.
///
/// The daemon outlives whoever started it, and every tab's shell inherits what it holds, so
/// a pipe that leaked into the starter (a `$(qoro ...)` in a script, or another thread's
/// pipe caught mid-spawn) would otherwise never see its end and its reader waits forever.
/// Call it first thing, before anything of the daemon's own is open.
pub fn close_inherited_fds() {
    let Ok(entries) = std::fs::read_dir("/dev/fd") else {
        return;
    };
    let fds: Vec<i32> = entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse().ok())
        .filter(|&fd| fd > 2)
        .collect();
    // The listing's own descriptor is among them and is already closed: EBADF, harmless,
    // since nothing else opens anything in between.
    for fd in fds {
        // SAFETY: nothing in this process owns a descriptor above 2 yet.
        unsafe { libc::close(fd) };
    }
}

/// How long nothing has to be awake, with nobody attached, before the daemon leaves.
const LEAVE_AFTER: std::time::Duration = std::time::Duration::from_secs(10);

/// What the leaving watch reads.
struct Leaving {
    terminals: Arc<TerminalRegistry>,
    connections: Arc<ConnectionRegistry>,
    agents: Arc<crate::agents::AgentRegistry>,
    books: Bookkeeping,
    socket: PathBuf,
}

impl Leaving {
    /// Nothing awake, nobody attached, and not already on the way out.
    fn quiet(&self) -> bool {
        self.terminals.is_empty()
            && self.connections.holders() == 0
            && !self
                .books
                .stopping
                .load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Leave once nothing has been awake and nobody attached for `LEAVE_AFTER`, so a machine
/// whose spaces are all asleep costs nothing at all.
///
/// Woken by what changes the answer, never by a timer while something is awake: a tab
/// coming or going, a connection coming, being welcomed or going. The countdown starts when
/// the daemon falls quiet and is not restarted by a watcher reconnecting, which would
/// otherwise keep a daemon up by watching it. `daemon.exit_when_asleep = false` in the
/// config keeps it up.
async fn leave_when_idle(watch: Leaving) {
    let mut changes = watch.agents.subscribe();
    loop {
        while !watch.quiet() {
            tokio::select! {
                _ = watch.connections.changed.notified() => {}
                changed = changes.recv() => {
                    if matches!(changed, Err(tokio::sync::broadcast::error::RecvError::Closed)) {
                        return;
                    }
                }
            }
        }
        if !crate::config::read().exit_when_asleep {
            watch.connections.changed.notified().await;
            continue;
        }
        let deadline = tokio::time::Instant::now() + LEAVE_AFTER;
        let lasted = loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => break true,
                _ = watch.connections.changed.notified() => {}
                changed = changes.recv() => {
                    if matches!(changed, Err(tokio::sync::broadcast::error::RecvError::Closed)) {
                        return;
                    }
                }
            }
            if !watch.quiet() {
                break false;
            }
        };
        if lasted && leave(&watch).await {
            return;
        }
    }
}

/// Go, unless something came in the meantime; false then, and the daemon carries on.
///
/// Under the startup lock, so a daemon started meanwhile waits for this one to be gone
/// rather than binding beside it; checked again under it, after every connection already
/// accepted has been welcomed or has gone. Then new connections are closed as they come and
/// the socket goes, so the next client starts the next daemon, which the lock holds until
/// this process has exited.
async fn leave(watch: &Leaving) -> bool {
    let Some(dir) = watch.socket.parent().map(Path::to_path_buf) else {
        return false;
    };
    let lock = tokio::task::spawn_blocking(move || {
        crate::lock::StartupLock::acquire(&crate::paths::lock_path(&dir))
    })
    .await;
    let Ok(Ok(lock)) = lock else {
        return false;
    };
    let deadline = tokio::time::Instant::now() + qoro_proto::reassembly::SEQUENCE_DEADLINE;
    while watch.connections.pending() > 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    if !watch.quiet() || !watch.connections.close_if_unheld() {
        drop(lock);
        return false;
    }
    let _ = std::fs::remove_file(&watch.socket);
    watch
        .books
        .stopping
        .store(true, std::sync::atomic::Ordering::SeqCst);
    crate::store::settle_owed(tokio::time::Instant::now() + SHUTDOWN_PATIENCE).await;
    eprintln!("qorod: nothing is awake and nobody is attached; leaving");
    // The lock goes with the process, and with it the next daemon may bind.
    std::process::exit(0);
}

/// Remove every terminal's history directory under `dir` whose id is not in `open`.
///
/// Only numbered entries: anything else in the directory is not a terminal's history and
/// is not this sweep's to remove.
fn sweep_history(dir: &Path, open: &std::collections::BTreeSet<u32>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let orphan = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
            .is_some_and(|id| !open.contains(&id));
        if orphan {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// One command line, quoted so a shell runs the argv that was meant.
///
/// A resume command carries a session id that came from an agent, and a directory name
/// that came from a person. Neither is guaranteed to be free of the characters a shell
/// reads, and this text is typed into a shell.
fn shell_line(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| {
            if !arg.is_empty()
                && arg
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./:=@+,".contains(c))
            {
                arg.clone()
            } else {
                format!("'{}'", arg.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// What to say about a control request this daemon could not read.
///
/// A serde error is the truth and not the answer. The one that actually happens is a
/// client newer than the daemon it found, because a daemon keeps running across an
/// upgrade: the binary on disk changed and the process in memory did not. `unknown variant
/// RenameWorkspace, expected one of ...` is that situation, and it reads like a bug in the
/// client rather than like a daemon that has been running since before the feature
/// existed. So it is named, and the fix is given, because the fix is one command.
pub(crate) fn unparsable(error: &str) -> String {
    if error.contains("unknown variant") {
        return format!(
            "this daemon is older than the client that called it: it does not know the \
             method that was asked for ({error}). `qoro stop`, then `qoro`, picks up the new \
             daemon; what is open comes back."
        );
    }
    format!("malformed control request: {error}")
}

/// The most rows one scrollback request may ask for.
///
/// Public because a client has to ask for a range that fits inside it: the truncation here
/// keeps the oldest end of what was asked for, so a client that asked for more than this
/// would lose the newest rows, which are the ones next to the screen.
///
/// A window's worth and then some. The count is a number from a client, and a client is
/// not to be trusted with how much memory the daemon allocates.
pub const MAX_SCROLLBACK_ROWS: usize = 500;

/// How long one search step reads before it answers with where to carry on.
///
/// A tenth of a second is under what a person notices as a wait, and long enough to cover
/// hundreds of thousands of rows: most searches end in the first step.
pub const SEARCH_STEP: std::time::Duration = std::time::Duration::from_millis(100);

/// How many bytes of rows one search step reads, whatever the clock says.
///
/// The clock alone lets a fast disk read a gigabyte between two answers, and the step
/// exists so that a search costs about the same on every machine: a person pressing `n`
/// again is cheaper than a daemon reading history nobody asked to see.
pub const SEARCH_BYTES: u64 = 64 * 1024 * 1024;

/// Rows read per disk call while searching: large enough that the seek and the segment
/// listing are a rounding error, small enough that a step overshoots its budget by little.
const SEARCH_CHUNK: u64 = 16_384;

/// How long a check of a directory the user chose may take before it is abandoned.
const PROBE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);

/// Which of `paths` are directories, every one probed at once under the same deadline.
///
/// Restore runs before the socket is served, and a client's handshake deadline
/// (`SEQUENCE_DEADLINE`, ten seconds) covers all of it. One probe after another cost a
/// `PROBE_DEADLINE` per tab on a dead mount, so a handful of such tabs failed the attach of
/// a daemon that was only busy; at once, the start waits one deadline however many there
/// are. Each distinct path is probed once, so tabs sharing a dead directory hold one probe
/// thread between them, not one each.
///
/// A path left out is gone, or did not answer in time, or found the pool busy. None of
/// those is spawned into: an unchecked path's `chdir` would run on this thread.
async fn existing_dirs(
    paths: impl IntoIterator<Item = PathBuf>,
) -> std::collections::BTreeSet<PathBuf> {
    let mut probes = tokio::task::JoinSet::new();
    for path in paths.into_iter().collect::<std::collections::BTreeSet<_>>() {
        probes.spawn(async move { matches!(is_dir_probed(&path).await, Ok(true)).then_some(path) });
    }
    let mut found = std::collections::BTreeSet::new();
    while let Some(probed) = probes.join_next().await {
        if let Ok(Some(path)) = probed {
            found.insert(path);
        }
    }
    found
}

/// Whether a directory a person is waiting on exists: like [`is_dir_probed`], but never
/// queued behind stuck probes.
async fn probe_dir_asked(path: &Path) -> Result<bool, crate::blocking::BlockingError> {
    let owned = path.to_path_buf();
    crate::blocking::probe_asked(&path.display().to_string(), PROBE_DEADLINE, move || {
        owned.is_dir()
    })
    .await
}

/// Whether a path the user chose is a directory, asked on the probe pool (ADR-011).
///
/// Keyed by the full path, so a mount that hangs degrades only itself and what is below it.
async fn is_dir_probed(path: &Path) -> Result<bool, crate::blocking::BlockingError> {
    let owned = path.to_path_buf();
    crate::blocking::probe(&path.display().to_string(), PROBE_DEADLINE, move || {
        owned.is_dir()
    })
    .await
}

/// Where the history of a tab of a sleeping space lives, when this is one. Only a tab a
/// sleeping space holds: opening a history creates its directory, so any other id would
/// leave an empty one behind.
async fn sleeping_history_of(
    books: &Bookkeeping,
    history_dir: Option<&PathBuf>,
    terminal: u32,
) -> Option<PathBuf> {
    let dir = history_dir?.join(terminal.to_string());
    let asleep: Vec<i64> = books.asleep.lock().unwrap().iter().copied().collect();
    for workspace in asleep {
        let Ok(rows) = books.store.open_terminals(workspace).await else {
            continue;
        };
        if rows.iter().any(|row| row.id == terminal as i64) {
            return Some(dir);
        }
    }
    None
}

/// Where a terminal's history lives, or why it cannot be read.
///
/// A terminal that does not exist is a refusal, not an empty answer. Opening a history
/// creates the directory it is given, so answering `{total: 0}` for an id nobody has would
/// quietly leave an empty directory per bad request.
fn history_of(
    terminals: &TerminalRegistry,
    history_dir: Option<&PathBuf>,
    terminal: u32,
) -> Result<PathBuf, String> {
    if terminals.get(terminal).is_none() {
        return Err(format!("there is no terminal {terminal}"));
    }
    history_dir
        .map(|d| d.join(terminal.to_string()))
        .ok_or_else(|| "this daemon keeps no history".to_string())
}

/// One search step over a terminal's history, answered as a `ScrollbackMatch`.
///
/// On the blocking pool with a deadline, like every other read of a disk (ADR-011). `stop`
/// is asked between chunks: true once the search is moot, and the step then answers with
/// where it got to like a step that ran out of budget.
async fn search_scrollback(
    id: String,
    terminal: u32,
    query: String,
    from: u64,
    backward: bool,
    dir: PathBuf,
    stop: impl Fn() -> bool + Send + 'static,
) -> ControlResponse {
    if query.is_empty() {
        return ControlResponse::Failed {
            id,
            reason: "there is nothing to search for".to_string(),
        };
    }
    let scan = crate::blocking::run_within(crate::store::STORE_DEADLINE, move || {
        let history = qoro_store::History::open(&dir)?;
        scan_history(
            &history,
            &query,
            from,
            backward,
            SEARCH_STEP,
            SEARCH_BYTES,
            stop,
        )
    })
    .await;
    match scan {
        Ok(Ok((found, resume))) => ControlResponse::ScrollbackMatch {
            id,
            terminal,
            row: found.map(|(row, _)| row),
            col: found.map_or(0, |(_, cell)| cell),
            resume,
        },
        Ok(Err(e)) => ControlResponse::Failed {
            id,
            reason: format!("the scrollback could not be searched: {e}"),
        },
        _ => ControlResponse::Failed {
            id,
            reason: "the scrollback search did not answer in time".to_string(),
        },
    }
}

/// Where a search step got to: the match as (row, cell), and where the next step starts.
type SearchStep = (Option<(u64, usize)>, Option<u64>);

/// Look through a history from row `from`, toward older rows with `backward`, until a row
/// holds `query` or a budget runs out.
///
/// Answers the match as (row, cell), and where the next step should start when a budget
/// ran out first. Both `None`: the history was searched to its end in that direction.
pub(crate) fn scan_history(
    history: &qoro_store::History,
    query: &str,
    from: u64,
    backward: bool,
    time: std::time::Duration,
    bytes: u64,
    stop: impl Fn() -> bool,
) -> Result<SearchStep, qoro_store::StoreError> {
    let total = history.len();
    if total == 0 || (!backward && from >= total) {
        return Ok((None, None));
    }
    let started = std::time::Instant::now();
    let mut read = 0u64;
    // Nothing below the oldest row still kept: past the cap the oldest segments are gone,
    // and a chunk that started below them would be read from the first kept row while
    // being labelled from where it was asked.
    let first = history.first_ordinal()?;
    if total <= first {
        return Ok((None, None));
    }
    // The next row to look at.
    let mut at = from.min(total - 1).max(first);
    loop {
        let start = if backward {
            (at + 1).saturating_sub(SEARCH_CHUNK).max(first)
        } else {
            at
        };
        let count = if backward {
            at + 1 - start
        } else {
            SEARCH_CHUNK.min(total - at)
        };
        let rows = history.read(start, count as usize)?;
        if rows.is_empty() {
            return Ok((None, None));
        }
        for k in 0..rows.len() {
            let i = if backward { rows.len() - 1 - k } else { k };
            read += rows[i].len() as u64;
            if let Some(cell) = find_in_row(&rows[i], query, None, backward) {
                return Ok((Some((start + i as u64, cell)), None));
            }
        }
        if backward {
            let Some(next) = start.checked_sub(1).filter(|next| *next >= first) else {
                return Ok((None, None));
            };
            at = next;
        } else {
            at = start + rows.len() as u64;
            if at >= total {
                return Ok((None, None));
            }
        }
        if stop() || started.elapsed() >= time || read >= bytes {
            return Ok((None, Some(at)));
        }
    }
}

/// Where `query` occurs in one row, as a cell: one per character of the row, which is how
/// a grid row is stored.
///
/// With `bound`, the first match starting after that cell, or with `backward` the last one
/// starting before it; without, the first or last in the whole row. Smart case, as in vim
/// and less: a query with a capital letter in it means exactly that, and one without means
/// any case.
///
/// Public because the client searches the rows on its screen with the same rule before it
/// asks for the history, and two rules would find two different things.
pub fn find_in_row(row: &str, query: &str, bound: Option<usize>, backward: bool) -> Option<usize> {
    if query.is_empty() {
        return None;
    }
    let exact = query.chars().any(char::is_uppercase);
    let keep = |cell: usize| match (bound, backward) {
        (None, _) => true,
        (Some(bound), false) => cell > bound,
        (Some(bound), true) => cell < bound,
    };
    // Plain ASCII, which is nearly every row a search reads: a byte is a cell and there is
    // no spacer to step over, so the match is a byte comparison.
    if row.is_ascii() && query.is_ascii() {
        let mut starts: Box<dyn Iterator<Item = usize> + '_> = if exact {
            Box::new(row.match_indices(query).map(|(at, _)| at))
        } else {
            let (q, bytes) = (query.as_bytes(), row.as_bytes());
            Box::new(
                (0..=bytes.len().saturating_sub(q.len())).filter(move |&at| {
                    bytes.len() >= q.len() && bytes[at..at + q.len()].eq_ignore_ascii_case(q)
                }),
            )
        };
        return if backward {
            starts.filter(|&cell| keep(cell)).last()
        } else {
            starts.find(|&cell| keep(cell))
        };
    }
    // Anything else is matched on the characters as drawn: the spacer cell the grid keeps
    // after a wide character is stepped over, or `世界` would never match the stored
    // `世 界 `, and each match is mapped back to the cell it starts in.
    let mut after_wide = false;
    let drawn: Vec<(char, usize)> = row
        .chars()
        .enumerate()
        .filter(|(_, c)| {
            let spacer = after_wide && *c == ' ';
            after_wide = !spacer && unicode_width::UnicodeWidthChar::width(*c) == Some(2);
            !spacer
        })
        .map(|(cell, c)| (c, cell))
        .collect();
    let q: Vec<char> = query.chars().collect();
    let same = |c: char, want: char| {
        if exact {
            c == want
        } else {
            c.to_lowercase().eq(want.to_lowercase())
        }
    };
    let mut starts = (0..(drawn.len() + 1).saturating_sub(q.len()))
        .filter(|&at| {
            q.iter()
                .enumerate()
                .all(|(i, want)| same(drawn[at + i].0, *want))
        })
        .map(|at| drawn[at].1);
    if backward {
        starts.rev().find(|&cell| keep(cell))
    } else {
        starts.find(|&cell| keep(cell))
    }
}

/// How long a shutdown waits for its terminals to end before going anyway.
///
/// Long enough for a shell to run its own exit, short enough that a person who typed
/// `qoro stop` gets their prompt back. A terminal still there at the end of it loses its
/// teardown and gets the hangup the closing pty sends, which is what would have happened
/// anyway had the daemon been killed.
const SHUTDOWN_PATIENCE: std::time::Duration = std::time::Duration::from_secs(3);

/// The half of the history path that is allowed to wait on a disk.
///
/// One task per terminal, on the async runtime, going to the blocking pool only when it
/// actually writes. Not one blocking task per terminal: that would make the thread count
/// grow with the terminal count, which is exactly what gate G2.1 forbids.
fn spawn_history_writer(
    dir: PathBuf,
    spill: crate::spill::SpillRing,
    rows_behind: Arc<std::sync::atomic::AtomicU64>,
    commands: mpsc::WeakSender<ClientCommand>,
    done: watch::Sender<()>,
) {
    let owed = crate::store::Owed::new();
    tokio::spawn(async move {
        let _owed = owed;
        // Dropped with the task, however it ends: that is the signal a reopen waits for.
        let _done = done;
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
        // What a restored tab already has on disk is behind its screen from the first
        // snapshot: published only after the next append, a tab that came back quiet
        // could not be scrolled into at all.
        rows_behind.store(history.len(), std::sync::atomic::Ordering::Relaxed);
        // The screen that went out before this count was known said nothing is behind it,
        // and a quiet tab would not send another: ask for one that says what is.
        if !history.is_empty()
            && let Some(commands) = commands.upgrade()
        {
            let _ = commands.try_send(ClientCommand::Refresh);
        }
        drop(commands);
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
            // Asked before the take: asked after, the terminal could push its last rows
            // and go in between, and the writer would leave them in the ring.
            let finished = spill.orphaned();
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
                // The terminal is gone and took its handle with it: nothing will arrive
                // again, so flush what is buffered and let the files go.
                if unflushed && (!woken || finished) {
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
                if finished {
                    return;
                }
                continue;
            }
            // A burst becomes one batch. Waking on every notify makes the batches as small
            // as the reads that produced them, and a batch is a blocking dispatch and a set
            // of syscalls however few rows are in it. Unless the burst is already filling
            // the ring: then waiting is how rows get displaced, so the writer drains now.
            if !spill.pressed()
                && rows.iter().map(String::len).sum::<usize>() < crate::spill::SPILL_PRESSURE
            {
                tokio::select! {
                    _ = tokio::time::sleep(HISTORY_BATCH_WINDOW) => {}
                    _ = spill.wait_pressure() => {}
                }
            }
            let mut rows = rows;
            let (more, later) = spill.take();
            rows.extend(more);
            if later.rows > 0 {
                // The same record as above: a loss inside the window is still a loss.
                eprintln!(
                    "qorod: history for {} lost {} rows between {} and {}",
                    dir.display(),
                    later.rows,
                    later.first_unix,
                    later.last_unix
                );
            }
            let written = crate::blocking::run(move || {
                let truncated = history.append(&rows);
                (history, truncated)
            })
            .await;
            match written {
                Ok((h, Ok(truncated))) => {
                    history = h;
                    unflushed = true;
                    // How many rows are behind the screen now. Published here because this
                    // is the one place that knows, and read by every snapshot, so a client
                    // scrolled back can hold its place while the terminal keeps printing
                    // rather than being dragged along by it.
                    rows_behind.store(history.len(), std::sync::atomic::Ordering::Relaxed);
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
                    // The last rows of a tab closed for good race the removal of its
                    // directory, and losing them there is the point, not a failure.
                    let gone = finished && !dir.exists();
                    if !gone {
                        eprintln!("qorod: history write failed: {e}");
                    }
                }
                Err(_) => return,
            }
            // The last batch of a terminal that is gone, written or not: nothing will
            // wake this again, so it ends here rather than parking for good, which a
            // failed write (a full disk) otherwise made it do.
            if finished {
                if unflushed {
                    let _ = crate::blocking::run(move || history.flush()).await;
                }
                return;
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
    /// The live index, so the terminal's own task can publish where it is
    /// without going through the store. The task is the single writer.
    pub terminals: Arc<TerminalRegistry>,
    /// Where what this terminal is and where it is get recorded, so a restart knows what
    /// to bring back. Absent for a daemon told not to persist.
    pub books: Option<Bookkeeping>,
}

#[allow(clippy::too_many_arguments)]
fn spawn_terminal(
    spawn: TerminalSpawn,
    from_clients: mpsc::Receiver<ClientCommand>,
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
        terminals,
        books,
    } = spawn;
    let register_master = agents.clone();
    // The directory the row already holds for this terminal, when one was asked for. None
    // when it starts where the daemon is: a restored tab whose directory did not answer in
    // time keeps its real one in the row, and must not have it replaced by where it
    // happened to start.
    let started_in = cwd.clone();
    let shell = shell.as_path();
    let cwd = cwd.as_deref();
    let pty = sys::open_pty(INITIAL_SIZE.cols, INITIAL_SIZE.rows)?;
    let master_fd = pty.master;
    let slave_fd = pty.slave;

    let mut cmd = std::process::Command::new(shell);
    cmd.arg("-i");
    // SAFETY: the closure runs post-fork and calls only async-signal-safe syscalls, with no
    // allocation. setsid plus TIOCSCTTY is what gives the child its own session with this
    // pty as its controlling terminal, which is the containment identity teardown uses.
    // The signal reset is the same kind of hygiene: a daemon started in the background
    // (`&`, launchd, systemd) carries SIGINT and SIGQUIT as ignored, `exec` preserves an
    // ignored disposition, and an interactive shell does not heal those two. Without the
    // reset every shell and everything it runs ignores Ctrl-C forever, while Ctrl-Z keeps
    // working, which reads as a broken key rather than an inherited disposition.
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
                // `signal` is async-signal-safe and SIG_DFL is always a valid disposition.
                libc::signal(libc::SIGINT, libc::SIG_DFL);
                libc::signal(libc::SIGQUIT, libc::SIG_DFL);
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
    register_master.attach_leader(stream, child.id() as i32, sys::scan_key(tty_dev));

    // What the supervisor needs if the task below panics: the nested task dies
    // with the child handle, so the ordered teardown never runs. The tty
    // identity, the child's pid with its start time, and a clone of the shared
    // scanner stay outside the panic boundary, which is what the fallback
    // teardown kills the tree with.
    let fallback_scanner = scanner.clone();
    let fallback_pid = child.id();
    let fallback_start = i32::try_from(fallback_pid)
        .ok()
        .and_then(|pid| qoro_pty::sys::start_time_of(qoro_pty::teardown::Pid(pid)));
    // The teardown below runs after the task (and its locals) is gone. When the
    // last update was a picture, which carries no grid, the end still needs the
    // live geometry: read it from the handle now, while the registry has it.
    let end_dims = terminals.get(stream).map(|h| h.last_dims.clone());
    tokio::spawn(async move {
        // A panic in one terminal's task must not take the daemon or its neighbours
        // with it, and must not leave a corpse tab behind either: without the
        // containment below, the task dies, `on_end` never runs, the registry keeps
        // a dead handle, and the tab reads as alive while nothing runs in it. The
        // join is the containment: a panic arrives here as an error rather than
        // unwinding the supervisor.
        let outcome = tokio::spawn(terminal_task(
            master_fd,
            child,
            tty_dev,
            from_clients,
            updates.clone(),
            warnings,
            spill,
            scanner,
            agents.clone(),
            stream,
            terminals,
            books,
            started_in,
        ))
        .await;
        // Again for a panic, whose unwinding closed the descriptor without the line at the
        // end of the task: the fallback teardown below takes seconds, and the reaper and
        // the foreground re-read would read a number that may be another terminal's.
        agents.detach_master(stream);
        match classify_task_end(outcome) {
            TaskEnd::Clean => {}
            TaskEnd::Ended { error } => {
                eprintln!("qorod: terminal ended: {error}");
            }
            TaskEnd::Panicked => {
                agents.announce(stream);
                // The ordered teardown died with the task, so the tree would
                // otherwise survive with no tab: kill it by tty identity before
                // the tab is deregistered below.
                let seen =
                    fallback_teardown(&fallback_scanner, tty_dev, fallback_pid, fallback_start)
                        .await;
                eprintln!(
                    "qorod: terminal {stream} task panicked; contained ({seen} tracked processes)"
                );
            }
        }
        // Deregistered before the last update, so a client that reacts to `Gone` by asking
        // for the terminal cannot find a handle whose task has already returned.
        on_end();
        let last = match updates.borrow().clone() {
            TerminalUpdate::Full(snapshot) | TerminalUpdate::Ended(snapshot) => snapshot,
            // The last update was a picture, which carries no grid. The client that
            // watched it still holds the last screen it drew; the end carries a blank
            // one of the live geometry rather than a guessed one.
            TerminalUpdate::Image(_) => {
                let packed = end_dims
                    .as_ref()
                    .map(|d| d.load(std::sync::atomic::Ordering::Relaxed));
                let (cols, rows) = packed
                    .map(|p| ((p >> 32) as usize, (p as u32) as usize))
                    .unwrap_or((80, 24));
                let (cols, rows) = (cols.max(1), rows.max(1));
                Snapshot {
                    cols,
                    rows,
                    cursor: (0, 0),
                    lines: vec![String::new(); rows],
                    styles: Vec::new(),
                    modes: Default::default(),
                }
            }
        };
        let _ = updates.send(TerminalUpdate::Ended(last));
    });
    Ok(())
}

/// How one terminal's task ended, after panic containment.
///
/// A task that returns is over however it returned; a task that panicked is over
/// however loudly. The distinction matters because only the last one means the
/// runtime itself broke, which is what the Failed observation reports.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TaskEnd {
    Clean,
    Ended { error: String },
    Panicked,
}

/// Sort a joined task outcome into its ending.
///
/// A returned error keeps its text for the caller's log line; a panic keeps
/// nothing, because a panic payload is arbitrary. Cancellation is not expected
/// (nothing aborts this task) and reads as an ending, because the tab is over
/// either way.
fn classify_task_end(outcome: Result<std::io::Result<()>, tokio::task::JoinError>) -> TaskEnd {
    match outcome {
        Ok(Ok(())) => TaskEnd::Clean,
        Ok(Err(e)) => TaskEnd::Ended {
            error: e.to_string(),
        },
        Err(join) if join.is_panic() => TaskEnd::Panicked,
        Err(join) => TaskEnd::Ended {
            error: join.to_string(),
        },
    }
}

/// Kill a terminal's tree after its task panicked.
///
/// The nested task died with the child handle, so the ordered teardown never
/// ran and the shell and its jobs would otherwise survive with no tab. What
/// remains outside the panic boundary is the tty identity, the child's pid
/// with the start time captured at spawn, and the shared scanner: signal the
/// recorded tree with escalating HUP, TERM, KILL and a grace between rounds,
/// pruning recycled pids before every round so a reused pid in a recycled
/// group is never signalled, then reap the child by pid.
///
/// The child itself is signalled directly every round, not only through its
/// group: an empty scanner snapshot would otherwise leave a HUP-ignoring
/// shell running with no tab. The start time captured at spawn guards the
/// direct signal, so a recycled pid is never touched.
///
/// Only the scanner snapshot is trusted, never a fresh enumeration: the master
/// is already dropped, so the device number may be recycled under a new
/// terminal, and signalling a fresh enumeration could kill that terminal's
/// processes. Not the session either: the panic may have come after the
/// ordered teardown reaped the child, and a reaped leader's pid may already
/// lead another terminal's session. A job the last scan never captured is the
/// accepted residual; the count is returned so the caller logs what the
/// fallback saw.
///
/// Deliberately not the ordered teardown: there is no master to drain, only
/// orphans to stop and one child to reap. A tree that outlives the last round
/// is logged, never silently kept.
async fn fallback_teardown(
    scanner: &ContainmentScanner,
    tty_dev: u64,
    child_pid: u32,
    child_start: Option<u64>,
) -> usize {
    let child = i32::try_from(child_pid).ok().map(qoro_pty::teardown::Pid);
    // A missing start time disables the direct signal below: signalling a pid
    // without its generation could hit a recycled stranger, and an unsignalled
    // orphan is the smaller failure. Said out loud, so the residual is visible
    // rather than silent. Capture fails only when the kernel will not describe
    // our own just-spawned child, which is how rarely this fires.
    if child.is_some() && child_start.is_none() {
        eprintln!(
            "qorod: fallback teardown for pid {child_pid} has no start time; direct signal skipped"
        );
    }
    let mut set = Containment::new();
    // Once, before the first signal: the snapshot is frozen here and never
    // refreshed. A scan taken after the master is dropped may already belong
    // to a recycled device's new owner, so absorbing again inside the loop
    // could signal a terminal this fallback did not come for. What the last
    // scan missed stays missed; the count says so.
    set.absorb(scanner.members(tty_dev));
    let seen = set.len();
    let mut round = 0usize;
    while let Some(signal) = teardown::plan(round) {
        set.prune(&qoro_pty::sys::still_alive);
        // The known child first: this is the one pid that is ours by spawn,
        // guarded by the start time from that same spawn.
        if let (Some(pid), Some(start)) = (child, child_start)
            && qoro_pty::sys::still_alive(pid, start)
        {
            let _ = qoro_pty::sys::signal_pid(pid, signal);
        }
        let child_gone = child
            .zip(child_start)
            .is_none_or(|(pid, start)| !qoro_pty::sys::still_alive(pid, start));
        let groups =
            set.groups_to_signal(round, &qoro_pty::sys::still_alive, &qoro_pty::sys::in_group);
        if groups.is_empty() && child_gone {
            break;
        }
        for g in groups {
            let _ = qoro_pty::sys::signal_group(g, signal);
        }
        round += 1;
        tokio::time::sleep(teardown::GRACE).await;
    }
    reap_child(child_pid).await;
    set.prune(&qoro_pty::sys::still_alive);
    if !set
        .groups_to_signal(round, &qoro_pty::sys::still_alive, &qoro_pty::sys::in_group)
        .is_empty()
    {
        eprintln!("qorod: fallback teardown for pid {child_pid} left processes behind");
    }
    seen
}

/// Reap one child without its handle.
///
/// The daemon is still the parent, so `waitpid` on exactly this pid reaps
/// exactly this child and nothing else. Anything that is not "still alive"
/// ends the loop: reaped, already gone, or never ours. A non-positive or
/// unrepresentable pid waits for nothing, because `waitpid(-1)` means any
/// child and this must never touch a terminal it did not come for.
async fn reap_child(child_pid: u32) {
    let pid = i32::try_from(child_pid).ok().filter(|p| *p > 0);
    let Some(pid) = pid else { return };
    for _ in 0..20 {
        let mut status = 0;
        // SAFETY: waitpid on our own child's pid with WNOHANG; blocks nothing,
        // reaps nothing but this pid.
        let r = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
        if r > 0 {
            return;
        }
        if r == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            continue;
        }
        // Interrupted before reaping anything: retry rather than leave a
        // zombie behind. Anything else negative is ECHILD or a real error,
        // and both mean there is nothing left to wait for.
        if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return;
    }
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
                agents.refresh_foreground(*stream, crate::agents::now_unix());
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

/// The size every terminal is born with: the PTY, the engine and the tracked
/// current size below all start here, from this one constant, so changing the
/// default cannot silently desynchronise the equal-size decision.
pub const INITIAL_SIZE: PtySize = PtySize { cols: 80, rows: 24 };

/// How long the shrunken size stays before the canonical size is restored.
///
/// Short on purpose: the shrink exists only to make the kernel deliver two
/// SIGWINCHs. Here the wait is an
/// event-driven timer arm in the terminal task, cancelled by a real resize.
pub const NUDGE_RESTORE_DELAY: tokio::time::Duration = tokio::time::Duration::from_millis(30);

/// Whether an applied resize needs the shrink-then-restore nudge.
///
/// A same-size `TIOCSWINSZ` is a no-op for the child: the kernel only
/// delivers SIGWINCH on change. A reattaching client whose size already
/// matches the terminal would therefore never force a repaint in a
/// full-screen app that redraws only on SIGWINCH. The nudge answers that by
/// shrinking one row (or column) and restoring on a timer.
pub fn needs_nudge(current: PtySize, requested: PtySize) -> bool {
    current == requested
}

/// The shrunken size for the nudge: one row fewer, or one column fewer when
/// there is no row to give. Keeps a minimum width, so a tiny
/// terminal never nudges to zero.
pub fn nudged_size(size: PtySize) -> PtySize {
    if size.rows > 2 {
        PtySize {
            cols: size.cols,
            rows: size.rows - 1,
        }
    } else {
        PtySize {
            cols: size.cols.saturating_sub(1).max(4),
            rows: size.rows,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn terminal_task(
    master: OwnedFd,
    mut child: std::process::Child,
    tty_dev: u64,
    mut from_clients: mpsc::Receiver<ClientCommand>,
    updates: watch::Sender<TerminalUpdate>,
    warnings: Arc<std::sync::Mutex<Vec<Warning>>>,
    spill: crate::spill::SpillRing,
    scanner: Arc<ContainmentScanner>,
    agents: Arc<crate::agents::AgentRegistry>,
    stream: u32,
    terminals: Arc<TerminalRegistry>,
    books: Option<Bookkeeping>,
    // Where this terminal was started, which is what its row already says.
    started_in: Option<PathBuf>,
) -> io::Result<()> {
    let master_raw = master.as_raw_fd();
    let child_pid = child.id() as i32;

    let afd = AsyncFd::with_interest(MasterIo(master), Interest::READABLE | Interest::WRITABLE)?;
    let mut engine = AlacrittyEngine::new(INITIAL_SIZE.cols, INITIAL_SIZE.rows);
    // The engine's discards as last handed to the index.
    let mut discarded = qoro_terminal::Discards::default();
    let mut buf = vec![0u8; 64 * 1024];
    let mut policy = SizePolicy::Unowned;
    // Writable clients refused a size while another owned the terminal, oldest first, with
    // the size each last asked for. Empty unless two clients size one terminal.
    let mut waiting: Vec<(ClientId, PtySize)> = Vec::new();
    // Bounded by bytes, with capacity reserved for terminal replies so a saturated input
    // queue cannot deadlock an application waiting for a cursor position report.
    let mut writes = WriteQueue::new(1024 * 1024, 64 * 1024);
    // Maintained during the terminal's life, not built at teardown. See the scan below.
    let mut containment = Containment::new();
    containment.absorb(qoro_pty::sys::enumerate_tty(tty_dev));
    let mut last_scan = std::time::Instant::now();
    // And the directory last written down, for the same reason. Seeded with the one this
    // terminal was started in, because that is already in the row: without it the first
    // read writes the same value back and a daemon that has done nothing has still
    // written to its database. Unseeded when it started without one of its own: then the
    // first read is only where it started, and only a move from there is written.
    let mut last_cwd: Option<PathBuf> = started_in;
    // When to read the directory once more, because the throttle below skipped a read.
    // A `cd` prints its prompt within the throttle window of the output before it, and
    // with nothing printed after that a quiet tab kept its old directory for good: the
    // index, the place line and `qoro worktree` all read the stale one. Armed only by
    // output, so an idle terminal still has no timer (ADR-009).
    let mut cwd_at: Option<tokio::time::Instant> = None;
    // When to read the screen for what the agent is doing. `None` while nothing has
    // happened, which is what keeps an idle terminal at no wakeups at all.
    let mut detect_at: Option<tokio::time::Instant> = None;
    // When the screen was last published, and when the output that arrived since is due.
    // `None` while nothing is held back, so a quiet terminal has no timer.
    let mut published: Option<tokio::time::Instant> = None;
    let mut publish_at: Option<tokio::time::Instant> = None;
    // Until when output is the echo of what was just typed, which goes out at once whatever
    // the frame limit says. A shell echoes a burst of typing over several reads, so it is a
    // window of one frame after the input, not only the next read. Held for a frame, every
    // key typed within 16 ms of the last screen waited up to 16 ms for its own echo:
    // measured, a quiet echo went from 0.1 ms to 17.5 ms at the median when the limit came
    // in.
    let mut echo_until: Option<tokio::time::Instant> = None;
    // How many screens the echo window may still publish at once. A count and not a time
    // floor: a timer rounds up to the next millisecond and more, which put a quiet echo at
    // 2.3 ms against a 2 ms budget; a count waits for nothing, and a key held down in a
    // terminal that streams still publishes only a few screens per key, not one per read.
    let mut echo_left: u8 = 0;
    // When recent pictures arrived, for the flood guard below. Empty while the
    // terminal draws no pictures, like every other arm in this loop.
    let mut image_at: std::collections::VecDeque<std::time::Instant> =
        std::collections::VecDeque::new();
    // When to look again at a foreground leader the read loop's look was held back for.
    let mut relook_at: Option<tokio::time::Instant> = None;
    // When the run of output that `detect_at` is waiting out began, so that waiting it out
    // can be given a ceiling.
    let mut detect_since: Option<tokio::time::Instant> = None;
    // When to re-announce a held blocker so a panel that missed one refresh
    // still converges. `None` unless a blocker holds, for the same reason:
    // the tick is recovery, not polling.
    let mut blocker_at: Option<tokio::time::Instant> = None;
    let mut pending_input: Option<std::time::Instant> = None;
    // A pending nudge restore: the canonical size to return to, and when.
    // `None` while no nudge is in flight, so a terminal that never needed one
    // has no timer, like every other arm in this loop.
    let mut nudge_restore: Option<(PtySize, tokio::time::Instant)> = None;
    // The size the PTY and the engine agree on. Seeded with the size both
    // are constructed with above, and updated on every applied resize, so
    // the equal-size decision never reads a converted snapshot.
    let mut current_size = INITIAL_SIZE;
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
                        ClientCommand::Input { bytes, from } => {
                        let arrived = std::time::Instant::now();
                        pending_input = Some(arrived);
                        echo_until = Some(tokio::time::Instant::now() + PUBLISH_EVERY);
                        echo_left = ECHO_SCREENS;
                            // Live input is traffic; restore replay is not:
                            // replayed bytes must not refresh the idle clock
                            // of a tab that has been quiet for hours.
                            if crate::idle::is_live_input(from.as_str()) {
                                agents.note_io(stream, crate::agents::now_unix());
                            }
                            // A large paste with no line terminator, to a child in canonical
                            // mode, is discarded by the line discipline past its limit while
                            // every write reports success. Measured: 1 MiB written, 0 received.
                            // The only honest thing to do is say so.
                            let longest = longest_line(&bytes);
                            if longest > MAX_CANON && is_canonical(master_raw) {
                                warn(
                                    &warnings,
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
                            let outcome = arbitrate_resize(&mut policy, &from, writable, size);
                            // A refused writable client's size is kept for when the owner
                            // leaves, in the order they were first refused; an applied
                            // one is no longer waiting.
                            match waiting.iter_mut().find(|(c, _)| *c == from) {
                                Some(entry) if outcome == ResizeOutcome::NotOwner => {
                                    entry.1 = size;
                                }
                                _ => {
                                    waiting.retain(|(c, _)| *c != from);
                                    if outcome == ResizeOutcome::NotOwner {
                                        waiting.push((from, size));
                                    }
                                }
                            }
                            if outcome == ResizeOutcome::Applied {
                                if needs_nudge(current_size, size) {
                                    // Same size, so the ioctl below would be a
                                    // no-op for the child and a reattaching
                                    // client would never force a repaint.
                                    // Shrink once, restore on the timer arm.
                                    // A refused resize never reaches here, so
                                    // a non-owner or observer attaches zero
                                    // resizes by construction.
                                    let shrunk = nudged_size(size);
                                    sys::resize(master_raw, shrunk.cols, shrunk.rows)?;
                                    engine.resize(shrunk.cols, shrunk.rows);
                                    current_size = shrunk;
                                    nudge_restore = Some((
                                        size,
                                        tokio::time::Instant::now() + NUDGE_RESTORE_DELAY,
                                    ));
                                } else {
                                    // A real size change cancels a pending
                                    // restore: the new size is canonical now,
                                    // and restoring the old one would fight it.
                                    nudge_restore = None;
                                    sys::resize(master_raw, cols, rows)?;
                                    engine.resize(cols, rows);
                                    current_size = size;
                                }
                                // The teardown path reads this when the last update
                                // was a picture: the end then carries a blank of
                                // the right geometry rather than a guessed one.
                                // The canonical size, not the transient shrunk
                                // one a nudge is holding.
                                if let Some(handle) = terminals.get(stream) {
                                    handle.last_dims.store(
                                        ((size.cols as u64) << 32) | (size.rows as u64),
                                        std::sync::atomic::Ordering::Relaxed,
                                    );
                                }
                                let _ = updates.send(TerminalUpdate::Full(engine.snapshot()));
                            }
                        }
                        // A close ends the loop, which runs teardown on the way out. It is not
                        // a signal to the child from here: the child is contained by the
                        // teardown that follows, and signalling twice from two places is how a
                        // containment set stops being the only path.
                        ClientCommand::Close => {
                            // Where this tab is, read while its shell is still alive,
                            // which is the last moment it can be read at all. This is the
                            // path a stop takes, and a stop is exactly when the directory
                            // has to be right: the next start opens the tab here.
                            if let Some(now) =
                                qoro_pty::sys::cwd_of(qoro_pty::teardown::Pid(child_pid))
                                && last_cwd.is_some()
                                && last_cwd.as_deref() != Some(now.as_path())
                            {
                                terminals.set_cwd(stream, Some(now.display().to_string()));
                                if let Some(books) = books.clone() {
                                    let _ = books
                                        .store
                                        .set_cwd(stream as i64, now.display().to_string())
                                        .await;
                                }
                            }
                            break;
                        }
                        // A new subscriber wants the live screen, not the last published
                        // one. Published unconditionally: the subscriber is watching by
                        // definition, so the idle-cost rule is not what this bypasses.
                        ClientCommand::Refresh => {
                            // Picture first, screen last: the channel keeps only the
                            // newest, so whatever is sent last is what a slow
                            // subscriber is guaranteed to see, and the picture is
                            // healed from the handle on the next wake regardless.
                            // A doubled delivery only happens when both arrive, and
                            // the client draws by id.
                            if let Some(handle) = terminals.get(stream)
                                && let Some(image) = handle.last_image.lock().unwrap().clone()
                            {
                                let _ = updates.send(TerminalUpdate::Image(image));
                            }
                            let _ = updates.send(TerminalUpdate::Full(engine.snapshot()));
                        }
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
                            waiting.retain(|(c, _)| *c != client);
                            if policy == SizePolicy::Owner(client) {
                                policy = policy.apply(OwnershipEvent::OwnerDisconnected, None);
                                // ADR-007: the geometry passes to the writable client
                                // refused a size first, and the size it last asked for
                                // with it. Without this a second window sat at
                                // the first one's size after it closed, since nothing made it
                                // ask again.
                                if !waiting.is_empty() {
                                    let (next, size) = waiting.remove(0);
                                    policy = SizePolicy::Owner(next);
                                    let canonical = nudge_restore.map_or(current_size, |(s, _)| s);
                                    if size != canonical {
                                        nudge_restore = None;
                                        sys::resize(master_raw, size.cols, size.rows)?;
                                        engine.resize(size.cols, size.rows);
                                        current_size = size;
                                        if let Some(handle) = terminals.get(stream) {
                                            handle.last_dims.store(
                                                ((size.cols as u64) << 32) | (size.rows as u64),
                                                std::sync::atomic::Ordering::Relaxed,
                                            );
                                        }
                                        let _ = updates.send(TerminalUpdate::Full(engine.snapshot()));
                                    }
                                }
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
                            // Output is traffic: one stamp per chunk, reusing a
                            // single clock read for the foreground look below.
                            let now_unix = crate::agents::now_unix();
                            agents.note_io(stream, now_unix);
                            // One ioctl, measured in microseconds, so it is cheap enough to
                            // ask on every read. A change of foreground process is a new
                            // generation, and everything believed about the old one is
                            // cleared: a pid is not an identity.
                            if agents
                                .note_foreground(
                                    stream,
                                    qoro_pty::sys::foreground_leader(master_raw),
                                    now_unix,
                                )
                                .is_some()
                            {
                                // The retained OSC title described the old process.
                                // Same-pid `exec` also mints a generation (via the
                                // process watcher) without touching the leader, so a
                                // title can briefly outlive that narrower change; it
                                // is replaced as soon as the new program sets its own,
                                // and no bundled rule matches a foreign program's
                                // title text.
                                engine.clear_title();
                            }
                            if relook_at.is_none() {
                                relook_at =
                                    agents.relook_due(stream).map(tokio::time::Instant::from_std);
                            }
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
                                // Where this tab is now, on the same throttle, because a
                                // person moves around inside one shell and the directory a
                                // tab was born in stops being where it is within a minute
                                // of real use. One syscall, and only written when it
                                // actually changed.
                                cwd_at = None;
                                note_cwd(child_pid, &mut last_cwd, &terminals, stream, &books);
                            } else if cwd_at.is_none() {
                                // Skipped by the throttle: read once more when it lapses.
                                cwd_at = Some(tokio::time::Instant::from_std(
                                    last_scan + CONTAINMENT_SCAN_INTERVAL,
                                ));
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
                            // One load a read: a program asking for the background mid read is
                            // answered with what the person is looking at now.
                            engine.set_host_colors(host_colors());
                            for (index, slice) in buf[..n].chunks(PARSE_SLICE).enumerate() {
                                let (_damage, mut fx) = engine.feed(slice);
                                effects.append(&mut fx);
                                let scrolled = engine.take_scrolled_off();
                                if !scrolled.is_empty() {
                                    // No log entry for output, not even its range: one per
                                    // slice grew the log with how chatty a build was, which
                                    // ADR-017 rules out, and cost a lock on the busiest path
                                    // here. The history records where output reached.
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
                            // What the engine threw away, handed to the index once it
                            // changes: before this read's screen is published, so a client
                            // that sees the output after a discard also sees it counted.
                            // Compared first, so the ordinary read takes no lock.
                            let now = engine.discards();
                            if now != discarded {
                                discarded = now;
                                if let Some(handle) = terminals.get(stream) {
                                    *handle.discards.lock().unwrap() = now;
                                }
                            }
                            for reply in replies(&effects) {
                                // Replies have their own reserved budget: an application
                                // that asked a question is blocked until it is answered.
                                let _ = writes.push(reply, WritePriority::TerminalReply);
                            }
                            // Pictures are rare and unthrottled: holding one for the
                            // next snapshot would put it on the wrong screen. Stored
                            // beside the watch channel so a late subscriber still
                            // finds it, and published at once so a watching client
                            // draws it under the output that follows.
                            //
                            // Unthrottled has one guard: a child emitting pictures in
                            // a loop is a malfunction, and each picture costs every
                            // subscriber a clone, a base64 and a socket write. Past
                            // eight a second the excess is dropped and counted on
                            // the handle, and the last good picture stays put.
                            for effect in &effects {
                                if let qoro_terminal::TerminalEffect::Image(captured) = effect {
                                    let now = std::time::Instant::now();
                                    while image_at
                                        .front()
                                        .is_some_and(|at| now - *at > FLOOD_WINDOW)
                                    {
                                        image_at.pop_front();
                                    }
                                    if image_at.len() >= MAX_IMAGES_PER_WINDOW {
                                        if let Some(handle) = terminals.get(stream) {
                                            handle.image_flood_dropped.fetch_add(
                                                1,
                                                std::sync::atomic::Ordering::Relaxed,
                                            );
                                        }
                                        continue;
                                    }
                                    image_at.push_back(now);
                                    let image = StoredImage {
                                        terminal: stream,
                                        id: captured.id,
                                        protocol: captured.protocol,
                                        bytes: captured.bytes.clone(),
                                    };
                                    if let Some(handle) = terminals.get(stream) {
                                        *handle.last_image.lock().unwrap() = Some(image.clone());
                                    }
                                    let _ = updates.send(TerminalUpdate::Image(image));
                                }
                            }
                            // Nothing changed for anyone, so nothing runs. A snapshot
                            // allocates one string per row whether or not a client is
                            // watching, and a terminal streaming in the background would
                            // do that on every read.
                            //
                            // At most one screen per `PUBLISH_EVERY` while output streams:
                            // one frame of a child's is several reads, and a snapshot per
                            // read is a string per row each time for screens nobody sees.
                            // The first read after a quiet spell goes out at once, so an
                            // echoed key is not held; the rest wait for the arm below.
                            let now = tokio::time::Instant::now();
                            let echo = echo_left > 0 && echo_until.is_some_and(|until| now < until);
                            if !echo {
                                echo_until = None;
                            }
                            if updates.receiver_count() > 0 && (publish_at.is_none() || echo) {
                                match published {
                                    Some(at) if now < at + PUBLISH_EVERY && !echo => {
                                        publish_at = Some(at + PUBLISH_EVERY);
                                    }
                                    _ => {
                                        let _ = updates.send(TerminalUpdate::Full(engine.snapshot()));
                                        published = Some(now);
                                        // What was held is in this screen too.
                                        publish_at = None;
                                        echo_left = echo_left.saturating_sub(u8::from(echo));
                                    }
                                }
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
                            //
                            // Pushed out by more output, but never past a ceiling. An agent
                            // holding the turn redraws a ticking timer, so the screen never
                            // goes quiet for eighty milliseconds and the deadline moved
                            // forever: measured against a real codex, the panel said `idle`
                            // for the whole of a thirty second turn whose own screen said
                            // `Working (29s, esc to interrupt)` in as many words. The
                            // busiest state was the one state that could never be read.
                            let now = tokio::time::Instant::now();
                            let began = *detect_since.get_or_insert(now);
                            detect_at = Some((now + DETECT_SETTLE).min(began + DETECT_CEILING));
                            if trace && let Some(at) = pending_input.take() {
                                eprintln!("trace: input to snapshot {:?}", at.elapsed());
                            }
                        }
                    }
                }
                _ = async {
                    match cwd_at {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                }, if cwd_at.is_some() => {
                    cwd_at = None;
                    note_cwd(child_pid, &mut last_cwd, &terminals, stream, &books);
                }
                _ = async {
                    match relook_at {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                }, if relook_at.is_some() => {
                    relook_at = None;
                    if agents
                        .note_foreground(
                            stream,
                            qoro_pty::sys::foreground_leader(master_raw),
                            crate::agents::now_unix(),
                        )
                        .is_some()
                    {
                        engine.clear_title();
                    }
                }
                _ = async {
                    match publish_at {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                }, if publish_at.is_some() => {
                    publish_at = None;
                    if updates.receiver_count() > 0 {
                        let _ = updates.send(TerminalUpdate::Full(engine.snapshot()));
                        published = Some(tokio::time::Instant::now());
                    }
                }
                _ = async {
                    match nudge_restore {
                        Some((_, at)) => tokio::time::sleep_until(at).await,
                        // Disabled: `select!` polls this arm only when the guard holds, and
                        // the guard is what keeps a terminal that never nudged at no timer.
                        None => std::future::pending().await,
                    }
                }, if nudge_restore.is_some() => {
                    // Back to the canonical size, never the attacher's: the
                    // shrink was a redraw trick, not a geometry decision, and
                    // the owner arbitration that approved it is unchanged.
                    // Best effort, never fatal: this arm is a cosmetic retry,
                    // and killing the terminal over a failed redraw ioctl
                    // would turn it into a destroyed session. On failure the
                    // shrunken size stands and the next real resize repairs it.
                    if let Some((size, _)) = nudge_restore.take()
                        && sys::resize(master_raw, size.cols, size.rows).is_ok()
                    {
                        engine.resize(size.cols, size.rows);
                        current_size = size;
                    }
                    let _ = updates.send(TerminalUpdate::Full(engine.snapshot()));
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
                    detect_since = None;
                    // Whether or not anyone is subscribed, deliberately: an agent blocked
                    // in a terminal nobody is looking at is exactly what this notices.
                    if let Some(program) = agents.rules_name_of(stream)
                        && let Some(manifest) = qoro_detect::rules_for(&program)
                    {
                        let screen = engine.visible_text();
                        let osc = qoro_detect::OscContext {
                            title: engine.title(),
                        };
                        let known = qoro_detect::for_program(&program)
                            .is_some_and(|m| m.id.to_lowercase() != "generic");
                        let reading = manifest.read_with_osc(&screen, &osc);
                        // A program inside its startup grace is still drawing:
                        // claiming idle from a half drawn first frame is what let a
                        // task be typed at Claude Code over a trust dialog that exits
                        // by default. Only the idle fallback waits; a rule that
                        // matched is read at once. The screen is read again when the
                        // grace ends, since a quiet program would give no other
                        // reason to look.
                        let grace = if known && matches!(reading, qoro_detect::Reading::Nothing) {
                            agents
                                .agent(stream)
                                .and_then(|a| a.grace_ends(std::time::Instant::now()))
                        } else {
                            None
                        };
                        let verdict = match grace {
                            Some(at) => {
                                detect_at = Some(tokio::time::Instant::from_std(at));
                                None
                            }
                            None => screen_verdict(reading, known),
                        };
                        if let Some(verdict) = verdict {
                            // Whether or not this changed the belief: it filters the
                            // repeat and announces the news itself.
                            agents.observe_screen(
                                stream,
                                &manifest.id,
                                &verdict,
                                crate::agents::now_unix(),
                            );
                        }
                    }
                    // Armed only while a blocker holds. A re-announce is a
                    // repeat, not news: it goes around the acceptance rule, so
                    // it can neither change the belief nor be filtered by the
                    // no-change gate, and anything but a blocker keeps using
                    // that gate alone.
                    blocker_at = agents
                        .belief_visible_blocker(stream)
                        .then(|| tokio::time::Instant::now() + BLOCKER_REANNOUNCE);
                }
                _ = async {
                    match blocker_at {
                        Some(at) => tokio::time::sleep_until(at).await,
                        // Disabled like the detect arm: no blocker, no wakeups.
                        None => std::future::pending().await,
                    }
                }, if blocker_at.is_some() => {
                    if agents.belief_visible_blocker(stream) {
                        agents.reannounce(stream);
                        blocker_at =
                            Some(tokio::time::Instant::now() + BLOCKER_REANNOUNCE);
                    } else {
                        blocker_at = None;
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
    // Output held back for the next frame is the child's last screen when the loop ends
    // here, and the ended update repeats whatever was published last.
    if publish_at.is_some() {
        let _ = updates.send(TerminalUpdate::Full(engine.snapshot()));
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
    // The history gets the rest of the tab: what scrolled off while it was being torn down,
    // then the screen it was showing. Only rows that scrolled off were ever written, so a
    // tab that stopped or slept came back without the output of its last command. The
    // alternate screen is a program's own canvas, not output, and stays out. A tab that
    // was closed for good has its history removed afterwards, so this costs it nothing.
    spill.push(engine.take_scrolled_off());
    if !engine.modes().alt_screen {
        let mut screen = engine.visible_text();
        while screen.last().is_some_and(|row| row.trim().is_empty()) {
            screen.pop();
        }
        spill.push(screen);
    }
    // While `afd` still holds the descriptor: returning closes it, and the number must not
    // be readable through the registry once another terminal can be handed it.
    agents.detach_master(stream);
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
        // The scan's own unreadable count is every other user's process on the machine,
        // which says nothing about one terminal. Teardown reports the ones in its session.
        let Ok((by_tty, _)) = crate::blocking::run_on(
            crate::blocking::Pool::Lifecycle,
            qoro_pty::sys::enumerate_all,
        )
        .await
        else {
            return;
        };
        *self.by_tty.lock().unwrap() = by_tty;
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

/// The shortest gap between two published screens of one terminal: sixty a second, the
/// same as the interface's own frame, since a screen published faster is never drawn.
pub const PUBLISH_EVERY: std::time::Duration = std::time::Duration::from_millis(16);

/// How many screens the frame after an input may publish at once: enough for an echo that
/// a shell writes over a few reads, and a bound on what one key costs a terminal that
/// streams.
pub const ECHO_SCREENS: u8 = 4;

/// How long a terminal has to be quiet before its screen is read for what the agent is
/// doing.
///
/// Armed by output and disarmed by the read, so a terminal that says nothing schedules
/// nothing: an idle workspace costs no wakeups at all. More output pushes the deadline out,
/// which is what makes a burst one evaluation rather than one per read, and what makes the
/// screen that gets read the settled one with the prompt on it rather than a screen halfway
/// through being drawn.
pub const DETECT_SETTLE: std::time::Duration = std::time::Duration::from_millis(80);

/// The longest a screen may go unread while it keeps changing.
///
/// The settle above is what makes a burst one evaluation instead of one per read, and it
/// works because bursts end. A screen that never stops is the case it gets wrong: an agent
/// holding the turn animates a timer, every frame pushes the deadline out, and the screen
/// is never read at all. So the push has a ceiling, and a terminal that has been talking
/// for this long is read whether or not it has stopped.
///
/// Half a second, which is under the settle schedule ADR-009 already promises for a state
/// change and far above the cost of reading one screen: a handful of regex passes over the
/// visible rows, on a terminal that is already spending real work on the output itself.
pub const DETECT_CEILING: std::time::Duration = std::time::Duration::from_millis(500);

/// How often a held blocker is re-announced while it holds.
///
/// Recovery, not polling: a panel that missed one index refresh converges on
/// the next repeat. Armed only while a `visible_blocker` holds and disarmed
/// the moment it clears, so terminals without a blocker schedule nothing.
pub const BLOCKER_REANNOUNCE: std::time::Duration = std::time::Duration::from_millis(800);

/// Where user agent-detection overrides live: `<config>/qoro/agent-detection/`.
///
/// The user's own config directory, like their shell rc: same trust, no
/// daemon-managed permissions. A missing directory simply means no overrides.
fn override_dir() -> PathBuf {
    crate::paths::config_dir().join("agent-detection")
}

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
    // And the session, which the revoke does not erase: a job started and orphaned inside
    // one scan interval (`nohup job &` then `exit`) was never on any scan. Read once, here,
    // before the first `try_wait` below can reap the leader, because only an unreaped
    // leader keeps its pid from becoming some other session's id.
    let leader = qoro_pty::teardown::Pid(child_pid);
    let (session, opaque) = crate::blocking::run_on(crate::blocking::Pool::Lifecycle, move || {
        qoro_pty::sys::enumerate_session(leader)
    })
    .await
    .unwrap_or_default();
    containment.absorb(session);
    if opaque > 0 {
        eprintln!(
            "qorod: teardown for pid {child_pid}: {opaque} processes in its session could not \
             be inspected and may still hold this terminal"
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
            &qoro_pty::sys::in_group,
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

#[cfg(test)]
mod image_wire_tests {
    use super::{ImageWire, StoredImage};

    fn stored() -> StoredImage {
        StoredImage {
            terminal: 7,
            id: 3,
            protocol: qoro_terminal::ImageProtocol::Kitty,
            bytes: b"\x1b_Gf=100,a=T;Zm9v\x1b\\".to_vec(),
        }
    }

    /// The envelope survives the wire byte for byte.
    #[test]
    fn an_image_round_trips_through_the_wire() {
        let image = stored();
        let body = serde_json::to_vec(&ImageWire::from(&image)).expect("serialize");
        let back: ImageWire = serde_json::from_slice(&body).expect("parse");
        let decoded = back.decode().expect("decode");
        assert_eq!(decoded.bytes, image.bytes);
        assert_eq!(decoded.id, image.id);
        assert_eq!(decoded.protocol, image.protocol);
    }

    /// The host colours come back as they went in, every channel at both ends of its range.
    #[test]
    fn host_colours_come_back_as_they_were_reported() {
        super::set_host_colors([255, 0, 18], [0, 255, 1]);
        assert_eq!(super::host_colors(), Some(([255, 0, 18], [0, 255, 1])));
        super::set_host_colors([0, 0, 0], [0, 0, 0]);
        assert_eq!(
            super::host_colors(),
            Some(([0, 0, 0], [0, 0, 0])),
            "black is said"
        );
    }

    /// A link crosses the wire both ways, costs nothing on a run without one, and a span
    /// from a daemon that predates links reads as unlinked.
    #[test]
    fn a_link_crosses_the_wire_and_its_absence_costs_nothing() {
        let mut engine = qoro_terminal::AlacrittyEngine::new(20, 2);
        use qoro_terminal::TerminalEngine as _;
        engine.feed(b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\\x1b[1mbold");
        let wire = super::SnapshotWire::from(&engine.snapshot());
        let body = serde_json::to_string(&wire).expect("serialize");
        assert_eq!(body.matches("\"link\"").count(), 1, "{body}");
        let back: super::SnapshotWire = serde_json::from_str(&body).expect("parse");
        assert_eq!(back, wire);
        assert_eq!(back.links, vec!["https://example.com".to_string()]);
        assert_eq!(back.styles[0][0].link, Some(0));
        let old: super::SpanWire = serde_json::from_str(r#"{"at":0,"len":3}"#).expect("old");
        assert_eq!(old.link, None);
    }

    /// An older client parses snapshots and skips anything else. The image shape
    /// must fail the snapshot parse, or the old client would draw garbage.
    #[test]
    fn an_image_is_not_mistaken_for_a_snapshot() {
        let body = serde_json::to_vec(&ImageWire::from(&stored())).expect("serialize");
        assert!(
            serde_json::from_slice::<super::SnapshotWire>(&body).is_err(),
            "an old client would render this as a screen"
        );
    }

    /// And the reverse: a snapshot must not parse as an image, or a new client
    /// would drop screens as broken pictures.
    #[test]
    fn a_snapshot_is_not_mistaken_for_an_image() {
        let snap = super::SnapshotWire {
            cols: 80,
            rows: 24,
            cursor: (1, 1),
            lines: vec!["hi".to_string()],
            styles: Vec::new(),
            links: Vec::new(),
            modes: Default::default(),
            behind: 0,
        };
        let body = serde_json::to_vec(&snap).expect("serialize");
        assert!(serde_json::from_slice::<ImageWire>(&body).is_err());
    }

    /// Hostile input decodes to absent, never panics: wrong version, unknown
    /// protocol, bad base64, over the cap.
    #[test]
    fn hostile_images_decode_to_absent() {
        let mut wire = ImageWire::from(&stored());
        wire.v = 2;
        assert!(wire.decode().is_none());
        let mut wire = ImageWire::from(&stored());
        wire.protocol = "bogus".to_string();
        assert!(wire.decode().is_none());
        let mut wire = ImageWire::from(&stored());
        wire.data = "!!!not-base64!!!".to_string();
        assert!(wire.decode().is_none());
        let mut wire = ImageWire::from(&stored());
        // Valid base64, but decoding past the cap: twice the cap in chars is half
        // again the cap in bytes.
        wire.data = "A".repeat(qoro_terminal::MAX_IMAGE_BYTES * 2);
        assert!(wire.decode().is_none());
    }
}

#[cfg(test)]
mod warning_tests {
    use super::{MAX_WARNINGS, Warning, warn};

    fn paste(bytes: usize) -> Warning {
        Warning::CanonicalPasteWithoutNewline { bytes, limit: 0 }
    }

    /// Nothing in production drains the list, so recording is what has to bound it, and
    /// what it drops is the oldest: a gate reads the warning it just caused.
    #[test]
    fn warnings_keep_the_newest_and_stop_growing() {
        let warnings = std::sync::Mutex::new(Vec::new());
        for bytes in 0..MAX_WARNINGS + 10 {
            warn(&warnings, paste(bytes));
        }
        let kept = warnings.into_inner().unwrap();
        assert_eq!(kept.len(), MAX_WARNINGS, "the list grew past its bound");
        assert_eq!(
            kept.first(),
            Some(&paste(10)),
            "the oldest were not the ones dropped"
        );
        assert_eq!(kept.last(), Some(&paste(MAX_WARNINGS + 9)));
    }
}

#[cfg(test)]
mod nudge_tests {
    use super::{NUDGE_RESTORE_DELAY, needs_nudge, nudged_size};
    use qoro_domain::PtySize;

    /// Only an equal size needs the trick: a real change already delivers
    /// SIGWINCH through the ordinary ioctl, and nudging there would triple
    /// the resizes for no reason.
    #[test]
    fn only_an_equal_size_needs_the_nudge() {
        let current = PtySize { cols: 80, rows: 24 };
        assert!(needs_nudge(current, current));
        assert!(!needs_nudge(current, PtySize { cols: 81, rows: 24 }));
        assert!(!needs_nudge(current, PtySize { cols: 80, rows: 25 }));
    }

    /// One row fewer, so the kernel sees a change and the child repaints.
    /// The restore arm returns to the canonical size, which the timer test
    /// below pins through the attach path instead of here.
    #[test]
    fn the_nudge_shrinks_one_row_and_never_to_zero() {
        let shrunk = nudged_size(PtySize { cols: 80, rows: 24 });
        assert_eq!((shrunk.cols, shrunk.rows), (80, 23));
        // No row to give: one column fewer, floored at a usable width.
        let tiny = nudged_size(PtySize { cols: 5, rows: 2 });
        assert_eq!((tiny.cols, tiny.rows), (4, 2));
        let floor = nudged_size(PtySize { cols: 4, rows: 1 });
        assert_eq!((floor.cols, floor.rows), (4, 1));
    }

    /// The restore delay is a redraw beat, not a settle: short enough that a
    /// reattaching client never perceives the shrunken frame.
    #[test]
    fn the_restore_delay_is_shorter_than_any_settle_schedule() {
        assert!(
            NUDGE_RESTORE_DELAY < std::time::Duration::from_secs(1),
            "a {NUDGE_RESTORE_DELAY:?} gap would be visible"
        );
    }
}

#[cfg(test)]
mod session_title_tests {
    use super::{claude_session_title_at, opencode_session_title_at};

    fn fake_db_with(title: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("opencode.db");
        let conn = rusqlite::Connection::open(&db).expect("create");
        conn.execute_batch("CREATE TABLE session (id TEXT PRIMARY KEY, title TEXT NOT NULL);")
            .expect("schema");
        conn.execute(
            "INSERT INTO session (id, title) VALUES (?1, ?2)",
            ["ses_test123", title],
        )
        .expect("insert");
        (dir, db)
    }

    /// The panel names the conversation the way the agent does: the title out
    /// of opencode's own database, keyed by the reported session id. No
    /// environment is touched: the lookup takes the database path.
    #[test]
    fn a_reported_session_resolves_to_its_title() {
        let (_dir, db) = fake_db_with("Samimi selamla\u{15f}ma sohbeti");
        assert_eq!(
            opencode_session_title_at(&db, "ses_test123"),
            Some("Samimi selamla\u{15f}ma sohbeti".to_string())
        );
        assert_eq!(opencode_session_title_at(&db, "ses_unknown"), None);
        let (_dir, db) = fake_db_with("  \nSecond line");
        assert_eq!(
            opencode_session_title_at(&db, "ses_test123"),
            Some("Second line".to_string()),
            "a stored newline never reaches the row"
        );
    }

    /// Claude's transcript names the conversation: the newest generated
    /// title, and a `/rename` title over it wherever the two sit.
    #[test]
    fn a_claude_transcript_resolves_to_its_newest_title() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("abc.jsonl");
        let lines = [
            r#"{"type":"ai-title","aiTitle":"Old title","sessionId":"abc"}"#,
            r#"{"type":"user","message":{"content":"mentions \"ai-title\" in text"}}"#,
            r#"{"type":"ai-title","aiTitle":"New title","sessionId":"abc"}"#,
        ];
        std::fs::write(&path, lines.join("\n")).expect("write");
        assert_eq!(
            claude_session_title_at(&path),
            Some("New title".to_string())
        );

        let renamed = [
            r#"{"type":"custom-title","customTitle":"my-rename","sessionId":"abc"}"#,
            r#"{"type":"ai-title","aiTitle":"New title","sessionId":"abc"}"#,
        ];
        std::fs::write(&path, renamed.join("\n")).expect("write");
        assert_eq!(
            claude_session_title_at(&path),
            Some("my-rename".to_string())
        );

        std::fs::write(&path, r#"{"type":"user"}"#).expect("write");
        assert_eq!(claude_session_title_at(&path), None);
    }
}

#[cfg(test)]
mod restore_report_tests {
    use super::RestoreReport;

    /// The summary names every category, so a silent fallback has nowhere to hide.
    #[test]
    fn the_summary_counts_panes_agents_and_fallbacks() {
        let report = RestoreReport {
            spaces: 1,
            terminals: 4,
            brought_back: 4,
            cwd_fallback: 1,
            agents_resumed: 1,
            agents_shell_fallback: 1,
            plain_shells: 2,
        };
        assert_eq!(
            report.summary(),
            "restore: 4/4 panes, 1 agents resumed, 1 agent shells, 2 plain shells, 1 moved directory"
        );
    }

    /// An empty store restores nothing and says nothing came back.
    #[test]
    fn an_empty_report_says_zero_of_zero() {
        let report = RestoreReport::default();
        assert_eq!(report.brought_back, 0);
        assert!(report.summary().starts_with("restore: 0/0 panes"));
    }

    /// A lagged news reader is told to re-read rather than left behind silently.
    ///
    /// The channel holds one announcement and two are sent, so the oldest is
    /// dropped and the next receive reports the lag instead of it.
    #[tokio::test]
    async fn a_lagged_news_reader_gets_a_resync() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(1);
        tx.send(1u32).expect("a listener");
        tx.send(2u32).expect("overflow the channel");
        assert_eq!(
            super::pump_agent_event(&mut rx).await,
            Some(crate::control::EventMessage::Resync),
            "a missed announcement must say so rather than vanish"
        );
    }

    /// The ordinary step passes the terminal through untouched.
    #[tokio::test]
    async fn an_unlagged_news_reader_gets_the_terminal() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(16);
        tx.send(7u32).expect("a listener");
        assert_eq!(
            super::pump_agent_event(&mut rx).await,
            Some(crate::control::EventMessage::AgentChanged { terminal: 7 }),
        );
    }

    /// A closed channel ends the pump rather than resyncing forever.
    #[tokio::test]
    async fn a_closed_channel_ends_the_pump() {
        let (tx, mut rx) = tokio::sync::broadcast::channel::<u32>(16);
        drop(tx);
        assert_eq!(super::pump_agent_event(&mut rx).await, None);
    }

    /// A task ending is sorted by how it ended: clean, returned error, or panic.
    #[tokio::test]
    async fn a_task_ending_keeps_a_returned_error_and_names_a_panic() {
        let clean = tokio::spawn(async { Ok::<(), std::io::Error>(()) }).await;
        assert_eq!(super::classify_task_end(clean), super::TaskEnd::Clean);
        let ended =
            tokio::spawn(async { Err::<(), std::io::Error>(std::io::Error::other("gone")) }).await;
        assert_eq!(
            super::classify_task_end(ended),
            super::TaskEnd::Ended {
                error: "gone".into()
            },
            "a returned error keeps its text for the log line"
        );
        let panicked = tokio::spawn(async {
            panic!("boom");
            #[allow(unreachable_code)]
            Ok::<(), std::io::Error>(())
        })
        .await;
        assert_eq!(super::classify_task_end(panicked), super::TaskEnd::Panicked);
    }

    /// A fallback teardown over a tty nothing lives on is a no-op, not a wait.
    ///
    /// No groups means no rounds and no sleeps: the timeout only fires if a
    /// regression parks the supervisor on an empty tree while real terminals
    /// wait behind it. `u32::MAX` also proves the reap guard: it is not a
    /// positive pid, so no waitpid runs and no unrelated child is touched.
    /// `None` for the start time proves the direct signal is skipped without
    /// one: there is no pid a signal without a generation could reach.
    #[tokio::test]
    async fn a_fallback_teardown_with_nobody_on_the_tty_returns_at_once() {
        let scanner = super::ContainmentScanner::default();
        let seen = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            super::fallback_teardown(&scanner, u64::MAX, u32::MAX, None),
        )
        .await
        .expect("an empty tree kills nothing and waits for nothing");
        assert_eq!(seen, 0, "nothing was tracked on an empty tty");
    }
}

#[cfg(test)]
mod screen_fallback_tests {
    use super::*;

    fn screen(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|l| (*l).to_string()).collect()
    }

    /// Claude's transcript viewer is a pager over past turns: while it is open the live
    /// prompt is not on screen, and the reading taken before it opened must stand. Read as
    /// silence it went through the idle fallback and a working agent turned idle the
    /// moment somebody scrolled back.
    #[test]
    fn a_transcript_viewer_after_a_working_reading_keeps_working() {
        let agents = crate::agents::AgentRegistry::new().expect("registry");
        agents.register(
            1,
            qoro_domain::TerminalId::new("1"),
            crate::agents::mint_token(1),
        );
        let claude = qoro_detect::rules_for("claude").expect("claude rules");
        let osc = qoro_detect::OscContext { title: "" };
        let look = |lines: &[&str]| {
            if let Some(v) = screen_verdict(claude.read_with_osc(&screen(lines), &osc), true) {
                agents.observe_screen(1, &claude.id, &v, 1_750_000_000);
            }
        };

        look(&[
            "\u{23f5} Cogitating\u{2026} (12s \u{b7} esc to interrupt)",
            "",
        ]);
        assert_eq!(
            agents.agent(1).expect("agent").belief.state,
            qoro_domain::AgentRuntimeState::Working
        );
        look(&[
            "\u{25cf} an earlier answer",
            "Showing detailed transcript \u{b7} ctrl+o to toggle",
        ]);
        assert_eq!(
            agents.agent(1).expect("agent").belief.state,
            qoro_domain::AgentRuntimeState::Working,
            "the transcript viewer ended a working turn"
        );
    }

    #[test]
    fn a_known_agent_with_nothing_on_screen_still_falls_back_to_idle() {
        let v = screen_verdict(qoro_detect::Reading::Nothing, true).expect("the fallback");
        assert_eq!(v.state, qoro_domain::AgentRuntimeState::Idle);
        assert_eq!(v.rule, "known_agent_idle_fallback");
        assert_eq!(screen_verdict(qoro_detect::Reading::Nothing, false), None);
    }
}

#[cfg(test)]
mod search_tests {
    use super::{find_in_row, scan_history};
    use std::time::Duration;

    fn history(rows: &[String]) -> (tempfile::TempDir, qoro_store::History) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut history = qoro_store::History::open(dir.path()).expect("open");
        history.append(rows).expect("append");
        history.flush().expect("flush");
        let history = qoro_store::History::open(dir.path()).expect("reopen");
        (dir, history)
    }

    #[test]
    fn a_lowercase_query_matches_any_case_and_a_capital_means_exactly_that() {
        assert_eq!(find_in_row("an Error here", "error", None, false), Some(3));
        assert_eq!(find_in_row("an Error here", "Error", None, false), Some(3));
        assert_eq!(find_in_row("an error here", "Error", None, false), None);
        assert_eq!(find_in_row("GÜNEŞ güneş", "güneş", None, false), Some(0));
        assert_eq!(find_in_row("GÜNEŞ güneş", "Güneş", None, false), None);
    }

    #[test]
    fn a_bound_finds_the_next_match_after_it_or_the_last_one_before_it() {
        let row = "ab ab ab";
        assert_eq!(find_in_row(row, "ab", None, false), Some(0));
        assert_eq!(find_in_row(row, "ab", None, true), Some(6));
        assert_eq!(find_in_row(row, "ab", Some(0), false), Some(3));
        assert_eq!(find_in_row(row, "ab", Some(6), true), Some(3));
        assert_eq!(find_in_row(row, "ab", Some(6), false), None);
        assert_eq!(find_in_row(row, "ab", Some(0), true), None);
    }

    /// A cell is a character of the stored row, spacer included: the client turns it into a
    /// screen column, the daemon never measures anything.
    #[test]
    fn the_answer_counts_cells_and_not_bytes() {
        // `世` then its spacer cell, then `x`.
        assert_eq!(find_in_row("\u{4e16} x", "x", None, false), Some(2));
    }

    /// Found in review: a stored row keeps a spacer cell after every wide character, so a
    /// query of two of them never matched until the spacers were stepped over.
    #[test]
    fn a_query_of_wide_characters_matches_across_their_spacers() {
        let row = "a\u{4e16} \u{754c} x \u{4e16} \u{754c} ";
        assert_eq!(find_in_row(row, "\u{4e16}\u{754c}", None, false), Some(1));
        assert_eq!(find_in_row(row, "\u{4e16}\u{754c}", None, true), Some(7));
        assert_eq!(
            find_in_row(row, "\u{4e16}\u{754c}", Some(1), false),
            Some(7)
        );
        assert_eq!(find_in_row(row, "\u{754c}x", None, false), Some(3));
        assert_eq!(
            find_in_row(row, "\u{754c} x", None, false),
            None,
            "the spacer is not a space"
        );
    }

    #[test]
    fn a_search_finds_the_nearest_row_in_either_direction() {
        let rows: Vec<String> = (0..50_000)
            .map(|i| {
                if i % 10_000 == 5 {
                    format!("mark {i}")
                } else {
                    format!("row {i}")
                }
            })
            .collect();
        let (_dir, h) = history(&rows);
        let never = || false;
        let long = Duration::from_secs(30);
        assert_eq!(
            scan_history(&h, "mark", 49_999, true, long, u64::MAX, never).unwrap(),
            (Some((40_005, 0)), None)
        );
        assert_eq!(
            scan_history(&h, "mark", 40_004, true, long, u64::MAX, never).unwrap(),
            (Some((30_005, 0)), None)
        );
        assert_eq!(
            scan_history(&h, "mark", 10_006, false, long, u64::MAX, never).unwrap(),
            (Some((20_005, 0)), None)
        );
        // Past either end is the end of the search, not an error.
        assert_eq!(
            scan_history(&h, "mark", 4, true, long, u64::MAX, never).unwrap(),
            (None, None)
        );
        assert_eq!(
            scan_history(&h, "mark", 40_006, false, long, u64::MAX, never).unwrap(),
            (None, None)
        );
        assert_eq!(
            scan_history(&h, "mark", 90_000, false, long, u64::MAX, never).unwrap(),
            (None, None)
        );
    }

    /// The step budgets on a history the size of G2.6's (7.1 million `seq` rows), without
    /// the socket: how long a step reads, and how long a search back to the first row takes.
    #[ignore = "writes 7.1 million rows; run it deliberately, in release"]
    #[test]
    fn a_step_over_seven_million_rows_keeps_to_its_budget() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut h = qoro_store::History::open(dir.path()).expect("open");
        h.append(&["OLDEST-MARKER-42".to_string()]).expect("append");
        for round in 0..40u32 {
            let rows: Vec<String> = (1..=177_500u32).map(|i| (i + round).to_string()).collect();
            h.append(&rows).expect("append");
        }
        h.flush().expect("flush");
        let h = qoro_store::History::open(dir.path()).expect("reopen");
        let total = h.len();
        let mut steps = Vec::new();
        let mut wholes = Vec::new();
        for _ in 0..5 {
            let whole = std::time::Instant::now();
            let mut from = total - 1;
            loop {
                let at = std::time::Instant::now();
                let (found, resume) = scan_history(
                    &h,
                    "oldest-marker-42",
                    from,
                    true,
                    super::SEARCH_STEP,
                    super::SEARCH_BYTES,
                    || false,
                )
                .unwrap();
                steps.push(at.elapsed());
                if found.is_some() {
                    break;
                }
                from = resume.expect("the marker is there to find");
            }
            wholes.push(whole.elapsed());
        }
        steps.sort();
        wholes.sort();
        let pct = |v: &[Duration], p: usize| v[(v.len() * p / 100).min(v.len() - 1)];
        println!(
            "{total} rows: step n={} p50 {:?} p95 {:?}; whole search p50 {:?} p95 {:?}",
            steps.len(),
            pct(&steps, 50),
            pct(&steps, 95),
            pct(&wholes, 50),
            pct(&wholes, 95)
        );
        assert!(pct(&steps, 95) < Duration::from_millis(250));
    }

    /// The budget is what keeps a step short: spent, the step answers where to carry on,
    /// and carrying on from there reaches the match a single long step would have.
    #[test]
    fn a_spent_budget_answers_where_to_carry_on_and_carrying_on_arrives() {
        let rows: Vec<String> = (0..60_000)
            .map(|i| {
                if i == 7 {
                    "needle".to_string()
                } else {
                    format!("row {i}")
                }
            })
            .collect();
        let (_dir, h) = history(&rows);
        let long = Duration::from_secs(30);
        let mut from = 59_999;
        let mut steps = 0;
        let found = loop {
            steps += 1;
            match scan_history(&h, "needle", from, true, long, 1, || false).unwrap() {
                (Some(found), None) => break found,
                (None, Some(resume)) => {
                    assert!(resume < from, "no progress from {from}");
                    from = resume;
                }
                other => panic!("the search ended without the needle: {other:?}"),
            }
        };
        assert_eq!(found, (7, 0));
        assert!(
            steps > 1,
            "a one byte budget still searched everything at once"
        );
        // A search that is moot stops at its first chunk, with somewhere to carry on.
        let (found, resume) =
            scan_history(&h, "needle", 59_999, true, long, u64::MAX, || true).unwrap();
        assert_eq!(found, None);
        assert!(resume.is_some());
    }
}
