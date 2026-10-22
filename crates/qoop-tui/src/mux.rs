//! Several daemons behind one connection: the local one, and remote ones reached over ssh.
//!
//! The interface speaks to one daemon and keeps its state keyed by that daemon's terminal
//! and workspace numbers. Rather than teach every part of it about hosts, this sits
//! between the interface and the daemons and gives each remote its own range of numbers:
//! a remote terminal is `2^31 | link << 24 | id`, a remote workspace `link << 40 | id`.
//! Local numbers pass through unchanged, and renders from any daemon are relayed as they
//! came, never decoded: with no remote added this costs a byte copy per screen.
//!
//! What a remote looks like from the interface: its spaces are listed after the local
//! ones, named `<remote>:<space>`, and everything that addresses one of its terminals or
//! spaces is sent to it. A space made with a `<remote>:` prefix is made over there. When a
//! remote drops, its spaces stay listed as offline and its terminals go; the same numbers
//! come back when it does.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use qoop_proto::{Channel, Frame, Reassembler};
use qoop_remote::Remote;
use qoop_runtime::client::{self, Connection, Incoming};
use qoop_runtime::control::{
    AgentRow, ControlRequest, ControlResponse, EventMessage, LinkState, Pane, WorkspaceRow,
    keeps_promises, too_old_for,
};
use qoop_runtime::daemon::ClientWire;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Notify, mpsc};

/// The bit every remote terminal number carries. Local terminal numbers are below it.
pub const REMOTE_TERMINAL: u32 = 1 << 31;
/// Remote workspace numbers start here. Local workspace numbers are below it.
const WORKSPACE_SHIFT: u32 = 40;
/// How many remotes fit in the numbering: seven bits of link, zero being local.
pub const MAX_REMOTES: usize = 127;
/// The longest wait between attempts to reach a remote that is down.
const BACKOFF_CAP: Duration = Duration::from_secs(10);

