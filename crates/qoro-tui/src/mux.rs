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
use qoro_proto::{Channel, Frame, Reassembler};
use qoro_remote::Remote;
use qoro_runtime::client::{self, Connection, Incoming};
use qoro_runtime::control::{
    AgentRow, ControlRequest, ControlResponse, EventMessage, RemoteSession, WorkspaceRow,
};
use qoro_runtime::daemon::ClientWire;
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

pub fn is_remote_terminal(terminal: u32) -> bool {
    terminal & REMOTE_TERMINAL != 0
}

pub fn is_remote_workspace(workspace: i64) -> bool {
    workspace >= 1 << WORKSPACE_SHIFT
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
    /// The sessions on a machine the interface asked about, or why they could not be read.
    Probed {
        id: String,
        target: String,
        result: Result<Vec<RemoteSession>, String>,
    },
}

/// One daemon as the mux keeps it.
struct Link {
    /// Empty for the local daemon.
    name: String,
    /// Where it is and which session, for a remote; `None` for the local daemon.
    remote: Option<Remote>,
    /// Rung when something is asked of this link while it is down, so the next attempt
    /// to reach it happens now rather than at the end of its backoff: a person clicking
    /// on an offline machine is the best sign there is that its network is back.
    wake: Arc<Notify>,
    /// The connection now up, if any, and its number, so a message from a connection that
    /// has since been replaced is recognised and dropped.
    conn: Option<(u64, mpsc::UnboundedSender<BytesMut>)>,
    control_seq: u32,
    seqs: BTreeMap<u32, u32>,
    /// Rows already in the interface's numbering.
    agents: Vec<AgentRow>,
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
            conn: None,
            control_seq: 0,
            seqs: BTreeMap::new(),
            agents: Vec::new(),
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

    /// The space rows this link contributes to the merged list.
    fn listed_spaces(&self, link: usize) -> Vec<WorkspaceRow> {
        if self.name.is_empty() && !self.listed {
            return Vec::new();
        }
        if self.conn.is_some() && self.listed {
            return self.spaces.clone();
        }
        if self.listed {
            return self
                .spaces
                .iter()
                .map(|row| WorkspaceRow {
                    id: row.id,
                    name: format!("{} ({})", row.name, self.reason),
                    terminals: 0,
                })
                .collect();
        }
        vec![WorkspaceRow {
            id: (link as i64) << WORKSPACE_SHIFT,
            name: format!("{} ({})", self.name, self.reason),
            terminals: 0,
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
    /// For the tasks the mux starts itself: a new link's supervisor, a session list.
    events: mpsc::Sender<FromLink>,
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
        let frames = qoro_proto::chunk(channel, stream, *seq, None, body.into());
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
        self.respond(&ControlResponse::Agents {
            id: id.to_string(),
            agents,
        })
        .await
    }

    fn merged_spaces(&self) -> Vec<WorkspaceRow> {
        self.links
            .iter()
            .enumerate()
            .filter_map(|(i, l)| l.as_ref().map(|l| l.listed_spaces(i)))
            .flatten()
            .collect()
    }

    fn merged_agents(&self) -> Vec<AgentRow> {
        self.links
            .iter()
            .flatten()
            .flat_map(|l| l.agents.iter().cloned())
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
        ));
        self.links.push(Some(entry));
        Some(link)
    }

    /// Something was asked of a link that is down: try it now rather than after the wait.
    fn wake(&mut self, link: usize) {
        if let Some(l) = self.link(link)
            && l.conn.is_none()
        {
            l.wake.notify_one();
        }
    }