/// Whether the interface has a daemon on this machine: attached to remotes alone it has not,
/// and this machine's own files speak of no machine it shows.
static HAS_LOCAL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn has_local() -> bool {
    HAS_LOCAL.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn is_remote_terminal(terminal: u32) -> bool {
    terminal & REMOTE_TERMINAL != 0
}

pub fn is_remote_workspace(workspace: i64) -> bool {
    workspace >= 1 << WORKSPACE_SHIFT
}

/// Which machine a space is on, as the list groups them: zero for this one.
pub fn machine_of(workspace: i64) -> usize {
    workspace_in(workspace).0
}

/// Why a typed address was refused: what is accepted, so the next try can be right.
fn not_ssh(target: &str) -> String {
    format!(
        "{target:?} is not an ssh address: a host like me@10.0.0.5, or ssh with -p, -l, -i, \
         -J, -4, -6 or -o Port, User, IdentityFile, ProxyJump, HostName, IdentitiesOnly"
    )
}

/// A daemon's terminal number as the interface sees it, or `None` when it does not fit
/// the range. Never wrapped: a wrapped number is somebody else's terminal.
fn terminal_out(link: usize, id: u32) -> Option<u32> {
    if link == 0 {
        (id < REMOTE_TERMINAL).then_some(id)
    } else {
        (id < 1 << 24).then_some(REMOTE_TERMINAL | (link as u32) << 24 | id)
    }
}

/// Which daemon an interface terminal number belongs to, and its number there.
fn terminal_in(terminal: u32) -> (usize, u32) {
    if is_remote_terminal(terminal) {
        (((terminal >> 24) & 0x7f) as usize, terminal & 0x00ff_ffff)
    } else {
        (0, terminal)
    }
}

fn workspace_out(link: usize, id: i64) -> Option<i64> {
    let fits = (0..1i64 << WORKSPACE_SHIFT).contains(&id);
    match link {
        0 => fits.then_some(id),
        // Zero is the placeholder of a remote nobody has reached yet, never a real space.
        _ => (fits && id != 0).then_some((link as i64) << WORKSPACE_SHIFT | id),
    }
}

fn workspace_in(workspace: i64) -> (usize, i64) {
    if is_remote_workspace(workspace) {
        (
            (workspace >> WORKSPACE_SHIFT) as usize,
            workspace & ((1 << WORKSPACE_SHIFT) - 1),
        )
    } else {
        (0, workspace)
    }
}

/// A remote space's name as listed: `<remote>:<space>`.
fn space_name(remote: &str, name: &str) -> String {
    if remote.is_empty() {
        name.to_string()
    } else {
        format!("{remote}:{name}")
    }
}

/// What the interface said, decoded.
enum FromTui {
    Control(ControlRequest),
    Wire { terminal: u32, wire: ClientWire },
}

/// What a link said, or that it came up or went down.
enum FromLink {
    Up {
        link: usize,
        conn: u64,
        writer: mpsc::UnboundedSender<BytesMut>,
        /// The version the daemon there is running, from its welcome. Empty for this
        /// machine's own daemon, which is never updated from here.
        server_version: String,
    },
    Said {
        link: usize,
        conn: u64,
        incoming: Incoming,
    },
    Down {
        link: usize,
        conn: u64,
        reason: String,
    },
    /// An answer a task of the mux's own worked out: that a machine has no qoop, that qoop
    /// is now there, or why not.
    Reply(ControlResponse),
    /// A machine asked to be connected answered with a qoop this build can talk to: add it.
    Probed { id: String, target: String },
    /// The time a remote with nothing awake is given before its link is let go.
    ParkCheck { link: usize, conn: u64 },
    /// A newer qoop is on the disk of this remote, `version`: its daemon is asked to move to
    /// it, at once with its tabs running when it can hand them over, else when nothing there
    /// is working.
    Arm { link: usize, version: String },
}

/// How long a remote with nothing awake keeps its link before the link is let go, so its
/// daemon, held by nothing else, can leave the way this machine's does.
const PARK_AFTER: Duration = Duration::from_secs(10);

/// The id of the mux's own ask that a remote's daemon move to the newer qoop on its disk.
const RESTART_ID: &str = "mux-restart";

/// The id of a status line the mux says about putting a newer qoop on a remote. Nobody
/// asked for it, and the interface shows a refusal it cannot place as a line, like any other.
pub(crate) const UPDATE_NOTICE: &str = "remote-update";

/// What is said of a machine whose daemon predates the promises of an unattended move: the
/// new qoop is on its disk, and it would end tabs and close windows to move onto it, so it
/// is left to the person.
fn left_for_a_hand_update(name: &str, running: &str) -> String {
    format!("{name} runs qoop {running}, which moves onto the new one when you run `qoop update`")
}

fn restart_when_safe(version: String) -> ControlRequest {
    ControlRequest::RestartWhenSafe {
        id: RESTART_ID.into(),
        version,
        // Nobody at that machine asked for this: its tabs are never ended, and a window of
        // a person there is never closed. Its own link to this Mac is not one.
        keep_tabs: true,
        quiet: true,
        same_version: false,
    }
}

/// One daemon as the mux keeps it.
struct Link {
    /// Empty for the local daemon.
    name: String,
    /// Where it is, for a remote; `None` for the local daemon.
    remote: Option<Remote>,
    /// Rung when something is asked of this link while it is down, so the next attempt
    /// to reach it happens now rather than at the end of its backoff: a person clicking
    /// on an offline machine is the best sign there is that its network is back.
    wake: Arc<Notify>,
    /// Let go on purpose, because nothing over there was awake: the supervisor closes the
    /// connection and waits for this to turn false before it tries again, with no backoff
    /// and no stale permit from an earlier offline spell to start it early.
    parked: Arc<tokio::sync::watch::Sender<bool>>,
    /// Requests for a parked link, sent once it is up again: the ask that wakes a machine
    /// is the one it should answer.
    pending: Vec<ControlRequest>,
    /// Whether this machine's own list may give its spaces' order among themselves: not
    /// while its part of a new order is unanswered (a list already on its way is older),
    /// and not once it missed or refused one (a qoop from before the ask, or away): its
    /// list would put a different space in the place a person dragged one to.
    order_owed: usize,
    order_off: bool,
    /// The version a newer qoop on this remote's disk says it is, while its daemon has not
    /// been asked to move to it yet: the link was down when the push finished. Asked on the
    /// next connection, unless that daemon already runs it.
    arm: Option<String>,
    /// The version the daemon on the connection now up runs, from its welcome. Only a qoop
    /// newer than this is asked for: a daemon already on it, moved there by another window
    /// or another Mac, would otherwise end and resume every tab a second time.
    server_version: String,
    /// The connection now up, if any, and its number, so a message from a connection that
    /// has since been replaced is recognised and dropped.
    conn: Option<(u64, mpsc::UnboundedSender<BytesMut>)>,
    control_seq: u32,
    seqs: BTreeMap<u32, u32>,
    /// Rows already in the interface's numbering.
    agents: Vec<AgentRow>,
    /// Split trees from the same answer as `agents`, numbered the same way.
    splits: Vec<Pane>,
    spaces: Vec<WorkspaceRow>,
    /// Whether this link has ever answered a space list. Before that it is one
    /// placeholder row, after it the last list it gave, marked offline while it is down.
    listed: bool,
    reason: String,
    /// The terminals of this link the interface subscribed to, in the link's numbering.
    desired: BTreeSet<u32>,
    /// Terminals this connection unsubscribed on its own, so it happens once.
    hidden: BTreeSet<u32>,
    /// Single-target requests sent and not answered, by request id.
    inflight: BTreeSet<String>,
    /// This link just created, closed or renamed something and its lists in the cache are
    /// older than that answer. The interface asks for fresh ones right after such an
    /// answer, and a merged list built from the old one in between would show a new tab
    /// missing or a closed one still there, which moves the keyboard. Merged lists of that
    /// kind are held until this link answers again.
    stale_agents: bool,
    stale_spaces: bool,
    /// Whether this link has ever answered an agent list. With `listed`, what holds the
    /// merged lists back until the local daemon has spoken: a first list made of a remote's
    /// placeholder alone parks the interface on it, empty, for as long as that host is
    /// unreachable.
    agents_seen: bool,
}

impl Link {
    fn new(name: String, remote: Option<Remote>) -> Self {
        Self {
            name,
            remote,
            wake: Arc::new(Notify::new()),
            parked: Arc::new(tokio::sync::watch::channel(false).0),
            pending: Vec::new(),
            order_owed: 0,
            order_off: false,
            arm: None,
            server_version: String::new(),
            conn: None,
            control_seq: 0,
            seqs: BTreeMap::new(),
            agents: Vec::new(),
            splits: Vec::new(),
            spaces: Vec::new(),
            listed: false,
            reason: "connecting".to_string(),
            desired: BTreeSet::new(),
            hidden: BTreeSet::new(),
            inflight: BTreeSet::new(),
            stale_agents: false,
            stale_spaces: false,
            agents_seen: false,
        }
    }

    fn send(&mut self, request: &ControlRequest) -> bool {
        let Some((_, tx)) = &self.conn else {
            return false;
        };
        match client::encode_control_seq(request, &mut self.control_seq) {
            Ok(bytes) => tx.send(bytes).is_ok(),
            Err(_) => false,
        }
    }

    fn send_wire(&mut self, terminal: u32, wire: &ClientWire) {
        let Some((_, tx)) = &self.conn else {
            return;
        };
        if let Ok(bytes) =
            client::encode_command_to(terminal, wire, self.seqs.entry(terminal).or_default())
        {
            let _ = tx.send(bytes);
        }
    }

    /// The rows the link's machine listed that are worth drawing.
    ///
    /// A remote's own default space (its lowest id, which its daemon never closes) is left
    /// out while it holds nothing and nobody gave it anything (no name, colour, goal or
    /// keep-awake or qass skip), as long as the machine has another space: it is a row nobody made and
    /// nobody can remove. A machine with only its default keeps it, that is where the first
    /// tab starts. The lowest id, not
    /// `closable == false`: an older daemon omits `closable`, which would hide a person's own
    /// empty spaces. This machine's own rows are all kept.
    fn shown_spaces(&self) -> impl Iterator<Item = &WorkspaceRow> {
        let default = (!self.name.is_empty() && self.spaces.len() > 1)
            .then(|| self.spaces.iter().map(|row| row.id).min())
            .flatten();
        self.spaces.iter().filter(move |row| {
            Some(row.id) != default
                || row.terminals > 0
                || row.asleep
                || row.keep_awake
                || row.qass_skip
                || row.ink.is_some()
                || row.name != "default"
                || row.goal.is_some()
        })
    }

    /// The space rows this link contributes to the merged list.
    ///
    /// A remote's rows say how its machine is reached, so the column can say it beside the
    /// name; this machine's rows say nothing, which is what a daemon sends.
    fn listed_spaces(&self, link: usize) -> Vec<WorkspaceRow> {
        if self.name.is_empty() && !self.listed {
            return Vec::new();
        }
        let state = |state| (!self.name.is_empty()).then_some(state);
        // A machine let go runs nothing: its spaces read as asleep, so an interface with
        // nothing else awake detaches rather than staying up, and this machine's daemon with
        // it, for a remote space that has no tab. Asking for one wakes the machine.
        if self.listed && *self.parked.borrow() {
            return self
                .shown_spaces()
                .map(|row| WorkspaceRow {
                    asleep: true,
                    link: state(LinkState::Parked),
                    ..row.clone()
                })
                .collect();
        }
        if self.listed && self.conn.is_some() {
            return self
                .shown_spaces()
                .map(|row| WorkspaceRow {
                    link: state(LinkState::Up),
                    ..row.clone()
                })
                .collect();
        }
        if self.listed {
            return self
                .shown_spaces()
                .map(|row| WorkspaceRow {
                    name: format!("{} ({})", row.name, self.reason),
                    terminals: 0,
                    // A link that is not answering cannot be asked to close anything, and
                    // what the far daemon would say about it is as stale as the rest of this
                    // row. No close, and no dead control drawn for one.
                    closable: false,
                    link: state(LinkState::Down),
                    ..row.clone()
                })
                .collect();
        }
        vec![WorkspaceRow {
            keep_awake: false,
            qass_skip: false,
            id: (link as i64) << WORKSPACE_SHIFT,
            name: format!("{} ({})", self.name, self.reason),
            terminals: 0,
            asleep: false,
            sleeping: Vec::new(),
            closable: false,
            ink: None,
            link: state(LinkState::Down),
            goal: None,
        }]
    }
}

/// Everything the mux knows, owned by one task.
struct Mux<W> {
    links: Vec<Option<Link>>,
    /// The link requests go to when nothing in them names one: the local daemon when there
    /// is one, else the first remote.
    home: usize,
    down: W,
    down_seqs: BTreeMap<(u8, u32), u32>,
    /// The ids of the last space list pushed, so a space that disappeared can be closed.
    pushed_spaces: Vec<i64>,
    /// The link of the terminal last typed into or sized, which is the focused one.
    last_link: usize,
    /// Where a person put each space in the merged list, across machines: each space as
    /// `<remote name>:<its id there>`, this machine's with an empty name. A space never
    /// placed follows the one listed before it.
    order: Vec<String>,
    /// Where `order` is kept, or nowhere (tests).
    order_file: Option<std::path::PathBuf>,
    /// For the tasks the mux starts itself: a new link's supervisor, a session list.
    events: mpsc::Sender<FromLink>,
    /// `qoop --space`: the one local space this interface shows. Every other space, and
    /// every tab in one, is left out of the lists it is given.
    only: Option<i64>,
    /// The remotes, by target, a push of a version was started for, with that version. A
    /// link let go after a quiet moment comes back on every ask, and each coming back is not
    /// a new download: one try per remote and version for the life of this interface.
    tried: BTreeSet<(String, String)>,
}

impl<W: AsyncWrite + Unpin> Mux<W> {
    fn link(&mut self, link: usize) -> Option<&mut Link> {
        self.links.get_mut(link).and_then(Option::as_mut)
    }

    async fn write(
        &mut self,
        channel: Channel,
        stream: u32,
        body: impl Into<Bytes>,
    ) -> io::Result<()> {
        let seq = self.down_seqs.entry((channel as u8, stream)).or_default();
        let frames = qoop_proto::chunk(channel, stream, *seq, None, body.into());
        *seq = seq.wrapping_add(frames.len() as u32);
        let mut buf = BytesMut::new();
        for frame in &frames {
            frame
                .encode(&mut buf)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        }
        self.down.write_all(&buf).await
    }

    async fn respond(&mut self, response: &ControlResponse) -> io::Result<()> {
        let body = serde_json::to_vec(response).map_err(io::Error::other)?;
        self.write(Channel::Control, 0, body).await
    }

    async fn failed(&mut self, id: String, reason: String) -> io::Result<()> {
        self.respond(&ControlResponse::Failed { id, reason }).await
    }

    /// Push the merged lists, closing first every space that is no longer in them.
    ///
    /// The interface only re-picks the space on screen when that space is closed, so a
    /// space that vanished without a close would leave it parked on a number nobody has.
    async fn push_lists(&mut self, id: &str) -> io::Result<()> {
        self.push_spaces(id).await?;
        self.push_agents(id).await
    }

    async fn push_spaces(&mut self, id: &str) -> io::Result<()> {
        let local_unheard = self.links[0].as_ref().is_some_and(|l| !l.listed);
        if local_unheard || self.links.iter().flatten().any(|l| l.stale_spaces) {
            return Ok(());
        }
        let spaces = self.merged_spaces();
        let now: Vec<i64> = spaces.iter().map(|w| w.id).collect();
        for gone in std::mem::take(&mut self.pushed_spaces) {
            if !now.contains(&gone) {
                self.respond(&ControlResponse::WorkspaceClosed {
                    id: "mux".into(),
                    workspace: gone,
                })
                .await?;
            }
        }
        self.pushed_spaces = now;
        self.respond(&ControlResponse::Workspaces {
            id: id.to_string(),
            workspaces: spaces,
        })
        .await
    }

    async fn push_agents(&mut self, id: &str) -> io::Result<()> {
        let local_unheard = self.links[0].as_ref().is_some_and(|l| !l.agents_seen);
        if local_unheard || self.links.iter().flatten().any(|l| l.stale_agents) {
            return Ok(());
        }
        let agents = self.merged_agents();
        let shown: BTreeSet<u32> = agents.iter().map(|row| row.terminal).collect();
        let splits = self
            .links
            .iter()
            .flatten()
            .flat_map(|l| l.splits.iter())
            .filter_map(|tree| tree.retain(&|t| shown.contains(&t)))
            .collect();
        self.respond(&ControlResponse::Agents {
            id: id.to_string(),
            agents,
            splits,
        })
        .await
    }

    fn merged_spaces(&self) -> Vec<WorkspaceRow> {
        let listed: Vec<WorkspaceRow> = self
            .links
            .iter()
            .enumerate()
            .filter_map(|(i, l)| l.as_ref().map(|l| l.listed_spaces(i)))
            .flatten()
            .filter(|w| self.only.is_none_or(|only| w.id == only))
            .collect();
        // In the order a person put them. One never placed follows the space its machine lists
        // before it, so a new space lands beside its machine's others; with none placed
        // before it on its machine, it goes last.
        let at = |w: &WorkspaceRow| match self.space_key(w.id) {
            Some(key) => self.order.iter().position(|k| *k == key),
            // A machine not listed yet stands where its spaces were put.
            None => {
                let name = &self.links.get(workspace_in(w.id).0)?.as_ref()?.name;
                let prefix = format!("{name}:");
                self.order.iter().position(|k| k.starts_with(&prefix))
            }
        };
        let last = 2 * self.order.len() + 2;
        let mut after: Option<(usize, usize)> = None;
        let mut ranked: Vec<(usize, usize, WorkspaceRow)> = listed
            .into_iter()
            .enumerate()
            .map(|(i, w)| {
                let machine = workspace_in(w.id).0;
                let rank = match at(&w) {
                    Some(place) => {
                        after = Some((machine, 2 * place + 2));
                        2 * place + 2
                    }
                    None => match after {
                        Some((m, rank)) if m == machine => rank + 1,
                        _ => last,
                    },
                };
                (rank, i, w)
            })
            .collect();
        ranked.sort_by_key(|(rank, i, _)| (*rank, *i));
        ranked.into_iter().map(|(_, _, w)| w).collect()
    }

    /// A space's name in the saved order: its machine's name and its id there. None for the
    /// placeholder a remote shows before it has listed, which is no space to keep a place for.
    fn space_key(&self, workspace: i64) -> Option<String> {
        let (link, id) = workspace_in(workspace);
        if link > 0 && id == 0 {
            return None;
        }
        let name = &self.links.get(link)?.as_ref()?.name;
        Some(format!("{name}:{id}"))
    }

    /// Fold a machine's own list into the order: the file says where each machine's spaces
    /// sit among the others', the machine says their order among themselves. The places of
    /// spaces it no longer lists go first, as ids are reused and a new space must not take a
    /// closed one's place; then, when its list can be trusted with it, its kept places are
    /// filled again with its spaces in the order it lists them, so a reorder made on it by
    /// anyone (the phone, another window) shows here too. Read from the file first: another
    /// interface may have changed it.
    fn refresh_order(&mut self, link: usize, name: &str, listed: &[String]) {
        let Some(file) = self.order_file.clone() else {
            return;
        };
        self.order = load_order(&file);
        let prefix = format!("{name}:");
        let mut order: Vec<String> = self
            .order
            .iter()
            .filter(|k| !k.starts_with(&prefix) || listed.contains(k))
            .cloned()
            .collect();
        let trusted = self
            .links
            .get(link)
            .and_then(Option::as_ref)
            .is_some_and(|l| l.order_owed == 0 && !l.order_off);
        if trusted {
            let slots: Vec<usize> = (0..order.len())
                .filter(|at| order[*at].starts_with(&prefix))
                .collect();
            let fresh: Vec<String> = listed
                .iter()
                .filter(|k| order.contains(k))
                .cloned()
                .collect();
            for (slot, key) in slots.into_iter().zip(fresh) {
                order[slot] = key;
            }
        }
        if order != self.order {
            let _ = save_order(&file, &order);
            self.order = order;
        }
    }

    /// Put a space just before `before` in the merged list, or last, and keep the order.
    /// Answers the machine the space is on, its id there, and the next space of the same
    /// machine after it now, so that machine can keep its own list in the same order.
    fn place_space(
        &mut self,
        workspace: i64,
        before: Option<i64>,
    ) -> Option<(usize, i64, Option<i64>)> {
        // Another interface may have moved something since this one last read the order.
        if let Some(file) = &self.order_file {
            self.order = load_order(file);
        }
        let shown: Vec<i64> = self.merged_spaces().iter().map(|w| w.id).collect();
        let moved = self
            .space_key(workspace)
            .filter(|_| shown.contains(&workspace))?;
        // Every space shown has a place first, an unplaced one where it is shown: after the
        // space before it on its machine, or last. A space of a machine that is away keeps
        // the place it had.
        let mut keys = self.order.clone();
        let mut prev: Option<(usize, String)> = None;
        for id in &shown {
            let Some(key) = self.space_key(*id) else {
                continue;
            };
            let machine = workspace_in(*id).0;
            if !keys.contains(&key) {
                let at = prev
                    .as_ref()
                    .filter(|(m, _)| *m == machine)
                    .and_then(|(_, p)| keys.iter().position(|k| k == p))
                    .map_or(keys.len(), |at| at + 1);
                keys.insert(at, key.clone());
            }
            prev = Some((machine, key));
        }
        keys.retain(|k| *k != moved);
        // Last means after the last space shown here, which a view of one machine also
        // shows, not after the places of spaces this view cannot see.
        let end = shown
            .iter()
            .filter_map(|id| self.space_key(*id))
            .filter_map(|key| keys.iter().position(|k| *k == key))
            .max()
            .map_or(keys.len(), |at| at + 1);
        let at = before
            .and_then(|b| self.space_key(b))
            .and_then(|b| keys.iter().position(|k| *k == b))
            .unwrap_or(end);
        keys.insert(at, moved);
        self.order = keys;
        if let Some(file) = &self.order_file {
            // ponytail: a failed save keeps the order for this run only; the list still moves.
            let _ = save_order(file, &self.order);
        }
        let (link, id) = workspace_in(workspace);
        let ids: Vec<i64> = self.merged_spaces().iter().map(|w| w.id).collect();
        let next = ids
            .iter()
            .skip_while(|w| **w != workspace)
            .skip(1)
            .map(|w| workspace_in(*w))
            .find(|(l, _)| *l == link)
            .map(|(_, w)| w);
        Some((link, id, next))
    }

    fn merged_agents(&self) -> Vec<AgentRow> {
        self.links
            .iter()
            .flatten()
            .flat_map(|l| l.agents.iter().cloned())
            .filter(|row| self.only.is_none_or(|only| row.workspace == only))
            .collect()
    }

    /// Start keeping `remote` connected, as a new link. `None` when the numbering is full.
    fn add_remote(&mut self, remote: Remote) -> Option<usize> {
        if self.links.len() > MAX_REMOTES {
            return None;
        }
        let link = self.links.len();
        let entry = Link::new(remote.name.clone(), Some(remote.clone()));
        tokio::spawn(supervise(
            link,
            remote,
            self.events.clone(),
            entry.wake.clone(),
            entry.parked.clone(),
        ));
        self.links.push(Some(entry));
        Some(link)
    }

    /// Start a link for every saved remote that has none: a machine added from a shell
    /// (`qoop remote add`) after this interface started. A list that does not read is left
    /// for the next look; the start already said so.
    async fn adopt_saved(&mut self) -> io::Result<()> {
        let Ok(saved) = qoop_remote::saved::load() else {
            return Ok(());
        };
        let mut added = false;
        for remote in saved {
            let linked = self
                .links
                .iter()
                .flatten()
                .any(|l| l.remote.as_ref().is_some_and(|r| r.target == remote.target));
            if !linked {
                added |= self.add_remote(remote).is_some();
            }
        }
        if added {
            self.push_lists("mux").await?;
        }
        Ok(())
    }

    /// Something was asked of a link that is down: try it now rather than after the wait.
    fn wake(&mut self, link: usize) {
        if let Some(l) = self.link(link)
            && l.conn.is_none()
        {
            l.wake.notify_one();
        }
    }

    /// A remote just came up running `server_version`: when this qoop is newer, put it on
    /// that machine's disk in the background and ask its daemon to move to it, once per
    /// remote and version. Whatever goes wrong is one line on the status bar, never a retry.
    fn update_remote(&mut self, link: usize, server_version: &str) {
        let here = env!("CARGO_PKG_VERSION");
        let Some(Some(l)) = self.links.get(link) else {
            return;
        };
        let Some(remote) = &l.remote else {
            return;
        };
        if !qoop_runtime::paths::is_newer_version(here, server_version)
            || !self.tried.insert((remote.target.clone(), here.to_string()))
        {
            return;
        }
        let (name, target) = (l.name.clone(), remote.target.clone());
        let events = self.events.clone();
        tokio::spawn(async move {
            let message = match crate::link::update(&target, here, false).await {
                Ok(crate::link::Update::OnDisk(version, warning)) => {
                    if let Some(warning) = warning {
                        let _ = events
                            .send(FromLink::Reply(ControlResponse::Failed {
                                id: UPDATE_NOTICE.into(),
                                reason: format!("qoop {here} is on {name}, but {warning}"),
                            }))
                            .await;
                    }
                    FromLink::Arm { link, version }
                }
                // Said once (`tried` keeps this to one attempt per version) unless the
                // releases are GitHub's, which are private and answer 404 to anyone else.
                Ok(crate::link::Update::NoRelease) if qoop_runtime::release::reads_github() => {
                    return;
                }
                Ok(crate::link::Update::NoRelease) => FromLink::Reply(ControlResponse::Failed {
                    id: UPDATE_NOTICE.into(),
                    reason: format!(
                        "the releases have no build of qoop {here}; {name} stays as it is"
                    ),
                }),
                Err(e) => FromLink::Reply(ControlResponse::Failed {
                    id: UPDATE_NOTICE.into(),
                    reason: format!("qoop {here} could not be put on {name}: {e}"),
                }),
            };
            let _ = events.send(message).await;
        });
    }

    /// Add the machine at `target` from the interface, keep it in the list of remotes,
    /// and say under what name it will appear. A machine already kept is taken there; a
    /// new one is asked first whether it has a qoop, in a task, since ssh can take seconds.
    async fn connect_remote(&mut self, id: String, target: String) -> io::Result<()> {
        // Written one way whatever way it was typed, so the same command twice is one machine.
        let Some(target) = qoop_remote::saved::canonical(&target) else {
            return self.failed(id, not_ssh(&target)).await;
        };
        let same = |r: &Remote| r.target == target;
        let linked = self
            .links
            .iter()
            .position(|l| l.as_ref().and_then(|l| l.remote.as_ref()).is_some_and(same));
        // Already kept: connecting it means "take me there". A link already running is
        // woken, which after an install is exactly what brings it up.
        if let Some(link) = linked {
            let name = self.link(link).map(|l| l.name.clone()).unwrap_or_default();
            self.wake(link);
            return self
                .respond(&ControlResponse::RemoteAdded { id, name })
                .await;
        }
        let events = self.events.clone();
        tokio::spawn(async move {
            let message = match crate::link::probe(&target).await {
                Ok(crate::link::Probe::Ready { .. }) => FromLink::Probed { id, target },
                Ok(crate::link::Probe::Missing(platform)) => {
                    FromLink::Reply(ControlResponse::RemoteMissing {
                        id,
                        target,
                        platform: platform.describe(),
                        root: platform.root,
                    })
                }
                Err(e) => FromLink::Reply(ControlResponse::Failed {
                    id,
                    reason: e.to_string(),
                }),
            };
            let _ = events.send(message).await;
        });
        Ok(())
    }

    /// Keep a machine that answered with a qoop, and start its link.
    async fn add_new_remote(&mut self, id: String, target: String) -> io::Result<()> {
        // Before anything is kept: a machine saved that this interface cannot show would
        // appear only on the next start, as if added then.
        if self.links.len() > MAX_REMOTES {
            return self.failed(id, "no room for another machine".into()).await;
        }
        let file = qoop_remote::saved::path();
        // Read again right before writing, so a remote added from a shell a moment ago is
        // kept rather than written over, and counted when checking names. A file that does
        // not parse is refused rather than replaced: it is what the person wrote.
        let mut saved = match qoop_remote::saved::load_from(&file) {
            Ok(saved) => saved,
            Err(e) => return self.failed(id, format!("remotes not loaded: {e}")).await,
        };
        let known: Vec<Remote> = self
            .links
            .iter()
            .flatten()
            .filter_map(|l| l.remote.clone())
            .chain(saved.iter().cloned())
            .collect();
        // One only in the file (added from a shell while this interface ran) is started.
        if let Some(kept) = saved.iter().find(|r| r.target == target).cloned() {
            let name = kept.name.clone();
            self.add_remote(kept);
            self.push_lists("mux").await?;
            return self
                .respond(&ControlResponse::RemoteAdded { id, name })
                .await;
        }
        let taken: Vec<&str> = known.iter().map(|r| r.name.as_str()).collect();
        let remote = Remote {
            name: qoop_remote::saved::name_for(&target, &taken),
            target,
        };
        saved.push(remote.clone());
        if let Err(e) = qoop_remote::saved::save_to(&file, &saved) {
            return self.failed(id, format!("remotes not saved: {e}")).await;
        }
        let name = remote.name.clone();
        self.add_remote(remote);
        self.push_lists("mux").await?;
        self.respond(&ControlResponse::RemoteAdded { id, name })
            .await
    }

    /// Send a request that names one daemon, or answer for a daemon that is not there.
    async fn send_one(&mut self, link: usize, request: ControlRequest) -> io::Result<()> {
        let id = request.request_id().to_string();
        // A machine let go because nothing there was awake: this ask is what brings it back,
        // and it is answered once the link is up rather than failed as offline. A connection
        // still closing keeps the ask too, and its `Down` starts the next one.
        if let Some(l) = self.link(link)
            && *l.parked.borrow()
        {
            l.pending.push(request);
            if l.conn.is_none() {
                l.reason = "connecting".to_string();
                l.parked.send_replace(false);
            }
            return Ok(());
        }
        let sent = self.link(link).is_some_and(|l| {
            let sent = l.send(&request);
            if sent {
                l.inflight.insert(id.clone());
            }
            sent
        });
        if sent {
            return Ok(());
        }
        self.wake(link);
        let reason = match self.link(link) {
            Some(l) if !l.name.is_empty() => format!("{} is offline", l.name),
            _ => "no such daemon".to_string(),
        };
        self.failed(id, reason).await
    }

    async fn on_tui(&mut self, message: FromTui) -> io::Result<()> {
        match message {
            FromTui::Wire { terminal, wire } => {
                let (link, id) = terminal_in(terminal);
                // The id of the sender is another daemon's number on a remote, and means
                // nothing there.
                let wire = match wire {
                    ClientWire::Input { bytes, .. } if link != 0 => ClientWire::Input {
                        bytes,
                        from_terminal: None,
                        from_token: None,
                    },
                    other => other,
                };
                if let Some(l) = self.link(link) {
                    l.send_wire(id, &wire);
                    self.last_link = link;
                }
                self.wake(link);
                Ok(())
            }
            FromTui::Control(request) => self.control(request).await,
        }
    }

    async fn control(&mut self, request: ControlRequest) -> io::Result<()> {
        use ControlRequest as R;
        match request {
            // `qoop --remote` has no local daemon, and its window cannot start the qoop on that
            // machine in its own place (only a window of this machine's daemon does): so it is
            // never offered the restart, and the remote is not asked.
            R::UpdateState { id } if self.home != 0 => {
                self.respond(&ControlResponse::UpdateState { id, ready: None })
                    .await
            }
            R::ListAgents { .. } | R::ListWorkspaces { .. } => {
                let id = request.request_id().to_string();
                let mut asked = false;
                for l in self.links.iter_mut().flatten() {
                    asked |= l.send(&request);
                }
                // Nobody to ask: what is known is the answer, so the interface still gets
                // one, placeholders included.
                if !asked {
                    self.push_lists(&id).await?;
                }
                Ok(())
            }
            R::Subscribe { id, terminals } => {
                for (link, ids) in split(&terminals) {
                    if let Some(l) = self.link(link) {
                        // Only what reached the daemon is wanted: an entry kept for a link
                        // that was down would let its renders through after it came back.
                        if l.send(&R::Subscribe {
                            id: id.clone(),
                            terminals: ids.clone(),
                        }) {
                            l.desired.extend(&ids);
                        }
                    }
                }
                Ok(())
            }
            R::Unsubscribe { id, terminals } => {
                for (link, ids) in split(&terminals) {
                    if let Some(l) = self.link(link) {
                        for t in &ids {
                            l.desired.remove(t);
                        }
                        l.send(&R::Unsubscribe {
                            id: id.clone(),
                            terminals: ids,
                        });
                    }
                }
                Ok(())
            }
            R::CreateTerminal {
                id,
                cwd,
                workspace,
                name,
            } => {
                let (link, workspace) = match workspace {
                    Some(w) => {
                        let (link, w) = workspace_in(w);
                        (link, Some(w))
                    }
                    // Scoped, a tab with no space named goes to the one on screen, not to
                    // the daemon's default where this interface would never show it.
                    None => (self.home, self.only),
                };
                // A directory is a place on one machine. Carried to another it is at best
                // missing and at worst somewhere else with the same name. It goes where it
                // is known: a directory a tab of that machine reports being in, or any one
                // when that machine is the one last typed into, the focused one.
                let known = |dir: &String| {
                    self.links
                        .get(link)
                        .and_then(Option::as_ref)
                        .is_some_and(|l| l.agents.iter().any(|a| a.cwd.as_ref() == Some(dir)))
                };
                let cwd = cwd.filter(|dir| link == self.last_link || known(dir));
                self.send_one(
                    link,
                    R::CreateTerminal {
                        id,
                        cwd,
                        workspace,
                        name,
                    },
                )
                .await
            }
            R::CreateWorkspace { id, .. } if self.only.is_some() => {
                self.failed(id, "this shows one space; `qoop` shows them all".into())
                    .await
            }
            R::CreateWorkspace { id, name } => {
                let named = name.split_once(':').and_then(|(prefix, rest)| {
                    self.links
                        .iter()
                        .position(|l| {
                            l.as_ref()
                                .is_some_and(|l| !l.name.is_empty() && l.name == prefix)
                        })
                        .map(|link| (link, rest.to_string()))
                });
                let (link, name) = named.unwrap_or((self.home, name));
                self.send_one(link, R::CreateWorkspace { id, name }).await
            }
            R::RenameWorkspace {
                id,
                workspace,
                name,
            } => {
                let (link, workspace) = workspace_in(workspace);
                let prefix = self
                    .link(link)
                    .map(|l| format!("{}:", l.name))
                    .unwrap_or_default();
                let name = match name.strip_prefix(&prefix) {
                    Some(rest) if prefix.len() > 1 => rest.to_string(),
                    _ => name,
                };
                self.send_one(
                    link,
                    R::RenameWorkspace {
                        id,
                        workspace,
                        name,
                    },
                )
                .await
            }
            R::SetWorkspaceInk { id, workspace, ink } => {
                let (link, workspace) = workspace_in(workspace);
                self.send_one(link, R::SetWorkspaceInk { id, workspace, ink })
                    .await
            }
            R::CloseWorkspace { id, workspace } => {
                let (link, workspace) = workspace_in(workspace);
                self.send_one(link, R::CloseWorkspace { id, workspace })
                    .await
            }
            // The merged list is this interface's: its order is kept here, across machines.
            // The space's own machine is told its part, so its list reads the same order to
            // whoever reads that machine alone; one too old to know the ask refuses it, and
            // that refusal is the mux's own housekeeping.
            R::MoveWorkspace {
                id,
                workspace,
                before,
            } => {
                let Some((link, local, next)) = self.place_space(workspace, before) else {
                    return self
                        .failed(id, format!("there is no workspace {workspace}"))
                        .await;
                };
                // The machine's part: this move alone when it holds the order so far, else its
                // whole order again, each space to the end in turn, since one move on top of an
                // order it never took would put the wrong space in a dragged one's place.
                let asks: Vec<ControlRequest> = match self.link(link) {
                    Some(l) if l.order_off => {
                        let prefix = format!("{}:", l.name);
                        // Only spaces it lists now: one closed since would be refused.
                        let listed: Vec<i64> =
                            l.spaces.iter().map(|w| workspace_in(w.id).1).collect();
                        let own: Vec<i64> = self
                            .order
                            .iter()
                            .filter_map(|k| k.strip_prefix(&prefix)?.parse().ok())
                            .filter(|w| listed.contains(w))
                            .collect();
                        own.into_iter()
                            .enumerate()
                            .map(|(at, w)| R::MoveWorkspace {
                                id: format!("{ORDER_ID}{id}-{at}"),
                                workspace: w,
                                before: None,
                            })
                            .collect()
                    }
                    _ => vec![R::MoveWorkspace {
                        id: format!("{ORDER_ID}{id}"),
                        workspace: local,
                        before: next,
                    }],
                };
                // A machine that is away misses its part, and holds its own order until it
                // takes a whole one again.
                if let Some(l) = self.link(link) {
                    let mut all = true;
                    for ask in &asks {
                        if l.send(ask) {
                            l.order_owed += 1;
                        } else {
                            all = false;
                        }
                    }
                    l.order_off = !all;
                }
                // The list moves now, not when every machine has answered again.
                self.push_spaces("mux").await?;
                self.respond(&ControlResponse::Renamed { id }).await
            }
            // A tab, the space it goes to and the tab it goes before all have to be on one
            // machine: each daemon only knows its own.
            R::MoveTerminal {
                id,
                terminal,
                workspace,
                place,
                leave_split,
            } => {
                use qoop_runtime::control::Place;
                let (link, terminal) = terminal_in(terminal);
                let (to, workspace) = workspace_in(workspace);
                // A remote on a release from before places existed ignores them and answers
                // a move into its own space as done: said here instead, rather than shown done.
                if link != 0
                    && (place.is_some() || leave_split)
                    && let Some(l) = self.link(link)
                    && !qoop_runtime::paths::is_newer_version(&l.server_version, BEFORE_PLACES)
                {
                    let name = l.name.clone();
                    return self
                        .failed(
                            id,
                            format!("{name} runs an older qoop: it places tabs once it updates"),
                        )
                        .await;
                }
                let before = match place {
                    Some(Place::Before(t)) => Some(terminal_in(t)),
                    _ => None,
                };
                if to != link || before.is_some_and(|(at, _)| at != link) {
                    return self
                        .failed(id, "a tab cannot move between machines".to_string())
                        .await;
                }
                let place = match before {
                    Some((_, t)) => Some(Place::Before(t)),
                    None => place,
                };
                self.send_one(
                    link,
                    R::MoveTerminal {
                        id,
                        terminal,
                        workspace,
                        place,
                        leave_split,
                    },
                )
                .await
            }
            // The machine the agent runs on, in its own numbering.
            R::SetParked { id, terminal, on } => {
                let (link, terminal) = terminal_in(terminal);
                self.send_one(link, R::SetParked { id, terminal, on }).await
            }
            // The machine whose idle sleep it is: the space's, a pane going with its space.
            R::SetKeepAwake {
                id,
                workspace,
                terminal,
                on,
            } => {
                let (link, workspace) = workspace_in(workspace);
                let terminal = terminal.map(|t| terminal_in(t).1);
                self.send_one(
                    link,
                    R::SetKeepAwake {
                        id,
                        workspace,
                        terminal,
                        on,
                    },
                )
                .await
            }
            // The machine whose qass it is: the space's, a pane going with its space.
            R::SetQassSkip {
                id,
                workspace,
                terminal,
                on,
            } => {
                let (link, workspace) = workspace_in(workspace);
                let terminal = terminal.map(|t| terminal_in(t).1);
                self.send_one(
                    link,
                    R::SetQassSkip {
                        id,
                        workspace,
                        terminal,
                        on,
                    },
                )
                .await
            }
            // To the machine the space is on, in its own numbering; a machine let go because
            // everything there slept is brought back by the wake itself.
            // A tab, when one is named, goes with its space: the same machine.
            R::SleepWorkspace {
                id,
                workspace,
                terminal,
            } => {
                let (link, workspace) = workspace_in(workspace);
                let terminal = terminal.map(|t| terminal_in(t).1);
                self.send_one(
                    link,
                    R::SleepWorkspace {
                        id,
                        workspace,
                        terminal,
                    },
                )
                .await
            }
            R::WakeWorkspace {
                id,
                workspace: Some(workspace),
                terminal,
            } => {
                let (link, workspace) = workspace_in(workspace);
                let terminal = terminal.map(|t| terminal_in(t).1);
                self.send_one(
                    link,
                    R::WakeWorkspace {
                        id,
                        workspace: Some(workspace),
                        terminal,
                    },
                )
                .await
            }
            R::RenameTerminal { id, terminal, name } => {
                let (link, terminal) = terminal_in(terminal);
                self.send_one(link, R::RenameTerminal { id, terminal, name })
                    .await
            }
            // What this machine calls a conversation lives with the agent's own daemon,
            // which is the one that knows which conversation the terminal is in.
            R::RenameSession {
                id,
                terminal,
                title,
            } => {
                let (link, terminal) = terminal_in(terminal);
                self.send_one(
                    link,
                    R::RenameSession {
                        id,
                        terminal,
                        title,
                    },
                )
                .await
            }
            // A tab lives on one daemon, so its tree does: sent to the daemon of its
            // first pane in that daemon's numbering. A tree reaching across daemons is
            // not one any daemon could bring back, and is refused.
            R::SetSplit { id, tree } => match split_in(&tree) {
                Some((link, tree)) => self.send_one(link, R::SetSplit { id, tree }).await,
                None => {
                    self.failed(id, "a split cannot span two hosts".to_string())
                        .await
                }
            },
            R::CloseTerminal { id, terminal } => {
                let (link, terminal) = terminal_in(terminal);
                self.send_one(link, R::CloseTerminal { id, terminal }).await
            }
            R::Scrollback {
                id,
                terminal,
                up,
                rows,
            } => {
                let (link, terminal) = terminal_in(terminal);
                self.send_one(
                    link,
                    R::Scrollback {
                        id,
                        terminal,
                        up,
                        rows,
                    },
                )
                .await
            }
            // The chat is parsed from the agent's own files, so the machine that owns the
            // terminal answers, in its own numbering.
            R::Messages {
                id,
                terminal,
                skip,
                limit,
                revision,
            } => {
                let (link, terminal) = terminal_in(terminal);
                self.send_one(
                    link,
                    R::Messages {
                        id,
                        terminal,
                        skip,
                        limit,
                        revision,
                    },
                )
                .await
            }
            R::MessageDetail {
                id,
                terminal,
                tool_id,
                max_bytes,
            } => {
                let (link, terminal) = terminal_in(terminal);
                self.send_one(
                    link,
                    R::MessageDetail {
                        id,
                        terminal,
                        tool_id,
                        max_bytes,
                    },
                )
                .await
            }
            // A history search names a terminal like a scrollback read, and goes where it is.
            R::SearchScrollback {
                id,
                terminal,
                query,
                from,
                backward,
            } => {
                let (link, terminal) = terminal_in(terminal);
                self.send_one(
                    link,
                    R::SearchScrollback {
                        id,
                        terminal,
                        query,
                        from,
                        backward,
                    },
                )
                .await
            }
            R::ListFocusEvents {
                id,
                terminal,
                last_n,
            } => {
                let (link, terminal) = terminal_in(terminal);
                self.send_one(
                    link,
                    R::ListFocusEvents {
                        id,
                        terminal,
                        last_n,
                    },
                )
                .await
            }
            R::InstallRemote { id, target } => {
                let Some(target) = qoop_remote::saved::canonical(&target) else {
                    return self.failed(id, not_ssh(&target)).await;
                };
                // A copy of several megabytes over ssh: in a task, like the probe.
                let events = self.events.clone();
                tokio::spawn(async move {
                    let reply = match crate::link::install(&target).await {
                        Ok(version) => ControlResponse::RemoteInstalled {
                            id,
                            target,
                            version,
                        },
                        Err(e) => ControlResponse::Failed {
                            id,
                            reason: e.to_string(),
                        },
                    };
                    let _ = events.send(FromLink::Reply(reply)).await;
                });
                Ok(())
            }
            R::ConnectRemote { id, target } => self.connect_remote(id, target).await,
            // A picture for a tab is kept where the tab runs, so a remote agent can read it.
            R::SaveUpload {
                id,
                ext,
                data,
                terminal: Some(terminal),
            } => {
                let (link, terminal) = terminal_in(terminal);
                let request = R::SaveUpload {
                    id,
                    ext,
                    data,
                    terminal: Some(terminal),
                };
                self.send_one(link, request).await
            }
            // Everything else the interface sends names no terminal: it is about the daemon
            // this machine runs, or the one remote when there is no local one.
            other => self.send_one(self.home, other).await,
        }
    }

    async fn on_link(&mut self, message: FromLink) -> io::Result<()> {
        match message {
            FromLink::Up {
                link,
                conn,
                writer,
                server_version,
            } => {
                self.update_remote(link, &server_version);
                let Some(l) = self.link(link) else {
                    return Ok(());
                };
                // Back on a newer qoop than before, which is the move a waiting line said
                // would come: that line is replaced by this one.
                let moved = (!l.server_version.is_empty()
                    && qoop_runtime::paths::is_newer_version(&server_version, &l.server_version))
                .then(|| format!("{} now runs qoop {server_version}", l.name));
                l.conn = Some((conn, writer));
                l.server_version = server_version;
                l.control_seq = 0;
                l.seqs.clear();
                l.hidden.clear();
                l.reason = "connecting".to_string();
                for request in std::mem::take(&mut l.pending) {
                    let id = request.request_id().to_string();
                    if l.send(&request) {
                        l.inflight.insert(id);
                    }
                }
                l.send(&ControlRequest::ListWorkspaces {
                    id: "mux-spaces".into(),
                });
                l.send(&ControlRequest::ListAgents {
                    id: "mux-agents".into(),
                });
                let mut line = moved;
                if let Some(version) = l.arm.take()
                    && qoop_runtime::paths::is_newer_version(&version, &l.server_version)
                {
                    if keeps_promises(&l.server_version) {
                        l.send(&restart_when_safe(version));
                    } else {
                        line = Some(left_for_a_hand_update(&l.name, &l.server_version));
                    }
                }
                if let Some(line) = line {
                    self.respond(&ControlResponse::RestartArmed {
                        id: UPDATE_NOTICE.into(),
                        waiting_on: Some(line),
                    })
                    .await?;
                }
                Ok(())
            }
            FromLink::Arm { link, version } => {
                let Some(l) = self.link(link) else {
                    return Ok(());
                };
                // Down, or going down with its pump gone before the mux has heard: asked on
                // the next connection, if its daemon is still older then.
                let mut said = None;
                let unsent = l.conn.is_none()
                    || (qoop_runtime::paths::is_newer_version(&version, &l.server_version)
                        && if keeps_promises(&l.server_version) {
                            !l.send(&restart_when_safe(version.clone()))
                        } else {
                            said = Some(left_for_a_hand_update(&l.name, &l.server_version));
                            false
                        });
                if unsent {
                    l.arm = Some(version);
                }
                if let Some(line) = said {
                    self.respond(&ControlResponse::RestartArmed {
                        id: UPDATE_NOTICE.into(),
                        waiting_on: Some(line),
                    })
                    .await?;
                }
                Ok(())
            }
            FromLink::Down { link, conn, reason } => {
                let Some(l) = self.link(link) else {
                    return Ok(());
                };
                if l.conn.as_ref().is_none_or(|(c, _)| *c != conn) {
                    // An attempt that never came up: only the reason changes.
                    // Said again only when it says something new: a host that keeps failing
                    // the same way is not news every thirty seconds.
                    let reason = format!("offline: {reason}");
                    // What asked for it is answered now, not held while the tries go on.
                    let waiting = if l.conn.is_none() {
                        std::mem::take(&mut l.pending)
                    } else {
                        Vec::new()
                    };
                    let said = reason.clone();
                    if l.conn.is_none() && reason.len() > "offline: ".len() && l.reason != reason {
                        l.reason = reason;
                        self.push_lists("mux").await?;
                    }
                    for request in waiting {
                        self.failed(request.request_id().to_string(), said.clone())
                            .await?;
                    }
                    return Ok(());
                }
                l.conn = None;
                // Its part of an order, asked and never answered, may never have been taken.
                l.order_off |= l.order_owed > 0;
                l.order_owed = 0;
                l.stale_agents = false;
                l.stale_spaces = false;
                l.agents.clear();
                l.splits.clear();
                l.desired.clear();
                // Asked for while it closed: up again at once for what is waiting.
                if !l.pending.is_empty() {
                    l.parked.send_replace(false);
                }
                l.reason = if !l.pending.is_empty() {
                    "connecting".to_string()
                } else if *l.parked.borrow() {
                    "asleep".to_string()
                } else if reason.is_empty() {
                    "offline".to_string()
                } else {
                    format!("offline: {reason}")
                };
                let name = l.name.clone();
                let pending: Vec<String> = std::mem::take(&mut l.inflight).into_iter().collect();
                for id in pending {
                    self.failed(id, format!("{name} went offline")).await?;
                }
                self.push_lists("mux").await
            }
            FromLink::Probed { id, target } => self.add_new_remote(id, target).await,
            FromLink::ParkCheck { link, conn } => {
                let Some(l) = self.link(link) else {
                    return Ok(());
                };
                // Still the same connection, still nothing awake over there, and nothing
                // asked of it meanwhile: let it go.
                let same = l.conn.as_ref().is_some_and(|(c, _)| *c == conn);
                if same && l.inflight.is_empty() && !l.stale_spaces && quiet(&l.spaces) {
                    l.parked.send_replace(true);
                }
                Ok(())
            }
            FromLink::Reply(reply) => {
                // A machine that just got its qoop: any link already kept for it tries now
                // rather than at the end of its wait.
                if let ControlResponse::RemoteInstalled { target, .. } = &reply {
                    let waiting: Vec<usize> = self
                        .links
                        .iter()
                        .enumerate()
                        .filter(|(_, l)| {
                            l.as_ref()
                                .and_then(|l| l.remote.as_ref())
                                .is_some_and(|r| &r.target == target)
                        })
                        .map(|(i, _)| i)
                        .collect();
                    for link in waiting {
                        self.wake(link);
                    }
                }
                self.respond(&reply).await
            }
            FromLink::Said {
                link,
                conn,
                incoming,
            } => {
                let current = self
                    .link(link)
                    .is_some_and(|l| l.conn.as_ref().is_some_and(|(c, _)| *c == conn));
                if !current {
                    return Ok(());
                }
                self.said(link, incoming).await
            }
        }
    }

    async fn said(&mut self, link: usize, incoming: Incoming) -> io::Result<()> {
        match incoming {
            // A screen or a picture, passed on as it came: only the terminal it is for is
            // renumbered, and the body is never parsed.
            Incoming::Raw { terminal, body } => {
                if !self.wanted(link, terminal) {
                    return Ok(());
                }
                let Some(out) = terminal_out(link, terminal) else {
                    return Ok(());
                };
                self.write(Channel::Terminal, out, body).await
            }
            // Links are read raw, so these never arrive here.
            Incoming::Render { .. } | Incoming::Image { .. } | Incoming::Copy { .. } => Ok(()),
            Incoming::Event(EventMessage::AgentChanged { terminal }) => {
                let Some(terminal) = terminal_out(link, terminal) else {
                    return Ok(());
                };
                let body = serde_json::to_vec(&EventMessage::AgentChanged { terminal })
                    .map_err(io::Error::other)?;
                self.write(Channel::Event, 0, body).await
            }
            // Renumbered like a change, or a remote's terminal 3 would show the local one.
            Incoming::Event(EventMessage::Focus { terminal }) => {
                let Some(terminal) = terminal_out(link, terminal) else {
                    return Ok(());
                };
                let body = serde_json::to_vec(&EventMessage::Focus { terminal })
                    .map_err(io::Error::other)?;
                self.write(Channel::Event, 0, body).await
            }
            // Renumbered too: a remote's recording in its terminal 3 must never open the
            // microphone for the local terminal 3.
            Incoming::Event(EventMessage::Listen {
                terminal,
                listener,
                on,
            }) => {
                let Some(terminal) = terminal_out(link, terminal) else {
                    return Ok(());
                };
                let event = EventMessage::Listen {
                    terminal,
                    listener,
                    on,
                };
                let body = serde_json::to_vec(&event).map_err(io::Error::other)?;
                self.write(Channel::Event, 0, body).await
            }
            // Renumbered like a change: a remote's chat of terminal 3 is not the local one's.
            Incoming::Event(EventMessage::MessagesChanged { terminal, revision }) => {
                let Some(terminal) = terminal_out(link, terminal) else {
                    return Ok(());
                };
                let body =
                    serde_json::to_vec(&EventMessage::MessagesChanged { terminal, revision })
                        .map_err(io::Error::other)?;
                self.write(Channel::Event, 0, body).await
            }
            Incoming::Event(event) => {
                let body = serde_json::to_vec(&event).map_err(io::Error::other)?;
                self.write(Channel::Event, 0, body).await
            }
            Incoming::Control(response) => self.answer(link, response).await,
        }
    }

    /// Whether the interface wants this link's renders for `terminal`.
    ///
    /// A new connection is subscribed to its daemon's lowest terminal before anybody asks.
    /// Locally that is how the interface's first screen arrives, and it stays as it is. For
    /// a remote it is a screen streamed over ssh that nobody is looking at, so it is
    /// unsubscribed the first time it shows up.
    fn wanted(&mut self, link: usize, terminal: u32) -> bool {
        if link == 0 {
            return true;
        }
        let Some(l) = self.link(link) else {
            return false;
        };
        if l.desired.contains(&terminal) {
            return true;
        }
        if l.hidden.insert(terminal) {
            l.send(&ControlRequest::Unsubscribe {
                id: "mux-hide".into(),
                terminals: vec![terminal],
            });
        }
        false
    }

    async fn answer(&mut self, link: usize, response: ControlResponse) -> io::Result<()> {
        use ControlResponse as C;
        let id = response.request_id().to_string();
        let events = self.events.clone();
        let Some(l) = self.link(link) else {
            return Ok(());
        };
        l.inflight.remove(&id);
        let name = l.name.clone();
        match response {
            C::Agents { agents, splits, .. } => {
                l.splits = splits
                    .iter()
                    .filter_map(|tree| tree.map(&|t| terminal_out(link, t)))
                    .collect();
                l.agents = agents
                    .into_iter()
                    .filter_map(|mut row| {
                        row.terminal = terminal_out(link, row.terminal)?;
                        row.workspace = workspace_out(link, row.workspace)?;
                        Some(row)
                    })
                    .collect();
                l.stale_agents = false;
                l.agents_seen = true;
                let id = if id.starts_with("mux-") {
                    "mux".to_string()
                } else {
                    id
                };
                self.push_agents(&id).await
            }
            C::Workspaces { workspaces, .. } => {
                l.listed = true;
                l.stale_spaces = false;
                // A remote with nothing awake is given a moment, then let go: held open, its
                // daemon would never leave. Checked again when the moment is up.
                if l.remote.is_some()
                    && quiet(&workspaces)
                    && let Some((conn, _)) = &l.conn
                {
                    let conn = *conn;
                    tokio::spawn(async move {
                        tokio::time::sleep(PARK_AFTER).await;
                        let _ = events.send(FromLink::ParkCheck { link, conn }).await;
                    });
                }
                l.spaces = workspaces
                    .into_iter()
                    .filter_map(|row| {
                        Some(WorkspaceRow {
                            id: workspace_out(link, row.id)?,
                            name: space_name(&name, &row.name),
                            // The tabs of a sleeping space carry the machine's numbers,
                            // moved into this link's range like every other terminal.
                            sleeping: row
                                .sleeping
                                .into_iter()
                                .filter_map(|mut tab| {
                                    tab.terminal = terminal_out(link, tab.terminal)?;
                                    tab.lead = tab.lead.and_then(|t| terminal_out(link, t));
                                    Some(tab)
                                })
                                .collect(),
                            // Everything else is the far daemon's own answer, carried
                            // across: a remote space carries its rules with it, and
                            // `CloseWorkspace` is routed back to the link that owns it.
                            ..row
                        })
                    })
                    .collect();
                let listed: Vec<String> = l
                    .spaces
                    .iter()
                    .map(|w| format!("{name}:{}", workspace_in(w.id).1))
                    .collect();
                self.refresh_order(link, &name, &listed);
                // Its own answer to coming up, or the interface's question: either way the
                // interface gets the whole list, with anything that vanished closed first.
                let id = if id.starts_with("mux-") {
                    "mux".to_string()
                } else {
                    id
                };
                self.push_lists(&id).await
            }
            // The answer to the ask to move to the newer qoop, ahead of the first lists' arm
            // below: a refusal of it is not a list that failed. Said once, as one line.
            C::RestartArmed { waiting_on, .. } if id == RESTART_ID => {
                let line = match waiting_on {
                    Some(reason) => format!(
                        "{name} moves to the new qoop once nothing there is working; now \
                         waiting on {reason}"
                    ),
                    None => format!("{name} moves to the new qoop once nothing there is working"),
                };
                self.respond(&C::RestartArmed {
                    id: UPDATE_NOTICE.into(),
                    waiting_on: Some(line),
                })
                .await
            }
            // A daemon from before the ask existed. One too old to name the request in its
            // refusal answers with no id, but still names the ask it did not know.
            C::Failed { reason, .. }
                if id == RESTART_ID
                    || (id.is_empty() && too_old_for(&reason, Some("RestartWhenSafe"))) =>
            {
                let line = if too_old_for(&reason, None) {
                    format!(
                        "{name} runs a qoop from before updates moved on their own: the new one \
                         is on disk there and takes over when {name} next restarts its qoop"
                    )
                } else {
                    format!("{name} keeps its qoop for now: {reason}")
                };
                self.failed(UPDATE_NOTICE.into(), line).await
            }
            // Its part of a new order, taken or refused.
            C::Renamed { .. } if id.starts_with(ORDER_ID) => {
                l.order_owed = l.order_owed.saturating_sub(1);
                Ok(())
            }
            C::Failed { .. } if id.starts_with(ORDER_ID) => {
                l.order_owed = l.order_owed.saturating_sub(1);
                l.order_off = true;
                Ok(())
            }
            // Its own first lists refused, by a store that was busy for instance: the merged
            // lists stop waiting for them rather than waiting for the next change, and the
            // interface's own next question fills them in.
            C::Failed { .. } if id.starts_with("mux-") => {
                l.listed = true;
                l.agents_seen = true;
                self.push_lists("mux").await
            }
            // Its own housekeeping; the interface did not ask.
            _ if id.starts_with("mux-") => Ok(()),
            C::TerminalCreated { id, terminal } => match terminal_out(link, terminal) {
                Some(terminal) => {
                    self.touched(link);
                    self.respond(&C::TerminalCreated { id, terminal }).await
                }
                None => self.failed(id, "terminal number out of range".into()).await,
            },
            C::TerminalClosed { id, terminal } => match terminal_out(link, terminal) {
                Some(terminal) => {
                    self.touched(link);
                    self.respond(&C::TerminalClosed { id, terminal }).await
                }
                None => Ok(()),
            },
            // The splits cached from this link predate the one just recorded: merged as
            // they are, the interface would take the old tree back.
            C::SplitSaved { id } => {
                self.touched(link);
                self.respond(&C::SplitSaved { id }).await
            }
            C::WorkspaceCreated { id, workspace } => match workspace_out(link, workspace) {
                Some(workspace) => {
                    self.touched(link);
                    self.respond(&C::WorkspaceCreated { id, workspace }).await
                }
                None => self.failed(id, "space number out of range".into()).await,
            },
            C::WorkspaceClosed { id, workspace } => match workspace_out(link, workspace) {
                Some(workspace) => {
                    self.touched(link);
                    self.respond(&C::WorkspaceClosed { id, workspace }).await
                }
                None => Ok(()),
            },
            // A machine running a qoop from before an ask existed says so in its own words,
            // which name a fix for itself. Over ssh that fix is on the other machine, and a
            // newer qoop there may be on its way from here already: said as it is, no more.
            C::Failed { id, reason }
                if !name.is_empty() && reason.contains("older than the client") =>
            {
                self.failed(
                    id,
                    format!("{name} runs an older qoop that cannot do this yet"),
                )
                .await
            }
            C::WorkspaceAsleep {
                id,
                workspace,
                terminals,
                changed,
                stayed,
            } => match workspace_out(link, workspace) {
                Some(workspace) => {
                    self.touched(link);
                    let changed = changed.map(|ts| {
                        ts.into_iter()
                            .filter_map(|t| terminal_out(link, t))
                            .collect()
                    });
                    let stayed = stayed
                        .into_iter()
                        .filter_map(|s| {
                            Some(qoop_runtime::control::Stayed {
                                terminal: terminal_out(link, s.terminal)?,
                                reason: s.reason,
                            })
                        })
                        .collect();
                    self.respond(&C::WorkspaceAsleep {
                        id,
                        workspace,
                        terminals,
                        changed,
                        stayed,
                    })
                    .await
                }
                None => Ok(()),
            },
            C::WorkspaceAwake {
                id,
                workspace,
                terminals,
                resumed,
                changed,
            } => match workspace_out(link, workspace) {
                Some(workspace) => {
                    self.touched(link);
                    let changed = changed.map(|ts| {
                        ts.into_iter()
                            .filter_map(|t| terminal_out(link, t))
                            .collect()
                    });
                    self.respond(&C::WorkspaceAwake {
                        id,
                        workspace,
                        terminals,
                        resumed,
                        changed,
                    })
                    .await
                }
                None => Ok(()),
            },
            C::Scrollback {
                id,
                terminal,
                from,
                total,
                lines,
            } => match terminal_out(link, terminal) {
                Some(terminal) => {
                    self.respond(&C::Scrollback {
                        id,
                        terminal,
                        from,
                        total,
                        lines,
                    })
                    .await
                }
                None => Ok(()),
            },
            C::ScrollbackMatch {
                id,
                terminal,
                row,
                col,
                resume,
            } => match terminal_out(link, terminal) {
                Some(terminal) => {
                    self.respond(&C::ScrollbackMatch {
                        id,
                        terminal,
                        row,
                        col,
                        resume,
                    })
                    .await
                }
                None => Ok(()),
            },
            C::FocusEvents {
                id,
                terminal,
                events,
            } => match terminal_out(link, terminal) {
                Some(terminal) => {
                    self.respond(&C::FocusEvents {
                        id,
                        terminal,
                        events,
                    })
                    .await
                }
                None => Ok(()),
            },
            C::Subscribed { id, terminals } => {
                let terminals = terminals
                    .into_iter()
                    .filter_map(|t| terminal_out(link, t))
                    .collect();
                self.respond(&C::Subscribed { id, terminals }).await
            }
            C::Terminals { id, terminals } => {
                let terminals = terminals
                    .into_iter()
                    .filter_map(|t| terminal_out(link, t))
                    .collect();
                self.respond(&C::Terminals { id, terminals }).await
            }
            C::Renamed { id } => {
                self.touched(link);
                self.respond(&C::Renamed { id }).await
            }
            C::TerminalMoved {
                id,
                terminal,
                workspace,
            } => {
                self.touched(link);
                match (terminal_out(link, terminal), workspace_out(link, workspace)) {
                    (Some(terminal), Some(workspace)) => {
                        self.respond(&C::TerminalMoved {
                            id,
                            terminal,
                            workspace,
                        })
                        .await
                    }
                    _ => Ok(()),
                }
            }
            // Answers that carry no number of either kind.
            other => self.respond(&other).await,
        }
    }
}

/// The last release whose daemon knows no place for a moved tab: one this old or older
/// would take a placed move, or a pane leaving its split, as done without doing it.
const BEFORE_PLACES: &str = "0.5.2";

/// The file the merged list's order is kept in, beside the list of remotes.
const ORDER_FILE: &str = "space-order.json";

/// The id each machine is told its part of a new order under. Its answer is the mux's own:
/// a daemon from before the ask refuses it, and that changes nothing here.
const ORDER_ID: &str = "mux-order-";

/// The kept order, or none when there is no file or it does not read.
fn load_order(file: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Keep the order, written beside the file and renamed over it so a reader never sees
/// half of it.
fn save_order(file: &std::path::Path, order: &[String]) -> io::Result<()> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string(order).map_err(io::Error::other)?;
    let partial = file.with_extension(format!("json.{}", std::process::id()));
    std::fs::write(&partial, text + "\n")?;
    std::fs::rename(&partial, file)
}

/// Marks a link whose cached lists an answer just made old. See `Link::stale_agents`.
impl<W> Mux<W> {
    fn touched(&mut self, link: usize) {
        if let Some(l) = self.links.get_mut(link).and_then(Option::as_mut) {
            l.stale_agents = true;
            l.stale_spaces = true;
        }
    }
}

/// A tree in the interface's numbering as the daemon of its first pane numbers it, and
/// which daemon that is. `None` when a pane belongs to another daemon.
fn split_in(tree: &Pane) -> Option<(usize, Pane)> {
    let link = terminal_in(*tree.leaves().first()?).0;
    let tree = tree.map(&|t| {
        let (at, t) = terminal_in(t);
        (at == link).then_some(t)
    })?;
    Some((link, tree))
}

/// Interface terminal numbers grouped by the daemon they belong to, in its numbering.
fn split(terminals: &[u32]) -> BTreeMap<usize, Vec<u32>> {
    let mut by: BTreeMap<usize, Vec<u32>> = BTreeMap::new();
    for t in terminals {
        let (link, id) = terminal_in(*t);
        by.entry(link).or_default().push(id);
    }
    by
}

/// Read what the interface writes and decode it into requests and terminal commands.
///
/// Its acknowledgements are dropped: they count renders from every daemon at once, and the
/// mux acknowledges each daemon for itself.
async fn read_tui<R: AsyncRead + Unpin>(mut rd: R, tx: mpsc::UnboundedSender<FromTui>) {
    let mut control = Reassembler::new();
    let mut terminal = Reassembler::new();
    let mut inbuf = BytesMut::with_capacity(64 * 1024);
    loop {
        match rd.read_buf(&mut inbuf).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        loop {
            let frame = match Frame::decode(&mut inbuf) {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                Err(_) => return,
            };
            let target = match frame.channel {
                Channel::Control => &mut control,
                Channel::Terminal => &mut terminal,
                _ => continue,
            };
            let message = match target.push(frame, Instant::now()) {
                Ok(Some(message)) => message,
                Ok(None) => continue,
                Err(_) => return,
            };
            let decoded = match message.channel {
                Channel::Control => serde_json::from_slice(&message.body)
                    .ok()
                    .map(FromTui::Control),
                _ if message.body.is_empty() => None,
                _ => serde_json::from_slice(&message.body)
                    .ok()
                    .map(|wire| FromTui::Wire {
                        terminal: message.stream,
                        wire,
                    }),
            };
            if let Some(decoded) = decoded
                && tx.send(decoded).is_err()
            {
                return;
            }
        }
    }
}

/// Carry one connection: decode what it says into the mux, acknowledge it, and write what
/// the mux queues for it. Returns when the connection ends, for whatever reason.
async fn pump<S>(
    link: usize,
    conn: u64,
    stream: S,
    leftover: BytesMut,
    server_version: String,
    connection: Connection,
    events: &mpsc::Sender<FromLink>,
) where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (mut rd, mut wr) = tokio::io::split(stream);
    let (tx, mut rx) = mpsc::unbounded_channel::<BytesMut>();
    // The only writer to this connection: requests from the mux and this pump's own acks
    // both queue here, so two writes can never interleave inside a frame.
    let mut writer = tokio::spawn(async move {
        while let Some(bytes) = rx.recv().await {
            if wr.write_all(&bytes).await.is_err() {
                return;
            }
        }
    });
    if events
        .send(FromLink::Up {
            link,
            conn,
            writer: tx.clone(),
            server_version,
        })
        .await
        .is_err()
    {
        writer.abort();
        return;
    }
    let mut connection = connection;
    let mut inbuf = leftover;
    let mut tick = tokio::time::interval(client::ACK_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            read = rd.read_buf(&mut inbuf) => {
                if !matches!(read, Ok(n) if n > 0) {
                    break;
                }
                let Ok(said) = connection.drain(&mut inbuf) else {
                    break;
                };
                for incoming in said {
                    // Bounded: a mux that cannot hand renders to a stalled interface stops
                    // taking them from here, this stops reading, and its acknowledgements
                    // stop with it, which is what the daemon's deadline is watching for.
                    if events.send(FromLink::Said { link, conn, incoming }).await.is_err() {
                        writer.abort();
                        return;
                    }
                }
                if let Some(ack) = connection.renders().due_ack(std::time::Instant::now()) {
                    let _ = tx.send(ack);
                }
            }
            _ = tick.tick() => {
                if let Some(ack) = connection.renders().due_ack(std::time::Instant::now()) {
                    let _ = tx.send(ack);
                }
            }
            _ = &mut writer => break,
        }
    }
    writer.abort();
}

/// Whether nothing in a machine's spaces is running.
fn quiet(spaces: &[WorkspaceRow]) -> bool {
    spaces.iter().all(|space| space.terminals == 0)
}

/// Keep one remote connected for as long as the mux runs, except while it is parked.
async fn supervise(
    link: usize,
    remote: Remote,
    events: mpsc::Sender<FromLink>,
    wake: Arc<Notify>,
    parked: Arc<tokio::sync::watch::Sender<bool>>,
) {
    let mut backoff = Duration::from_secs(1);
    let mut conn = 0u64;
    let mut unparked = parked.subscribe();
    let key = link_key();
    loop {
        // Parked: nothing is tried until something is asked of this machine.
        if unparked.wait_for(|parked| !*parked).await.is_err() {
            return;
        }
        conn += 1;
        let attempted = Instant::now();
        match crate::link::open(&remote, true, &key).await {
            Ok(opened) => {
                let crate::link::Link {
                    stream,
                    leftover,
                    child,
                    said,
                    server_version,
                } = opened;
                let started = Instant::now();
                // Pumped until the connection ends or the mux lets it go; the child is
                // dropped either way, and with it the ssh.
                let mut parking = parked.subscribe();
                tokio::select! {
                    _ = pump(link, conn, stream, leftover, server_version, Connection::raw().paced().deltas(), &events) => {}
                    _ = parking.wait_for(|parked| *parked) => {}
                }
                drop(child);
                if started.elapsed() > BACKOFF_CAP {
                    backoff = Duration::from_secs(1);
                }
                let reason = last_word(&said);
                if events
                    .send(FromLink::Down { link, conn, reason })
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Err(e) => {
                if events
                    .send(FromLink::Down {
                        link,
                        conn,
                        reason: e.to_string(),
                    })
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
        // Until the backoff runs out, or until somebody asks for this machine: then at once,
        // and from the shortest wait again, since a person reaching for it now is a good
        // sign the network is back.
        // A link let go on purpose starts again only when asked, from the top of the loop.
        if *parked.borrow() {
            backoff = Duration::from_secs(1);
            continue;
        }
        tokio::select! {
            _ = tokio::time::sleep(backoff) => backoff = (backoff * 2).min(BACKOFF_CAP),
            _ = wake.notified() => {
                backoff = Duration::from_secs(1);
                // At most one attempt a second however fast the asking: every keystroke
                // into an offline space rings this, and each ring is not an ssh to start.
                tokio::time::sleep_until((attempted + backoff).into()).await;
            }
        }
    }
}

/// One link's key, kept across its reconnects: a far daemon ends the connections it made
/// before, which a dropped network leaves open there holding terminal sizes. Random, so no
/// other link, in this interface or another, ever names the same one. Empty when there is no
/// randomness to read, which a daemon takes as no key: the old connection then waits for
/// the no-ack sweep, as before keys.
fn link_key() -> String {
    qoop_runtime::paths::random_hex().unwrap_or_default()
}

fn last_word(said: &Arc<Mutex<String>>) -> String {
    said.lock().map(|s| s.clone()).unwrap_or_default()
}

/// Serve the interface's side of `down` from the local daemon at `local`, if any, and the
/// `remotes`. Returns when the interface goes away or the local daemon does.
pub async fn serve<D>(
    down: D,
    local: Option<(tokio::net::UnixStream, BytesMut)>,
    remotes: Vec<Remote>,
    only: Option<i64>,
) where
    D: AsyncRead + AsyncWrite + Send + 'static,
{
    let (down_rd, down_wr) = tokio::io::split(down);
    let (tui_tx, mut tui_rx) = mpsc::unbounded_channel();
    tokio::spawn(read_tui(down_rd, tui_tx));
    let (link_tx, mut link_rx) = mpsc::channel::<FromLink>(64);

    let mut links: Vec<Option<Link>> = vec![None];
    let has_local = local.is_some();
    HAS_LOCAL.store(has_local, std::sync::atomic::Ordering::Relaxed);
    if let Some((stream, leftover)) = local {
        links[0] = Some(Link::new(String::new(), None));
        let events = link_tx.clone();
        tokio::spawn(async move {
            pump(
                0,
                1,
                stream,
                leftover,
                String::new(),
                Connection::raw(),
                &events,
            )
            .await;
            let _ = events
                .send(FromLink::Down {
                    link: 0,
                    conn: 1,
                    reason: String::new(),
                })
                .await;
        });
    }
    // With no local daemon, the first remote is where requests naming nothing go.
    let home = if has_local { 0 } else { 1 };
    let order_file = qoop_remote::saved::path().with_file_name(ORDER_FILE);
    let mut mux = Mux {
        links,
        home,
        down: down_wr,
        down_seqs: BTreeMap::new(),
        pushed_spaces: Vec::new(),
        last_link: home,
        order: load_order(&order_file),
        order_file: Some(order_file),
        events: link_tx,
        only,
        tried: BTreeSet::new(),
    };
    for remote in remotes {
        mux.add_remote(remote);
    }
    route(&mut mux, &mut tui_rx, &mut link_rx, has_local).await;
    // However the mux ended, the interface is told: it is waiting on a read, and a stream
    // that is only abandoned rather than closed leaves it painting its last screen until
    // the next key press finds out.
    let _ = mux.down.shutdown().await;
}

async fn route<W: AsyncWrite + Unpin>(
    mux: &mut Mux<W>,
    tui_rx: &mut mpsc::UnboundedReceiver<FromTui>,
    link_rx: &mut mpsc::Receiver<FromLink>,
    has_local: bool,
) {
    // The local daemon first: the interface's opening questions, and its first resize, are
    // for this machine, and asked before it is up they would be answered by nobody.
    while has_local && mux.links[0].as_ref().is_some_and(|l| l.conn.is_none()) {
        let Some(message) = link_rx.recv().await else {
            return;
        };
        if let FromLink::Down { link: 0, .. } = message {
            return;
        }
        if mux.on_link(message).await.is_err() {
            return;
        }
    }
    // The full interface only: `--space` shows one space and `--remote` one machine, and
    // neither takes in the rest of the saved list.
    let follows_saved = has_local && mux.only.is_none();
    let mut saved_tick = tokio::time::interval(client::ACK_INTERVAL);
    saved_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let step = tokio::select! {
            _ = saved_tick.tick(), if follows_saved => mux.adopt_saved().await,
            message = tui_rx.recv() => match message {
                Some(message) => mux.on_tui(message).await,
                // The interface is gone, and so is the reason for everything else here.
                None => return,
            },
            Some(message) = link_rx.recv() => {
                // The local daemon going away is the end, as it is without a mux: the
                // interface is a client of this machine's daemon first.
                if let FromLink::Down { link: 0, .. } = message {
                    return;
                }
                mux.on_link(message).await
            }
            else => return,
        };
        if step.is_err() {
            return;
        }
    }
}

/// The interface over the local daemon at `socket` (when given) and `remotes`, showing
/// only the local space `only` when one is named. `focus` is the terminal a window that
/// came back after its daemon moved showed before (see `run::FOCUS_ENV`).
pub async fn run(
    socket: Option<&Path>,
    remotes: Vec<Remote>,
    notice: Option<String>,
    only: Option<i64>,
    focus: Option<u32>,
) -> io::Result<()> {
    let local = match socket {
        Some(socket) => Some(client::connect(socket, qoop_proto::INTERFACE_ID).await?),
        None => None,
    };
    // A megabyte each way: a full screen snapshot is tens of kilobytes, and the interface
    // reads as fast as it paints.
    let (tui_side, mux_side) = tokio::io::duplex(1 << 20);
    tokio::spawn(serve(mux_side, local, remotes, only));
    crate::run::run_stream(
        tui_side,
        BytesMut::new(),
        notice,
        socket.map(crate::run::last_focus_path),
        focus,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_numbers_pass_through_and_remote_ones_round_trip() {
        assert_eq!(terminal_out(0, 7), Some(7));
        assert_eq!(terminal_in(7), (0, 7));
        let t = terminal_out(3, 42).expect("fits");
        assert!(is_remote_terminal(t));
        assert_eq!(terminal_in(t), (3, 42));
        let t = terminal_out(MAX_REMOTES, (1 << 24) - 1).expect("fits");
        assert_eq!(terminal_in(t), (MAX_REMOTES, (1 << 24) - 1));

        assert_eq!(workspace_out(0, 5), Some(5));
        let w = workspace_out(2, 5).expect("fits");
        assert!(is_remote_workspace(w));
        assert_eq!(workspace_in(w), (2, 5));
        assert!(!is_remote_workspace(5));
    }

    /// A number that does not fit is refused, never wrapped into somebody else's range.
    #[test]
    fn numbers_outside_their_range_are_refused() {
        assert_eq!(terminal_out(0, REMOTE_TERMINAL), None);
        assert_eq!(terminal_out(1, 1 << 24), None);
        assert_eq!(workspace_out(0, 1 << 40), None);
        assert_eq!(workspace_out(1, 1 << 40), None);
        assert_eq!(workspace_out(1, -1), None);
        // Zero is the placeholder's, never a real remote space.
        assert_eq!(workspace_out(1, 0), None);
    }

    /// Local numbers sort below every remote one, so the interface's `min()` and `first()`
    /// still settle on this machine.
    #[test]
    fn local_numbers_sort_first() {
        let local = terminal_out(0, REMOTE_TERMINAL - 1).expect("fits");
        let remote = terminal_out(1, 1).expect("fits");
        assert!(local < remote);
        assert!(
            workspace_out(0, (1 << 40) - 1).expect("fits") < workspace_out(1, 1).expect("fits")
        );
    }

    #[test]
    fn a_remote_tabs_split_goes_home_in_its_own_numbering() {
        let beside = |a, b| Pane::Split {
            dir: qoop_runtime::control::SplitDir::V,
            permille: 500,
            questions: 0,
            first: Box::new(Pane::Leaf { terminal: a }),
            second: Box::new(Pane::Leaf { terminal: b }),
        };
        let (a, b) = (terminal_out(2, 4).unwrap(), terminal_out(2, 5).unwrap());
        assert_eq!(split_in(&beside(a, b)), Some((2, beside(4, 5))));
        assert_eq!(split_in(&beside(3, 7)), Some((0, beside(3, 7))));
        assert_eq!(split_in(&beside(3, a)), None, "a tab lives on one host");
        // And back out: what the remote keeps renumbers into the interface's range.
        assert_eq!(
            beside(4, 5).map(&|t| terminal_out(2, t)),
            Some(beside(a, b))
        );
    }

    #[test]
    fn a_mixed_subscription_splits_by_daemon() {
        let a = terminal_out(1, 4).expect("fits");
        let b = terminal_out(2, 4).expect("fits");
        let by = split(&[3, a, b, 9]);
        assert_eq!(by[&0], vec![3, 9]);
        assert_eq!(by[&1], vec![4]);
        assert_eq!(by[&2], vec![4]);
    }

    /// The one frame's body in `bytes`, read as `T`.
    fn body_of<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> T {
        let mut buf = BytesMut::from(bytes);
        let frame = Frame::decode(&mut buf).expect("a frame").expect("whole");
        serde_json::from_slice(&frame.body).expect("json")
    }

    /// This machine and one remote, `devbox`, both connected: the mux, what each daemon is
    /// sent, and the events receiver, held so the mux's sender stays open.
    fn two_links() -> (
        Mux<Vec<u8>>,
        mpsc::UnboundedReceiver<BytesMut>,
        mpsc::UnboundedReceiver<BytesMut>,
        mpsc::Receiver<FromLink>,
    ) {
        let (home_tx, home_rx) = mpsc::unbounded_channel();
        let (remote_tx, remote_rx) = mpsc::unbounded_channel();
        let mut home = Link::new(String::new(), None);
        home.conn = Some((1, home_tx));
        let mut remote = Link::new("devbox".into(), None);
        remote.conn = Some((1, remote_tx));
        let (events, held) = mpsc::channel(1);
        let mux = Mux {
            links: vec![Some(home), Some(remote)],
            home: 0,
            down: Vec::new(),
            down_seqs: BTreeMap::new(),
            pushed_spaces: Vec::new(),
            last_link: 0,
            order: Vec::new(),
            order_file: None,
            events,
            only: None,
            tried: BTreeSet::new(),
        };
        (mux, home_rx, remote_rx, held)
    }

    /// A history search on a remote space goes to that remote with its own number for the
    /// terminal, and the answer comes back in the interface's numbering: sent home with
    /// the encoded number, the home daemon said "there is no terminal", and an answer left
    /// in the remote's numbering was dropped as another terminal's.
    #[tokio::test]
    async fn a_history_search_reaches_the_remote_that_holds_the_terminal_and_comes_back() {
        let (mut mux, mut home_rx, mut remote_rx, _events) = two_links();
        let there = terminal_out(1, 21).expect("fits");
        mux.control(ControlRequest::SearchScrollback {
            id: "search-1".into(),
            terminal: there,
            query: "needle".into(),
            from: 5,
            backward: true,
        })
        .await
        .expect("sent");
        assert!(home_rx.try_recv().is_err(), "the home daemon was asked");
        let asked: ControlRequest = body_of(&remote_rx.try_recv().expect("the remote was asked"));
        assert!(
            matches!(asked, ControlRequest::SearchScrollback { terminal: 21, .. }),
            "{asked:?}"
        );
        mux.answer(
            1,
            ControlResponse::ScrollbackMatch {
                id: "search-1".into(),
                terminal: 21,
                row: Some(3),
                col: 4,
                resume: None,
            },
        )
        .await
        .expect("answered");
        let answered: ControlResponse = body_of(&mux.down);
        assert_eq!(
            answered,
            ControlResponse::ScrollbackMatch {
                id: "search-1".into(),
                terminal: there,
                row: Some(3),
                col: 4,
                resume: None,
            }
        );
    }

    /// The chat of a remote tab is parsed by the machine that owns its session file, so the
    /// request goes there in its own numbering, and that machine's news of a changed chat
    /// comes back in the interface's: sent home, the home daemon would read its own disk, and
    /// left in the remote's numbering the news would be taken for a local tab's.
    #[tokio::test]
    async fn the_chat_of_a_remote_tab_is_read_where_the_tab_runs_and_its_news_is_renumbered() {
        let (mut mux, mut home_rx, mut remote_rx, _events) = two_links();
        let there = terminal_out(1, 21).expect("fits");
        let page = |terminal| ControlRequest::Messages {
            id: "chat-1".into(),
            terminal,
            skip: 60,
            limit: 30,
            revision: Some(7),
        };
        mux.control(page(there)).await.expect("sent");
        mux.control(ControlRequest::MessageDetail {
            id: "chat-2".into(),
            terminal: there,
            tool_id: "toolu_1".into(),
            max_bytes: None,
        })
        .await
        .expect("sent");
        assert!(home_rx.try_recv().is_err(), "the home daemon was asked");
        let asked: ControlRequest = body_of(&remote_rx.try_recv().expect("the remote was asked"));
        assert_eq!(asked, page(21));
        let asked: ControlRequest = body_of(&remote_rx.try_recv().expect("the remote was asked"));
        assert!(
            matches!(asked, ControlRequest::MessageDetail { terminal: 21, .. }),
            "{asked:?}"
        );
        mux.said(
            1,
            Incoming::Event(EventMessage::MessagesChanged {
                terminal: 21,
                revision: 9,
            }),
        )
        .await
        .expect("passed on");
        let frame = Frame::decode(&mut BytesMut::from(mux.down.as_slice()))
            .expect("a frame")
            .expect("whole");
        let event: EventMessage = serde_json::from_slice(&frame.body).expect("json");
        assert_eq!(
            event,
            EventMessage::MessagesChanged {
                terminal: there,
                revision: 9,
            }
        );
    }

    /// A picture for a remote tab is kept by that remote's daemon, under its own number for
    /// the tab: kept at home, its path would name a file the remote agent cannot read. One
    /// that names no tab still goes home, as the phone's do.
    #[tokio::test]
    async fn a_picture_for_a_remote_tab_is_kept_where_the_tab_runs() {
        let (mut mux, mut home_rx, mut remote_rx, _events) = two_links();
        let upload = |id: &str, terminal| ControlRequest::SaveUpload {
            id: id.into(),
            ext: "jpg".into(),
            data: "aGk=".into(),
            terminal,
        };
        mux.control(upload("up-1", Some(terminal_out(1, 21).expect("fits"))))
            .await
            .expect("sent");
        assert!(home_rx.try_recv().is_err(), "the home daemon was asked");
        let asked: ControlRequest = body_of(&remote_rx.try_recv().expect("the remote was asked"));
        assert_eq!(asked, upload("up-1", Some(21)));
        mux.control(upload("up-2", None)).await.expect("sent");
        assert!(remote_rx.try_recv().is_err(), "the remote was asked");
        let asked: ControlRequest = body_of(&home_rx.try_recv().expect("home was asked"));
        assert_eq!(asked, upload("up-2", None));
    }

    /// A remote's recording names its terminal in the interface's numbering: left as the
    /// remote's own number it would name a local tab, and lend the microphone there.
    #[tokio::test]
    async fn a_remote_recording_names_its_tab_in_the_interfaces_numbering() {
        let (mut mux, _home_rx, _remote_rx, _events) = two_links();
        let listen = |terminal| {
            Incoming::Event(EventMessage::Listen {
                terminal,
                listener: 5,
                on: true,
            })
        };
        mux.said(1, listen(3)).await.expect("passed on");
        let frame = Frame::decode(&mut BytesMut::from(mux.down.as_slice()))
            .expect("a frame")
            .expect("whole");
        let event: EventMessage = serde_json::from_slice(&frame.body).expect("json");
        assert_eq!(
            event,
            EventMessage::Listen {
                terminal: terminal_out(1, 3).expect("fits"),
                listener: 5,
                on: true,
            }
        );
    }

    /// A colour picked for a remote space goes to that remote in its own numbering: sent
    /// home with the encoded number, the home daemon would say there is no such space.
    #[tokio::test]
    async fn a_remote_spaces_colour_goes_to_its_machine_in_its_own_numbering() {
        let (mut mux, mut home_rx, mut remote_rx, _events) = two_links();
        mux.control(ControlRequest::SetWorkspaceInk {
            id: "ink-1".into(),
            workspace: workspace_out(1, 3).expect("fits"),
            ink: Some(4),
        })
        .await
        .expect("sent");
        assert!(home_rx.try_recv().is_err(), "the home daemon was asked");
        let asked: ControlRequest = body_of(&remote_rx.try_recv().expect("the remote was asked"));
        assert_eq!(
            asked,
            ControlRequest::SetWorkspaceInk {
                id: "ink-1".into(),
                workspace: 3,
                ink: Some(4),
            }
        );
    }

    /// A tab moved on a remote goes to that machine in its own numbering, the tab it goes
    /// before too, and its answer comes back in the interface's; a move that would cross
    /// machines is refused here, never sent to a daemon that cannot know the other's tabs.
    #[tokio::test]
    async fn a_remote_tab_moves_on_its_machine_and_never_between_machines() {
        use qoop_runtime::control::Place;
        let (mut mux, mut home_rx, mut remote_rx, _events) = two_links();
        let tab = terminal_out(1, 7).expect("fits");
        let space = workspace_out(1, 3).expect("fits");
        // A remote on a release from before places is told no here, never asked.
        mux.link(1).expect("remote").server_version = BEFORE_PLACES.into();
        mux.control(ControlRequest::MoveTerminal {
            id: "move-0".into(),
            terminal: tab,
            workspace: space,
            place: Some(Place::End),
            leave_split: false,
        })
        .await
        .expect("answered");
        assert!(remote_rx.try_recv().is_err(), "an old remote was asked");
        let said = String::from_utf8_lossy(&mux.down).to_string();
        assert!(said.contains("older qoop"), "{said}");
        mux.down.clear();
        mux.link(1).expect("remote").server_version = "0.6.0".into();
        mux.control(ControlRequest::MoveTerminal {
            id: "move-1".into(),
            terminal: tab,
            workspace: space,
            place: Some(Place::Before(terminal_out(1, 5).expect("fits"))),
            leave_split: false,
        })
        .await
        .expect("sent");
        assert!(home_rx.try_recv().is_err(), "the home daemon was asked");
        let asked: ControlRequest = body_of(&remote_rx.try_recv().expect("the remote was asked"));
        assert_eq!(
            asked,
            ControlRequest::MoveTerminal {
                id: "move-1".into(),
                terminal: 7,
                workspace: 3,
                place: Some(Place::Before(5)),
                leave_split: false,
            }
        );
        mux.answer(
            1,
            ControlResponse::TerminalMoved {
                id: "move-1".into(),
                terminal: 7,
                workspace: 3,
            },
        )
        .await
        .expect("answered");
        let said = String::from_utf8_lossy(&mux.down).to_string();
        assert!(said.contains(&format!("\"terminal\":{tab}")), "{said}");
        assert!(said.contains(&format!("\"workspace\":{space}")), "{said}");
        // Into a space of this machine, or before a tab of it: refused, nothing sent.
        for (workspace, place) in [(1, None), (space, Some(Place::Before(2)))] {
            mux.down.clear();
            mux.control(ControlRequest::MoveTerminal {
                id: "move-2".into(),
                terminal: tab,
                workspace,
                place,
                leave_split: false,
            })
            .await
            .expect("answered");
            assert!(home_rx.try_recv().is_err() && remote_rx.try_recv().is_err());
            let said = String::from_utf8_lossy(&mux.down).to_string();
            assert!(said.contains("cannot move between machines"), "{said}");
        }
    }

    /// Spaces move anywhere in the merged list, across machines: the order is kept by the
    /// mux, answered at once, and each machine is told its own part of it in its numbering.
    #[tokio::test]
    async fn spaces_move_anywhere_and_each_machine_keeps_its_part() {
        let (mut mux, mut home_rx, mut remote_rx, _events) = two_links();
        let row = |id: i64, name: &str| WorkspaceRow {
            keep_awake: false,
            qass_skip: false,
            id,
            name: name.into(),
            terminals: 1,
            asleep: false,
            sleeping: Vec::new(),
            closable: true,
            ink: None,
            link: None,
            goal: None,
        };
        let far = workspace_out(1, 3).expect("fits");
        for (link, rows) in [
            (0, vec![row(1, "qoop"), row(2, "app")]),
            (1, vec![row(far, "devbox:api")]),
        ] {
            let l = mux.link(link).expect("link");
            l.listed = true;
            l.spaces = rows;
        }
        let ids =
            |mux: &Mux<Vec<u8>>| -> Vec<i64> { mux.merged_spaces().iter().map(|w| w.id).collect() };
        assert_eq!(ids(&mux), vec![1, 2, far], "never placed: by machine");
        // The remote's space to the top.
        mux.control(ControlRequest::MoveWorkspace {
            id: "order-1".into(),
            workspace: far,
            before: Some(1),
        })
        .await
        .expect("answered");
        assert_eq!(ids(&mux), vec![far, 1, 2]);
        let said = String::from_utf8_lossy(&mux.down).to_string();
        assert!(
            said.contains("\"Renamed\"") && said.contains("order-1"),
            "{said}"
        );
        let told: ControlRequest = body_of(&remote_rx.try_recv().expect("the remote was told"));
        assert_eq!(
            told,
            ControlRequest::MoveWorkspace {
                id: "mux-order-order-1".into(),
                workspace: 3,
                before: None,
            }
        );
        assert!(home_rx.try_recv().is_err(), "nothing changed at home");
        // A local space below the remote's: home keeps its own two in the new order.
        mux.control(ControlRequest::MoveWorkspace {
            id: "order-2".into(),
            workspace: 2,
            before: Some(1),
        })
        .await
        .expect("answered");
        assert_eq!(ids(&mux), vec![far, 2, 1]);
        let told: ControlRequest = body_of(&home_rx.try_recv().expect("home was told"));
        assert_eq!(
            told,
            ControlRequest::MoveWorkspace {
                id: "mux-order-order-2".into(),
                workspace: 2,
                before: Some(1),
            }
        );
        // Home lists its own in that order now; a new space there lands after the one it
        // lists before it, not at the top.
        mux.link(0).expect("home").spaces = vec![row(2, "app"), row(1, "qoop"), row(5, "new")];
        assert_eq!(ids(&mux), vec![far, 2, 1, 5]);
    }

    /// The kept order survives the mux, folds each machine's own order in when it lists,
    /// forgets a closed space's place, and keeps an unlisted machine where its spaces were.
    #[tokio::test]
    async fn the_kept_order_follows_each_machines_own_list() {
        let dir = tempfile::tempdir().expect("dir");
        let file = dir.path().join(ORDER_FILE);
        let (mut mux, _home_rx, _remote_rx, _events) = two_links();
        mux.order_file = Some(file.clone());
        let row = |id: i64, name: &str| WorkspaceRow {
            keep_awake: false,
            qass_skip: false,
            id,
            name: name.into(),
            terminals: 1,
            asleep: false,
            sleeping: Vec::new(),
            closable: true,
            ink: None,
            link: None,
            goal: None,
        };
        let far = workspace_out(1, 3).expect("fits");
        let set = |mux: &mut Mux<Vec<u8>>, link: usize, rows: Vec<WorkspaceRow>| {
            let l = mux.link(link).expect("link");
            l.listed = true;
            l.spaces = rows;
        };
        set(&mut mux, 0, vec![row(1, "a"), row(2, "b")]);
        set(&mut mux, 1, vec![row(far, "devbox:api")]);
        let ids =
            |mux: &Mux<Vec<u8>>| -> Vec<i64> { mux.merged_spaces().iter().map(|w| w.id).collect() };
        // The remote's space between the two local ones, kept on disk.
        mux.control(ControlRequest::MoveWorkspace {
            id: "o".into(),
            workspace: far,
            before: Some(2),
        })
        .await
        .expect("answered");
        assert_eq!(ids(&mux), vec![1, far, 2]);
        assert_eq!(load_order(&file), vec![":1", "devbox:3", ":2"]);
        // Home reorders its own (the phone, another window): its two swap places around the
        // remote's, which stays between them.
        mux.refresh_order(0, "", &[":2".into(), ":1".into()]);
        set(&mut mux, 0, vec![row(2, "b"), row(1, "a")]);
        assert_eq!(ids(&mux), vec![2, far, 1]);
        // A space home no longer lists loses its place.
        mux.refresh_order(0, "", &[":2".into()]);
        assert_eq!(load_order(&file), vec![":2", "devbox:3"]);
        // A remote not listed yet stands where its spaces were put, after `2`; the space whose
        // place was forgotten follows the one its machine lists before it.
        set(&mut mux, 0, vec![row(2, "b"), row(1, "a")]);
        let placeholder = 1i64 << WORKSPACE_SHIFT;
        set(&mut mux, 1, vec![row(placeholder, "devbox")]);
        mux.order = load_order(&file);
        assert_eq!(ids(&mux), vec![2, 1, placeholder]);
        mux.order = vec!["devbox:3".into(), ":2".into()];
        assert_eq!(
            ids(&mux),
            vec![placeholder, 2, 1],
            "it keeps its machine's place"
        );
    }

    /// A machine's closed space gives up only its own place, wherever it sat; and a machine
    /// that refused its part of a new order (a qoop from before the ask) does not get to put
    /// its old order back over the one a person dragged.
    #[tokio::test]
    async fn a_closed_space_and_a_refused_order_keep_every_other_place() {
        let dir = tempfile::tempdir().expect("dir");
        let file = dir.path().join(ORDER_FILE);
        let (mut mux, _home_rx, _remote_rx, _events) = two_links();
        mux.order_file = Some(file.clone());
        save_order(
            &file,
            &[
                ":1".into(),
                "devbox:3".into(),
                ":2".into(),
                "devbox:4".into(),
            ],
        )
        .expect("saved");
        // `:1`, in the first slot, closes: the remote's space keeps the top.
        mux.refresh_order(0, "", &[":2".into()]);
        assert_eq!(load_order(&file), vec!["devbox:3", ":2", "devbox:4"]);
        // The remote refused its part: its list, still in its old order, changes nothing.
        mux.link(1).expect("remote").order_off = true;
        mux.refresh_order(1, "devbox", &["devbox:4".into(), "devbox:3".into()]);
        assert_eq!(load_order(&file), vec!["devbox:3", ":2", "devbox:4"]);
        // Once it takes one, its own order is its again.
        mux.link(1).expect("remote").order_off = false;
        mux.refresh_order(1, "devbox", &["devbox:4".into(), "devbox:3".into()]);
        assert_eq!(load_order(&file), vec!["devbox:4", ":2", "devbox:3"]);
    }

    /// A machine that missed or refused its part of an order is sent its whole order on the
    /// next move, each of its spaces to the end in turn, rather than one move on top of an
    /// order it never took.
    #[tokio::test]
    async fn a_machine_out_of_step_is_sent_its_whole_order() {
        let (mut mux, _home_rx, mut remote_rx, _events) = two_links();
        let row = |id: i64, name: &str| WorkspaceRow {
            keep_awake: false,
            qass_skip: false,
            id,
            name: name.into(),
            terminals: 1,
            asleep: false,
            sleeping: Vec::new(),
            closable: true,
            ink: None,
            link: None,
            goal: None,
        };
        let (r1, r2, r3) = (
            workspace_out(1, 1).expect("fits"),
            workspace_out(1, 2).expect("fits"),
            workspace_out(1, 3).expect("fits"),
        );
        let l = mux.link(1).expect("remote");
        l.listed = true;
        l.spaces = vec![
            row(r1, "devbox:a"),
            row(r2, "devbox:b"),
            row(r3, "devbox:c"),
        ];
        l.order_off = true;
        mux.control(ControlRequest::MoveWorkspace {
            id: "o".into(),
            workspace: r3,
            before: Some(r1),
        })
        .await
        .expect("answered");
        let mut told = Vec::new();
        while let Ok(bytes) = remote_rx.try_recv() {
            let ask: ControlRequest = body_of(&bytes);
            if let ControlRequest::MoveWorkspace {
                workspace, before, ..
            } = ask
            {
                assert_eq!(before, None, "each to the end, in turn");
                told.push(workspace);
            }
        }
        assert_eq!(told, vec![3, 1, 2]);
        let l = mux.link(1).expect("remote");
        assert!(!l.order_off && l.order_owed == 3);
    }

    /// A new tab carries a directory a tab of its machine is in, whichever machine was typed
    /// into last; any other directory only goes to the machine last typed into.
    #[tokio::test]
    async fn a_new_tab_naming_its_space_keeps_its_directory() {
        let (mut mux, mut home_rx, _remote_rx, _events) = two_links();
        mux.last_link = 1;
        let dir = "/Users/me/coding/qoop".to_string();
        let mut tab: AgentRow = serde_json::from_value(serde_json::json!({
            "terminal": 4, "workspace": 1, "agent": true, "state": "idle",
            "readiness": "ready", "cwd": dir,
        }))
        .expect("a row");
        tab.cwd = Some(dir.clone());
        mux.link(0).expect("home").agents = vec![tab];
        for (cwd, kept) in [(dir.clone(), true), ("/somewhere/else".to_string(), false)] {
            mux.control(ControlRequest::CreateTerminal {
                id: "new-1".into(),
                cwd: Some(cwd.clone()),
                workspace: Some(1),
                name: None,
            })
            .await
            .expect("sent");
            let asked: ControlRequest = body_of(&home_rx.try_recv().expect("home was asked"));
            let ControlRequest::CreateTerminal { cwd: sent, .. } = asked else {
                panic!("{asked:?}");
            };
            assert_eq!(sent.is_some(), kept, "{cwd}");
        }
    }

    /// A cup filled on a remote space or one of its panes reaches that machine's idle sleep,
    /// in its own numbering: sent home, the home daemon would keep the wrong thing awake.
    #[tokio::test]
    async fn a_remote_cup_goes_to_its_machine_in_its_own_numbering() {
        let (mut mux, mut home_rx, mut remote_rx, _events) = two_links();
        mux.control(ControlRequest::SetKeepAwake {
            id: "cup-1".into(),
            workspace: workspace_out(1, 3).expect("fits"),
            terminal: Some(terminal_out(1, 7).expect("fits")),
            on: true,
        })
        .await
        .expect("sent");
        assert!(home_rx.try_recv().is_err(), "the home daemon was asked");
        let asked: ControlRequest = body_of(&remote_rx.try_recv().expect("the remote was asked"));
        assert_eq!(
            asked,
            ControlRequest::SetKeepAwake {
                id: "cup-1".into(),
                workspace: 3,
                terminal: Some(7),
                on: true,
            }
        );
    }

    /// A qass skip on a remote space or one of its panes reaches that machine's watcher, in its
    /// own numbering: sent home, the home daemon would hide whatever its own tab of that
    /// number is.
    #[tokio::test]
    async fn a_remote_qass_skip_goes_to_its_machine_in_its_own_numbering() {
        let (mut mux, mut home_rx, mut remote_rx, _events) = two_links();
        mux.control(ControlRequest::SetQassSkip {
            id: "skip-1".into(),
            workspace: workspace_out(1, 3).expect("fits"),
            terminal: Some(terminal_out(1, 7).expect("fits")),
            on: true,
        })
        .await
        .expect("sent");
        assert!(home_rx.try_recv().is_err(), "the home daemon was asked");
        let asked: ControlRequest = body_of(&remote_rx.try_recv().expect("the remote was asked"));
        assert_eq!(
            asked,
            ControlRequest::SetQassSkip {
                id: "skip-1".into(),
                workspace: 3,
                terminal: Some(7),
                on: true,
            }
        );
    }

    /// A park of an agent on a remote reaches that machine, in its own numbering: sent home,
    /// the home daemon would park whatever its own terminal of that number is.
    #[tokio::test]
    async fn a_remote_park_goes_to_its_machine_in_its_own_numbering() {
        let (mut mux, mut home_rx, mut remote_rx, _events) = two_links();
        mux.control(ControlRequest::SetParked {
            id: "park-1".into(),
            terminal: terminal_out(1, 7).expect("fits"),
            on: true,
        })
        .await
        .expect("sent");
        assert!(home_rx.try_recv().is_err(), "the home daemon was asked");
        let asked: ControlRequest = body_of(&remote_rx.try_recv().expect("the remote was asked"));
        assert_eq!(
            asked,
            ControlRequest::SetParked {
                id: "park-1".into(),
                terminal: 7,
                on: true,
            }
        );
    }

    /// A conversation on a remote is named on that remote, in its own numbering: sent home,
    /// the home daemon renamed whatever its own terminal of that number was, or nothing.
    #[tokio::test]
    async fn a_remote_sessions_name_goes_to_its_machine_in_its_own_numbering() {
        let (mut mux, mut home_rx, mut remote_rx, _events) = two_links();
        mux.control(ControlRequest::RenameSession {
            id: "title-1".into(),
            terminal: terminal_out(1, 5).expect("fits"),
            title: Some("api".into()),
        })
        .await
        .expect("sent");
        assert!(home_rx.try_recv().is_err(), "the home daemon was asked");
        let asked: ControlRequest = body_of(&remote_rx.try_recv().expect("the remote was asked"));
        assert_eq!(
            asked,
            ControlRequest::RenameSession {
                id: "title-1".into(),
                terminal: 5,
                title: Some("api".into()),
            }
        );
    }

    /// A remote's rows say how its machine is reached, in every state the link can be in,
    /// and this machine's rows say nothing.
    #[test]
    fn a_remote_space_says_how_its_machine_is_reached() {
        let links = |link: &Link| -> Vec<Option<LinkState>> {
            link.listed_spaces(1).iter().map(|row| row.link).collect()
        };
        let row = WorkspaceRow {
            keep_awake: false,
            qass_skip: false,
            id: 1 << WORKSPACE_SHIFT,
            name: "devbox:api".into(),
            terminals: 0,
            asleep: false,
            sleeping: Vec::new(),
            closable: true,
            ink: None,
            link: None,
            goal: None,
        };
        let mut remote = Link::new("devbox".into(), None);
        assert_eq!(links(&remote), [Some(LinkState::Down)], "never listed");
        remote.listed = true;
        remote.spaces = vec![row.clone()];
        assert_eq!(
            links(&remote),
            [Some(LinkState::Down)],
            "listed, not connected"
        );
        remote.conn = Some((1, mpsc::unbounded_channel().0));
        assert_eq!(links(&remote), [Some(LinkState::Up)]);
        remote.parked.send_replace(true);
        assert_eq!(links(&remote), [Some(LinkState::Parked)]);
        let mut home = Link::new(String::new(), None);
        home.listed = true;
        home.spaces = vec![row];
        home.conn = Some((1, mpsc::unbounded_channel().0));
        assert_eq!(links(&home), [None]);
    }

    /// A remote's own default space, the lowest id, is not drawn while it is empty and the
    /// machine has another space. It stays when it is used, when it is the only space, and
    /// for this machine's own link; an older daemon's rows (none `closable`) lose only the
    /// lowest id, never a person's own empty space.
    #[test]
    fn a_remote_hides_its_empty_default_but_nothing_a_person_made() {
        let row = |n: i64, terminals: usize| WorkspaceRow {
            keep_awake: false,
            qass_skip: false,
            id: n << WORKSPACE_SHIFT,
            name: if n == 1 {
                "default".into()
            } else {
                format!("s{n}")
            },
            terminals,
            asleep: false,
            sleeping: Vec::new(),
            closable: false,
            ink: None,
            link: None,
            goal: None,
        };
        let ids = |name: &str, spaces: Vec<WorkspaceRow>| -> Vec<i64> {
            let mut link = Link::new(name.into(), None);
            link.listed = true;
            link.conn = Some((1, mpsc::unbounded_channel().0));
            link.spaces = spaces;
            link.listed_spaces(1)
                .iter()
                .map(|r| r.id >> WORKSPACE_SHIFT)
                .collect()
        };
        let empty_default_and_two = || vec![row(1, 0), row(2, 0), row(3, 2)];
        assert_eq!(ids("devbox", empty_default_and_two()), [2, 3]);
        assert_eq!(ids("", empty_default_and_two()), [1, 2, 3], "this machine");
        assert_eq!(ids("devbox", vec![row(1, 0)]), [1], "alone");
        assert_eq!(ids("devbox", vec![row(1, 1), row(2, 1)]), [1, 2], "used");
        let mut used = empty_default_and_two();
        used[0].asleep = true;
        assert_eq!(ids("devbox", used.clone()), [1, 2, 3], "asleep");
        used[0].asleep = false;
        used[0].goal = Some("g".into());
        assert_eq!(ids("devbox", used.clone()), [1, 2, 3], "goal");
        used[0].goal = None;
        used[0].keep_awake = true;
        assert_eq!(ids("devbox", used.clone()), [1, 2, 3], "kept awake");
        used[0].keep_awake = false;
        used[0].qass_skip = true;
        assert_eq!(ids("devbox", used.clone()), [1, 2, 3], "skipped by qass");
        used[0].qass_skip = false;
        used[0].ink = Some(2);
        assert_eq!(ids("devbox", used.clone()), [1, 2, 3], "coloured");
        used[0].ink = None;
        used[0].name = "main".into();
        assert_eq!(ids("devbox", used), [1, 2, 3], "renamed");
        // Not in id order on the wire: the default is still the lowest id.
        assert_eq!(ids("devbox", vec![row(3, 2), row(1, 0)]), [3]);
        // A link let go, or one that is down, draws the same rows.
        let mut link = Link::new("devbox".into(), None);
        link.listed = true;
        link.spaces = empty_default_and_two();
        assert_eq!(link.listed_spaces(1).len(), 2, "down");
        link.parked.send_replace(true);
        assert_eq!(link.listed_spaces(1).len(), 2, "parked");
    }

    /// The ask to move to a newer qoop: kept while its link is down and sent when it is up
    /// again to a daemon still older than it. A pre-feature daemon's refusal of it is one
    /// actionable line, not a first list that failed, and a waiting reason is worded once.
    #[tokio::test]
    async fn the_ask_to_move_waits_for_its_link_and_its_answers_are_one_line() {
        let remote = Link::new("devbox".into(), None);
        let (events, _events) = mpsc::channel(1);
        let mut mux = Mux {
            links: vec![None, Some(remote)],
            home: 1,
            down: Vec::new(),
            down_seqs: BTreeMap::new(),
            pushed_spaces: Vec::new(),
            last_link: 1,
            order: Vec::new(),
            order_file: None,
            events,
            only: None,
            tried: BTreeSet::new(),
        };
        mux.on_link(FromLink::Arm {
            link: 1,
            version: "9.0.0".into(),
        })
        .await
        .expect("kept");
        let (tx, mut rx) = mpsc::unbounded_channel();
        mux.on_link(FromLink::Up {
            link: 1,
            conn: 1,
            writer: tx,
            server_version: "8.0.0".into(),
        })
        .await
        .expect("up");
        let mut asked = Vec::new();
        while let Ok(bytes) = rx.try_recv() {
            asked.push(body_of::<ControlRequest>(&bytes));
        }
        assert!(
            asked.contains(&ControlRequest::RestartWhenSafe {
                id: RESTART_ID.into(),
                version: "9.0.0".into(),
                keep_tabs: true,
                quiet: true,
                same_version: false,
            }),
            "{asked:?}"
        );

        mux.answer(
            1,
            ControlResponse::Failed {
                id: RESTART_ID.into(),
                reason: "this daemon is older than the client that called it: unknown variant \
                         `RestartWhenSafe`"
                    .into(),
            },
        )
        .await
        .expect("answered");
        let said: ControlResponse = body_of(&std::mem::take(&mut mux.down));
        assert!(
            matches!(&said, ControlResponse::Failed { id, reason }
                if id == UPDATE_NOTICE && reason.contains("next restarts its qoop")),
            "{said:?}"
        );
        assert!(
            !mux.links[1].as_ref().expect("link").listed,
            "the refusal was read as a failed first list"
        );

        mux.answer(
            1,
            ControlResponse::RestartArmed {
                id: RESTART_ID.into(),
                waiting_on: Some("tab 3: sleep is running in it".into()),
            },
        )
        .await
        .expect("answered");
        let said: ControlResponse = body_of(&mux.down);
        assert!(
            matches!(&said, ControlResponse::RestartArmed { waiting_on: Some(line), .. }
                if line.starts_with("devbox ") && line.contains("tab 3")),
            "{said:?}"
        );
    }

    /// Two windows, one remote on 0.9.1. A moves it to 0.9.2 while B's push is under way;
    /// B's swap finds 0.9.2 already there and comes back with that, by which time B's link
    /// is up again to the daemon A moved. B asks nothing: asked, the daemon would end and
    /// resume every tab a second time. Nor is a version that does not parse ever asked for.
    #[tokio::test]
    async fn a_daemon_already_on_the_version_is_not_asked_to_move_to_it() {
        let (events, _events) = mpsc::channel(1);
        let mut mux = Mux {
            links: vec![None, Some(Link::new("devbox".into(), None))],
            home: 1,
            down: Vec::new(),
            down_seqs: BTreeMap::new(),
            pushed_spaces: Vec::new(),
            last_link: 1,
            order: Vec::new(),
            order_file: None,
            events,
            only: None,
            tried: BTreeSet::new(),
        };
        let restarts = |rx: &mut mpsc::UnboundedReceiver<BytesMut>| {
            let mut asked = 0;
            while let Ok(bytes) = rx.try_recv() {
                if let ControlRequest::RestartWhenSafe { .. } = body_of::<ControlRequest>(&bytes) {
                    asked += 1;
                }
            }
            asked
        };
        let up = |conn, writer, version: &str| FromLink::Up {
            link: 1,
            conn,
            writer,
            server_version: version.into(),
        };
        let (tx, _rx) = mpsc::unbounded_channel();
        mux.on_link(up(1, tx, "0.9.1")).await.expect("up");
        mux.on_link(FromLink::Down {
            link: 1,
            conn: 1,
            reason: String::new(),
        })
        .await
        .expect("down");
        let (tx2, mut rx2) = mpsc::unbounded_channel();
        mux.on_link(up(2, tx2, "0.9.2")).await.expect("up again");
        // The move is news: the line that said it would come is replaced.
        let mut down = BytesMut::from(&std::mem::take(&mut mux.down)[..]);
        let mut said = Vec::new();
        while let Ok(Some(frame)) = Frame::decode(&mut down) {
            if let Ok(response) = serde_json::from_slice::<ControlResponse>(&frame.body) {
                said.push(response);
            }
        }
        assert!(
            said.contains(&ControlResponse::RestartArmed {
                id: UPDATE_NOTICE.into(),
                waiting_on: Some("devbox now runs qoop 0.9.2".into()),
            }),
            "{said:?}"
        );
        for version in ["0.9.2", "not a version"] {
            mux.on_link(FromLink::Arm {
                link: 1,
                version: version.into(),
            })
            .await
            .expect("arm");
        }
        assert_eq!(
            restarts(&mut rx2),
            0,
            "a daemon on 0.9.2 was asked to move to it"
        );
        assert_eq!(mux.links[1].as_ref().expect("link").arm, None);

        // A connection whose pump is gone, the mux not yet told: kept for the next one.
        let (dead, gone) = mpsc::unbounded_channel();
        drop(gone);
        mux.links[1].as_mut().expect("link").conn = Some((3, dead));
        mux.on_link(FromLink::Arm {
            link: 1,
            version: "0.9.3".into(),
        })
        .await
        .expect("arm");
        assert_eq!(
            mux.links[1].as_ref().expect("link").arm.as_deref(),
            Some("0.9.3")
        );
        mux.links[1].as_mut().expect("link").arm = None;
        let (tx3, mut rx3) = mpsc::unbounded_channel();
        mux.links[1].as_mut().expect("link").conn = Some((2, tx3));

        // A newer one than it runs still is.
        mux.on_link(FromLink::Arm {
            link: 1,
            version: "0.9.3".into(),
        })
        .await
        .expect("arm");
        assert_eq!(restarts(&mut rx3), 1);
    }

    /// A daemon from before the promises of an unattended move ignores them and would move
    /// the old way, ending tabs and closing windows: the Mac swaps the file there and says
    /// so, and asks it nothing, whether the link was up when the swap finished or came up
    /// after it.
    #[tokio::test]
    async fn a_daemon_that_predates_the_promises_is_never_asked_to_move_by_itself() {
        let (events, _events) = mpsc::channel(1);
        let mut mux = Mux {
            links: vec![None, Some(Link::new("devbox".into(), None))],
            home: 1,
            down: Vec::new(),
            down_seqs: BTreeMap::new(),
            pushed_spaces: Vec::new(),
            last_link: 1,
            order: Vec::new(),
            order_file: None,
            events,
            only: None,
            tried: BTreeSet::new(),
        };
        let restarts = |rx: &mut mpsc::UnboundedReceiver<BytesMut>| {
            let mut asked = 0;
            while let Ok(bytes) = rx.try_recv() {
                if let ControlRequest::RestartWhenSafe { .. } = body_of::<ControlRequest>(&bytes) {
                    asked += 1;
                }
            }
            asked
        };
        let up = |conn, writer, version: &str| FromLink::Up {
            link: 1,
            conn,
            writer,
            server_version: version.into(),
        };
        // Up, then the swap finishes: the push is dropped, and the person is told.
        let (tx, mut rx) = mpsc::unbounded_channel();
        mux.on_link(up(1, tx, "0.9.1")).await.expect("up");
        mux.down.clear();
        mux.on_link(FromLink::Arm {
            link: 1,
            version: "0.9.3".into(),
        })
        .await
        .expect("arm");
        assert_eq!(restarts(&mut rx), 0, "a 0.9.1 daemon was asked to move");
        assert_eq!(mux.links[1].as_ref().expect("link").arm, None);
        let said = String::from_utf8_lossy(&mux.down).to_string();
        assert!(
            said.contains("moves onto the new one when you run"),
            "{said}"
        );

        // The swap finished while it was down, and it comes back as 0.9.1.
        mux.on_link(FromLink::Down {
            link: 1,
            conn: 1,
            reason: String::new(),
        })
        .await
        .expect("down");
        mux.on_link(FromLink::Arm {
            link: 1,
            version: "0.9.3".into(),
        })
        .await
        .expect("arm");
        assert_eq!(
            mux.links[1].as_ref().expect("link").arm.as_deref(),
            Some("0.9.3")
        );
        mux.down.clear();
        let (tx2, mut rx2) = mpsc::unbounded_channel();
        mux.on_link(up(2, tx2, "0.9.1")).await.expect("up again");
        assert_eq!(restarts(&mut rx2), 0, "a 0.9.1 daemon was asked to move");
        assert_eq!(mux.links[1].as_ref().expect("link").arm, None);
        let said = String::from_utf8_lossy(&mux.down).to_string();
        assert!(
            said.contains("moves onto the new one when you run"),
            "{said}"
        );

        // One that keeps them is asked, with both.
        mux.on_link(FromLink::Down {
            link: 1,
            conn: 2,
            reason: String::new(),
        })
        .await
        .expect("down");
        let (tx3, mut rx3) = mpsc::unbounded_channel();
        mux.on_link(up(3, tx3, "0.9.2")).await.expect("up");
        mux.on_link(FromLink::Arm {
            link: 1,
            version: "0.9.3".into(),
        })
        .await
        .expect("arm");
        assert_eq!(restarts(&mut rx3), 1);
    }

    /// Whether the daemon could move onto a newer qoop is asked of this machine's own daemon. A
    /// window for a remote machine alone (`qoop --remote`) cannot start the new qoop in its place,
    /// so it is told there is nothing, and the remote is not asked.
    #[tokio::test]
    async fn only_a_window_with_a_local_daemon_is_offered_the_restart() {
        let (events, _events) = mpsc::channel(1);
        let mut mux = Mux {
            links: vec![
                Some(Link::new(String::new(), None)),
                Some(Link::new("devbox".into(), None)),
            ],
            home: 0,
            down: Vec::new(),
            down_seqs: BTreeMap::new(),
            pushed_spaces: Vec::new(),
            last_link: 0,
            order: Vec::new(),
            order_file: None,
            events,
            only: None,
            tried: BTreeSet::new(),
        };
        let (local_tx, mut local_rx) = mpsc::unbounded_channel();
        let (remote_tx, mut remote_rx) = mpsc::unbounded_channel();
        mux.links[0].as_mut().expect("local").conn = Some((1, local_tx));
        mux.links[1].as_mut().expect("remote").conn = Some((1, remote_tx));
        let ask = ControlRequest::UpdateState { id: "u".into() };
        let asked = |rx: &mut mpsc::UnboundedReceiver<BytesMut>| {
            let mut n = 0;
            while let Ok(bytes) = rx.try_recv() {
                if matches!(
                    body_of::<ControlRequest>(&bytes),
                    ControlRequest::UpdateState { .. }
                ) {
                    n += 1;
                }
            }
            n
        };
        // With a local daemon it goes there, and only there.
        mux.control(ask.clone()).await.expect("ask");
        assert_eq!((asked(&mut local_rx), asked(&mut remote_rx)), (1, 0));
        assert!(mux.down.is_empty());

        // `qoop --remote`: answered here with nothing, and nobody is asked.
        mux.home = 1;
        mux.control(ask).await.expect("ask");
        assert_eq!((asked(&mut local_rx), asked(&mut remote_rx)), (0, 0));
        let said: ControlResponse = body_of(&mux.down);
        assert_eq!(
            said,
            ControlResponse::UpdateState {
                id: "u".into(),
                ready: None
            }
        );
    }

    /// `qoop --space`: the interface is given the one space and its tabs, and a tab asked
    /// for with no space named is made in that space rather than the daemon's default.
    #[tokio::test]
    async fn a_scoped_mux_passes_one_space_and_its_tabs() {
        let (home_tx, mut home_rx) = mpsc::unbounded_channel();
        let mut home = Link::new(String::new(), None);
        home.conn = Some((1, home_tx));
        let (events, _events) = mpsc::channel(1);
        let mut mux = Mux {
            links: vec![Some(home)],
            home: 0,
            down: Vec::new(),
            down_seqs: BTreeMap::new(),
            pushed_spaces: Vec::new(),
            last_link: 0,
            order: Vec::new(),
            order_file: None,
            events,
            only: Some(2),
            tried: BTreeSet::new(),
        };
        let space = |id: i64, name: &str| WorkspaceRow {
            keep_awake: false,
            qass_skip: false,
            id,
            name: name.into(),
            terminals: 1,
            asleep: false,
            sleeping: Vec::new(),
            closable: true,
            ink: None,
            link: None,
            goal: None,
        };
        mux.answer(
            0,
            ControlResponse::Workspaces {
                id: "list-1".into(),
                workspaces: vec![space(1, "default"), space(2, "alpha")],
            },
        )
        .await
        .expect("answered");
        let listed: ControlResponse = body_of(&std::mem::take(&mut mux.down));
        assert_eq!(
            listed,
            ControlResponse::Workspaces {
                id: "list-1".into(),
                workspaces: vec![space(2, "alpha")],
            }
        );

        let tab = |terminal: u32, workspace: i64| -> AgentRow {
            serde_json::from_value(serde_json::json!({
                "terminal": terminal, "workspace": workspace, "program": null,
                "behind": null, "state": "idle", "generation": null,
            }))
            .expect("row")
        };
        let leaf = |terminal| Pane::Leaf { terminal };
        mux.answer(
            0,
            ControlResponse::Agents {
                id: "list-2".into(),
                agents: vec![tab(5, 1), tab(6, 2)],
                splits: vec![leaf(5), leaf(6)],
            },
        )
        .await
        .expect("answered");
        let listed: ControlResponse = body_of(&std::mem::take(&mut mux.down));
        assert_eq!(
            listed,
            ControlResponse::Agents {
                id: "list-2".into(),
                agents: vec![tab(6, 2)],
                splits: vec![leaf(6)],
            }
        );

        mux.control(ControlRequest::CreateTerminal {
            id: "new-1".into(),
            cwd: None,
            workspace: None,
            name: None,
        })
        .await
        .expect("sent");
        let asked: ControlRequest = body_of(&home_rx.try_recv().expect("the daemon was asked"));
        assert!(
            matches!(
                asked,
                ControlRequest::CreateTerminal {
                    workspace: Some(2),
                    ..
                }
            ),
            "{asked:?}"
        );
    }
}