    /// Add the machine at `target` from the interface, keep it in the list of remotes,
    /// and say under what name it will appear.
    async fn connect_remote(
        &mut self,
        id: String,
        target: String,
        session: String,
    ) -> io::Result<()> {
        let session = if session.trim().is_empty() {
            "default".to_string()
        } else {
            session.trim().to_string()
        };
        if !qoro_remote::saved::valid_target(&target) {
            return self
                .failed(id, format!("{target:?} is not an ssh address"))
                .await;
        }
        if !qoro_runtime::paths::valid_session_name(&session) {
            return self
                .failed(id, format!("{session:?} cannot be a session name"))
                .await;
        }
        // Before anything is kept: a machine saved that this interface cannot show would
        // appear only on the next start, as if added then.
        if self.links.len() > MAX_REMOTES {
            return self.failed(id, "no room for another machine".into()).await;
        }
        let file = qoro_remote::saved::path();
        // Read again right before writing, so a remote added from a shell a moment ago is
        // kept rather than written over, and counted when checking names. A file that does
        // not parse is refused rather than replaced: it is what the person wrote.
        let mut saved = match qoro_remote::saved::load_from(&file) {
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
        if let Some(same) = known
            .iter()
            .find(|r| r.target == target && r.session == session)
        {
            let name = same.name.clone();
            return self
                .failed(id, format!("{target} {session} is already here as {name}"))
                .await;
        }
        let taken: Vec<&str> = known.iter().map(|r| r.name.as_str()).collect();
        let remote = Remote {
            name: qoro_remote::saved::name_for(&target, &taken),
            target,
            session,
        };
        saved.push(remote.clone());
        if let Err(e) = qoro_remote::saved::save_to(&file, &saved) {
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
                    None => (self.home, None),
                };
                // A directory is a place on one machine. Carried to another it is at best
                // missing and at worst somewhere else with the same name.
                let cwd = cwd.filter(|_| link == self.last_link);
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
            R::CloseWorkspace { id, workspace } => {
                let (link, workspace) = workspace_in(workspace);
                self.send_one(link, R::CloseWorkspace { id, workspace })
                    .await
            }
            R::RenameTerminal { id, terminal, name } => {
                let (link, terminal) = terminal_in(terminal);
                self.send_one(link, R::RenameTerminal { id, terminal, name })
                    .await
            }
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
            R::ListRemoteSessions { id, target } => {
                if !qoro_remote::saved::valid_target(&target) {
                    return self
                        .failed(id, format!("{target:?} is not an ssh address"))
                        .await;
                }
                // ssh can take seconds, and the mux has renders to pass meanwhile: asked in a
                // task of its own, answered when it reports back.
                let events = self.events.clone();
                tokio::spawn(async move {
                    let result = crate::link::list_sessions(&target)
                        .await
                        .map(|listed| {
                            listed
                                .into_iter()
                                .map(|s| RemoteSession {
                                    name: s.name,
                                    running: s.running,
                                    tabs: s.tabs,
                                })
                                .collect()
                        })
                        .map_err(|e| e.to_string());
                    let _ = events.send(FromLink::Probed { id, target, result }).await;
                });
                Ok(())
            }
            R::ConnectRemote {
                id,
                target,
                session,
            } => self.connect_remote(id, target, session).await,
            // Everything else the interface sends names no terminal: it is about the daemon
            // this machine runs, or the one remote when there is no local one.
            other => self.send_one(self.home, other).await,
        }
    }

    async fn on_link(&mut self, message: FromLink) -> io::Result<()> {
        match message {
            FromLink::Up { link, conn, writer } => {
                let Some(l) = self.link(link) else {
                    return Ok(());
                };
                l.conn = Some((conn, writer));
                l.control_seq = 0;
                l.seqs.clear();
                l.hidden.clear();
                l.reason = "connecting".to_string();
                l.send(&ControlRequest::ListWorkspaces {
                    id: "mux-spaces".into(),
                });
                l.send(&ControlRequest::ListAgents {
                    id: "mux-agents".into(),
                });
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
                    if l.conn.is_none() && reason.len() > "offline: ".len() && l.reason != reason {
                        l.reason = reason;
                        self.push_lists("mux").await?;
                    }
                    return Ok(());
                }
                l.conn = None;
                l.stale_agents = false;
                l.stale_spaces = false;
                l.agents.clear();
                l.desired.clear();
                l.reason = if reason.is_empty() {
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
            FromLink::Probed { id, target, result } => match result {
                Ok(sessions) => {
                    self.respond(&ControlResponse::RemoteSessions {
                        id,
                        target,
                        sessions,
                    })
                    .await
                }
                Err(reason) => self.failed(id, reason).await,
            },
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
            Incoming::Render { .. } | Incoming::Image { .. } => Ok(()),
            Incoming::Event(EventMessage::AgentChanged { terminal }) => {
                let Some(terminal) = terminal_out(link, terminal) else {
                    return Ok(());
                };
                let body = serde_json::to_vec(&EventMessage::AgentChanged { terminal })
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
        let Some(l) = self.link(link) else {
            return Ok(());
        };
        l.inflight.remove(&id);
        let name = l.name.clone();
        match response {
            C::Agents { agents, .. } => {
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
                l.spaces = workspaces
                    .into_iter()
                    .filter_map(|row| {
                        Some(WorkspaceRow {
                            id: workspace_out(link, row.id)?,
                            name: space_name(&name, &row.name),
                            terminals: row.terminals,
                        })
                    })
                    .collect();
                // Its own answer to coming up, or the interface's question: either way the
                // interface gets the whole list, with anything that vanished closed first.
                let id = if id.starts_with("mux-") {
                    "mux".to_string()
                } else {
                    id
                };
                self.push_lists(&id).await
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
            // Answers that carry no number of either kind.
            other => self.respond(&other).await,
        }
    }
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
        })
        .await
        .is_err()
    {
        writer.abort();
        return;
    }
    let mut connection = Connection::raw();
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

/// Keep one remote connected for as long as the mux runs.
async fn supervise(link: usize, remote: Remote, events: mpsc::Sender<FromLink>, wake: Arc<Notify>) {
    let mut backoff = Duration::from_secs(1);
    let mut conn = 0u64;
    loop {
        conn += 1;
        let attempted = Instant::now();
        match crate::link::open(&remote, true).await {
            Ok(opened) => {
                let crate::link::Link {
                    stream,
                    leftover,
                    child,
                    said,
                } = opened;
                let started = Instant::now();
                pump(link, conn, stream, leftover, &events).await;
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

fn last_word(said: &Arc<Mutex<String>>) -> String {
    said.lock().map(|s| s.clone()).unwrap_or_default()
}

/// Serve the interface's side of `down` from the local daemon at `local`, if any, and the
/// `remotes`. Returns when the interface goes away or the local daemon does.
pub async fn serve<D>(
    down: D,
    local: Option<(tokio::net::UnixStream, BytesMut)>,
    remotes: Vec<Remote>,
) where
    D: AsyncRead + AsyncWrite + Send + 'static,
{
    let (down_rd, down_wr) = tokio::io::split(down);
    let (tui_tx, mut tui_rx) = mpsc::unbounded_channel();
    tokio::spawn(read_tui(down_rd, tui_tx));
    let (link_tx, mut link_rx) = mpsc::channel::<FromLink>(64);

    let mut links: Vec<Option<Link>> = vec![None];
    let has_local = local.is_some();
    if let Some((stream, leftover)) = local {
        links[0] = Some(Link::new(String::new(), None));
        let events = link_tx.clone();
        tokio::spawn(async move {
            pump(0, 1, stream, leftover, &events).await;
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
    let mut mux = Mux {
        links,
        home,
        down: down_wr,
        down_seqs: BTreeMap::new(),
        pushed_spaces: Vec::new(),
        last_link: home,
        events: link_tx,
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
    loop {
        let step = tokio::select! {
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

/// The interface over the local daemon at `socket` (when given) and `remotes`.
pub async fn run(
    socket: Option<&Path>,
    remotes: Vec<Remote>,
    notice: Option<String>,
) -> io::Result<()> {
    let local = match socket {
        Some(socket) => Some(client::connect(socket, "qoro-tui").await?),
        None => None,
    };
    // A megabyte each way: a full screen snapshot is tens of kilobytes, and the interface
    // reads as fast as it paints.
    let (tui_side, mux_side) = tokio::io::duplex(1 << 20);
    tokio::spawn(serve(mux_side, local, remotes));
    crate::run::run_stream(tui_side, BytesMut::new(), notice).await
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
    fn a_mixed_subscription_splits_by_daemon() {
        let a = terminal_out(1, 4).expect("fits");
        let b = terminal_out(2, 4).expect("fits");
        let by = split(&[3, a, b, 9]);
        assert_eq!(by[&0], vec![3, 9]);
        assert_eq!(by[&1], vec![4]);
        assert_eq!(by[&2], vec![4]);
    }
}
