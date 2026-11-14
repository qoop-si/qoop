//! What a supervising agent needs beyond reading and prompting: hearing that a
//! terminal changed, starting and ending terminals, pressing named keys, and
//! refusing to write to a process it did not read.
//!
//! Every verb here is a client of something the daemon already serves: the
//! event lane, `ListAgents`, `CreateTerminal`, `CloseTerminal` and the
//! terminal input lane. The one control method added for them is `Focus`, since
//! which terminal an interface shows was never the daemon's to know before.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use qoop_runtime::client;
use qoop_runtime::control::{
    AgentRow, ControlRequest, ControlResponse, EventMessage, WorkspaceRow,
};
use qoop_runtime::daemon::ClientWire;
use qoop_runtime::paths;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// States a terminal can be waited for: the index's own closed set.
const STATES: &[&str] = &[
    "starting",
    "idle",
    "working",
    "blocked",
    "done",
    "failed",
    "uncertain",
    "unknown",
    "sleeping",
];

/// What `wait` waits for when not told: every state that means "look now".
const DEFAULT_UNTIL: &[&str] = &["idle", "done", "blocked", "failed"];

/// The exit status of a wait that ran out of time, the same as `timeout(1)`.
pub(crate) const TIMED_OUT: i32 = 124;

/// How long `new` waits for a fresh shell before typing its command.
const SHELL_PATIENCE: Duration = Duration::from_secs(10);

/// How long `close` waits for the index to stop listing a closed terminal.
/// Past the daemon's own teardown grace, which ends with a kill.
const CLOSE_PATIENCE: Duration = Duration::from_secs(10);

/// The gap between two named keys, so `esc` then `enter` does not arrive as
/// one alt-enter in an agent's key parser.
const KEY_GAP: Duration = Duration::from_millis(50);

/// One connection to this session's daemon, held for the whole command.
///
/// One and not one per question, because `wait` depends on it: the daemon
/// starts a connection's event pump when it accepts it, so a change made
/// between a listing and the wait that follows is still delivered on the
/// same connection. A second connection would open a window where it is not.
struct Link {
    rd: tokio::net::unix::OwnedReadHalf,
    wr: tokio::net::unix::OwnedWriteHalf,
    inbuf: bytes::BytesMut,
    connection: client::Connection,
    seq: u32,
    input_seq: BTreeMap<u32, u32>,
    /// The terminal and text `wait --match` follows on the live screen, and whether the
    /// newest screen of that terminal holds it.
    screen_text: Option<(u32, String)>,
    on_screen: bool,
}

impl Link {
    async fn open(who: &str) -> std::io::Result<Self> {
        Self::open_as(who, false).await
    }

    /// The same, as a connection that only watches: a daemon with nothing awake leaves
    /// with this one still open, which is what lets a notch watch without keeping it up.
    async fn open_observer(who: &str) -> std::io::Result<Self> {
        Self::open_as(who, true).await
    }

    async fn open_as(who: &str, observer: bool) -> std::io::Result<Self> {
        let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
        let socket = paths::socket_path(&dir);
        // These verbs never start a daemon; `connect` says when there is none.
        let (stream, inbuf) = if observer {
            client::connect_observer(&socket, who).await?
        } else {
            client::connect(&socket, who).await?
        };
        let (rd, wr) = stream.into_split();
        Ok(Self {
            rd,
            wr,
            inbuf,
            connection: client::Connection::new(),
            seq: 0,
            input_seq: BTreeMap::new(),
            screen_text: None,
            on_screen: false,
        })
    }

    /// Every space, as the daemon lists them.
    async fn spaces(&mut self) -> std::io::Result<Vec<WorkspaceRow>> {
        match self
            .ask(&ControlRequest::ListWorkspaces {
                id: super::request_id("spaces"),
            })
            .await?
            .0
        {
            ControlResponse::Workspaces { workspaces, .. } => Ok(workspaces),
            _ => Err(std::io::Error::other(
                "unexpected answer to the space listing",
            )),
        }
    }

    /// The space an awake terminal is in, from the index.
    async fn workspace_of(&mut self, terminal: u32) -> std::io::Result<Option<i64>> {
        match self
            .ask(&ControlRequest::ListAgents {
                id: super::request_id("agents"),
            })
            .await?
            .0
        {
            ControlResponse::Agents { agents, .. } => Ok(agents
                .iter()
                .find(|row| row.terminal == terminal)
                .map(|row| row.workspace)),
            _ => Err(std::io::Error::other(
                "unexpected answer to the agent listing",
            )),
        }
    }

    /// A space by id or name, made when the name is new.
    ///
    /// The list and the create hold a lock in the session's directory, so two callers
    /// naming the same new space make it once: the daemon makes a space per request,
    /// which the interface relies on, and eight parallel `new --space x` made three
    /// spaces called `x` without it.
    async fn space(&mut self, wanted: &str) -> std::io::Result<i64> {
        let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
        let _held = qoop_runtime::lock::StartupLock::acquire(&dir.join("spaces.lock"))?;
        let ControlResponse::Workspaces { workspaces, .. } = self
            .ask(&ControlRequest::ListWorkspaces {
                id: super::request_id("spaces"),
            })
            .await?
            .0
        else {
            return Err(std::io::Error::other(
                "unexpected answer to the space listing",
            ));
        };
        if let Some(id) = pick_space(&workspaces, wanted)? {
            return Ok(id);
        }
        match self
            .ask(&ControlRequest::CreateWorkspace {
                id: super::request_id("space"),
                name: wanted.to_string(),
            })
            .await?
            .0
        {
            ControlResponse::WorkspaceCreated { workspace, .. } => Ok(workspace),
            ControlResponse::Failed { reason, .. } => Err(std::io::Error::other(reason)),
            _ => Err(std::io::Error::other("unexpected answer to a new space")),
        }
    }

    /// One control round trip, plus whatever else arrived while waiting.
    ///
    /// A screen that arrived in between is read here, the one place every round trip
    /// passes: a subscription delivers the current screen inside its own round trip, and
    /// a `wait --match` that only read screens between round trips never saw text that
    /// was already there.
    async fn ask(
        &mut self,
        request: &ControlRequest,
    ) -> std::io::Result<(ControlResponse, Vec<client::Incoming>)> {
        let (answer, stashed) = super::control_roundtrip(
            &mut self.rd,
            &mut self.wr,
            &mut self.inbuf,
            &mut self.connection,
            &mut self.seq,
            request,
            Duration::from_secs(10),
        )
        .await?;
        for incoming in &stashed {
            self.note_screen(incoming);
        }
        Ok((answer, stashed))
    }

    /// Update `on_screen` from a render of the `screen_text` terminal; true if it was one.
    fn note_screen(&mut self, incoming: &client::Incoming) -> bool {
        match (incoming, &self.screen_text) {
            (
                client::Incoming::Render {
                    terminal, snapshot, ..
                },
                Some((watched, text)),
            ) if terminal == watched => {
                self.on_screen = snapshot.lines.iter().any(|l| l.contains(text.as_str()));
                true
            }
            _ => false,
        }
    }

    /// The live index, plus whatever else arrived while asking for it.
    async fn agents(&mut self) -> std::io::Result<(Vec<AgentRow>, Vec<client::Incoming>)> {
        match self
            .ask(&ControlRequest::ListAgents {
                id: super::request_id("agents"),
            })
            .await?
        {
            (ControlResponse::Agents { agents, .. }, stashed) => Ok((agents, stashed)),
            (ControlResponse::Failed { reason, .. }, _) => Err(std::io::Error::other(reason)),
            _ => Err(std::io::Error::other(
                "unexpected answer to the agent listing",
            )),
        }
    }

    /// Bytes into a terminal on the input lane, signed like `send` signs them.
    async fn input(&mut self, terminal: u32, bytes: Vec<u8>) -> std::io::Result<()> {
        let (from_terminal, from_token) = super::cli_sender();
        let seq = self.input_seq.entry(terminal).or_insert(0);
        let frame = client::encode_command_to(
            terminal,
            &ClientWire::Input {
                bytes,
                from_terminal,
                from_token,
            },
            seq,
        )?;
        self.wr.write_all(&frame).await?;
        self.wr.flush().await
    }

    /// Read until an event arrives, acknowledging renders on the way.
    ///
    /// Renders are acknowledged because the daemon disconnects a reader that
    /// holds unacknowledged ones past `NO_ACK_DEADLINE`, and a connection is
    /// subscribed to a terminal the moment it is accepted. `None` at the
    /// deadline. A render of the `screen_text` terminal updates `on_screen`
    /// and wakes the caller with no events.
    async fn next_events(
        &mut self,
        deadline: Option<Instant>,
    ) -> std::io::Result<Option<Vec<EventMessage>>> {
        let mut read = [0u8; 64 * 1024];
        loop {
            if let Some(ack) = self.connection.renders().due_ack(Instant::now()) {
                self.wr.write_all(&ack).await?;
            }
            let tick = Duration::from_secs(1);
            let wait = match deadline {
                Some(at) => {
                    let left = at.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Ok(None);
                    }
                    left.min(tick)
                }
                None => tick,
            };
            let n = match tokio::time::timeout(wait, self.rd.read(&mut read)).await {
                Err(_) => continue,
                Ok(n) => n?,
            };
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "the daemon closed the connection",
                ));
            }
            self.inbuf.extend_from_slice(&read[..n]);
            let mut events = Vec::new();
            let mut screen = false;
            for incoming in self.connection.drain(&mut self.inbuf)? {
                if let client::Incoming::Event(event) = incoming {
                    events.push(event);
                } else {
                    screen |= self.note_screen(&incoming);
                }
            }
            if !events.is_empty() || screen {
                return Ok(Some(events));
            }
        }
    }
}

/// Whether any of these arrivals says a watched terminal may have changed.
fn touches(events: &[EventMessage], watched: &[u32]) -> bool {
    events.iter().any(|event| match event {
        EventMessage::AgentChanged { terminal } => watched.contains(terminal),
        EventMessage::Resync => true,
        // Which terminal an interface shows says nothing about any agent, nor does the board,
        // the question inbox, a result shown, a recording or the words of a chat.
        EventMessage::Focus { .. }
        | EventMessage::TasksChanged
        | EventMessage::AsksChanged
        | EventMessage::MessagesChanged { .. }
        | EventMessage::ShowsChanged
        | EventMessage::SchedulesChanged
        | EventMessage::Listen { .. } => false,
    })
}

/// The same, over what a control round trip set aside.
fn stashed_touches(stashed: Vec<client::Incoming>, watched: &[u32]) -> bool {
    let events: Vec<EventMessage> = stashed
        .into_iter()
        .filter_map(|incoming| match incoming {
            client::Incoming::Event(event) => Some(event),
            _ => None,
        })
        .collect();
    touches(&events, watched)
}

/// Each session's agent rows, by session name: what a watch prints from. One daemon, so
/// one entry, `default`, kept so a reader of `--all` still finds its rows under a name.
pub(crate) type Sessions = BTreeMap<String, Vec<AgentRow>>;

/// `qoop agents --watch`: the index now, then again each time it reads differently.
///
/// Woken by the daemon's own news on this connection, never by a timer: a watcher over
/// a quiet session costs nothing. What it prints is the whole index each time, since a
/// reader that missed a line would otherwise hold a row that is no longer true. Ends when
/// the daemon does, so a caller that wants to outlive a session starts it again.
pub(crate) fn run_watch(
    line: impl Fn(&Sessions) -> std::io::Result<String>,
) -> std::io::Result<()> {
    use std::io::Write;

    let line = |rows: &[AgentRow]| {
        line(&Sessions::from([(
            paths::DEFAULT_SESSION.to_string(),
            rows.to_vec(),
        )]))
    };
    let rt = tokio::runtime::Runtime::new()?;
    let watch = async move {
        let mut last = String::new();
        // Printed only when it changes, so a watcher that reconnects says nothing new.
        let mut say = |text: String| -> std::io::Result<bool> {
            if text == last {
                return Ok(true);
            }
            let mut out = std::io::stdout().lock();
            // A reader that went away ends the watch, rather than a panic on write.
            if writeln!(out, "{text}").and_then(|()| out.flush()).is_err() {
                return Ok(false);
            }
            last = text;
            Ok(true)
        };
        loop {
            let ended = match Link::open_observer("qoop-watch").await {
                Ok(mut link) => match follow(&mut link, &line, &mut say).await {
                    Ok(()) => return Ok(()),
                    Err(e) => e,
                },
                Err(e) => e,
            };
            if !daemon_gone(&ended) {
                return Err(ended);
            }
            // Nothing is awake, so nothing is listed: said, then waited out without a
            // process started or a timer run, until a daemon binds its socket again.
            if !say(line(&[])?)? {
                return Ok(());
            }
            if !wait_for_daemon().await? {
                return Err(ended);
            }
        }
    };
    rt.block_on(async {
        tokio::select! {
            ended = watch => ended,
            () = reader_gone() => Ok(()),
        }
    })
}

/// Resolves once whoever reads stdout has gone: a pipe's reader closed, or the ssh link a
/// remote watch runs over dropped (no tty there, so no hangup arrives). A watch over a quiet
/// session writes nothing for hours, and without this it only learnt of it on its next
/// write, outliving its reader all that time. Woken by the kernel's edge (epoll, kqueue),
/// never by a timer. Never resolves for a stdout that cannot be watched, such as a file.
async fn reader_gone() {
    use tokio::io::{Interest, unix::AsyncFd};

    if let Ok(out) = AsyncFd::with_interest(std::io::stdout(), Interest::WRITABLE) {
        while let Ok(mut ready) = out.writable().await {
            if ready.ready().is_write_closed() {
                return;
            }
            ready.clear_ready();
        }
    }
    std::future::pending().await
}

/// Whether an error means there is no daemon now (never started, or it left), rather than
/// a daemon that refused or misbehaved.
fn daemon_gone(e: &std::io::Error) -> bool {
    use std::io::ErrorKind as K;
    matches!(
        e.kind(),
        K::NotFound | K::ConnectionRefused | K::UnexpectedEof | K::ConnectionReset | K::BrokenPipe
    )
}

/// Follow one daemon's agents until it goes, printing each change through `say`.
async fn follow(
    link: &mut Link,
    line: &impl Fn(&[AgentRow]) -> std::io::Result<String>,
    say: &mut impl FnMut(String) -> std::io::Result<bool>,
) -> std::io::Result<()> {
    let mut unsubscribed = false;
    loop {
        let (agents, mut stashed) = link.agents().await?;
        // The daemon subscribes every new connection to its lowest terminal. A watcher
        // draws no screen, so the renders go back before they start to cost anything.
        // What arrived meanwhile is kept: a change during this round trip is news too.
        if !unsubscribed {
            unsubscribed = true;
            let (_, meanwhile) = link
                .ask(&ControlRequest::Unsubscribe {
                    id: super::request_id("watch"),
                    terminals: agents.iter().map(|r| r.terminal).collect(),
                })
                .await?;
            stashed.extend(meanwhile);
        }
        if !say(line(&agents)?)? {
            return Ok(());
        }
        // A change that landed while asking is news already had: ask again at once.
        if stashed.iter().any(|i| {
            matches!(
                i,
                client::Incoming::Event(EventMessage::AgentChanged { .. } | EventMessage::Resync)
            )
        }) {
            continue;
        }
        // A row carries its agent's task, so a board change can change a row while its state
        // holds (a stage the daemon moved after the turn ended): looked at too, and said only
        // when the listing reads differently.
        loop {
            let events = link.next_events(None).await?.unwrap_or_default();
            if events.iter().any(|e| {
                !matches!(
                    e,
                    EventMessage::Focus { .. }
                        | EventMessage::AsksChanged
                        | EventMessage::MessagesChanged { .. }
                        | EventMessage::ShowsChanged
                        | EventMessage::SchedulesChanged
                )
            }) {
                break;
            }
        }
    }
}

/// Wait, without a timer, until a daemon's socket appears in the runtime directory, and
/// answer true then. False where there is no way to wait (anything but macOS), and the
/// watch ends as it did before: the one watcher that stays up, the notch, is macOS only.
#[cfg(target_os = "macos")]
async fn wait_for_daemon() -> std::io::Result<bool> {
    let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
    let socket = paths::socket_path(&dir);
    tokio::task::spawn_blocking(move || paths::wait_for_socket(&dir, &socket))
        .await
        .map_err(std::io::Error::other)??;
    Ok(true)
}

#[cfg(not(target_os = "macos"))]
async fn wait_for_daemon() -> std::io::Result<bool> {
    Ok(false)
}

/// The least time between two screens `read --watch` prints. An agent streaming output
/// renders far faster than anyone reads; a viewer gets the newest screen at this pace.
const WATCH_GAP: Duration = Duration::from_millis(150);

/// `qoop read <terminal> --watch`: the screen now, then again each time it changes.
///
/// A viewer of the same grid the interface draws, on one connection subscribed to that
/// terminal alone. It never sends a size: the grid belongs to its owner (ADR-007), so
/// watching changes nothing for the agent. Ends when the terminal or the daemon does.
pub(crate) fn run_read_watch(
    terminal: u32,
    emit: impl Fn(&qoop_runtime::daemon::SnapshotWire) -> std::io::Result<String>,
) -> std::io::Result<()> {
    use std::io::Write;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let mut link = Link::open_observer("qoop-read").await?;
        let (agents, _) = link.agents().await?;
        // The daemon subscribes a new connection to its lowest terminal; any other one
        // goes back before its renders cost anything.
        let others: Vec<u32> = agents
            .iter()
            .map(|r| r.terminal)
            .filter(|t| *t != terminal)
            .collect();
        if !others.is_empty() {
            link.ask(&ControlRequest::Unsubscribe {
                id: super::request_id("read"),
                terminals: others,
            })
            .await?;
        }
        let (answer, stashed) = link
            .ask(&ControlRequest::Subscribe {
                id: super::request_id("read"),
                terminals: vec![terminal],
            })
            .await?;
        match answer {
            ControlResponse::Subscribed { terminals, .. } if terminals.contains(&terminal) => {}
            _ => {
                return Err(std::io::Error::other(format!(
                    "no such terminal: {terminal}"
                )));
            }
        }
        let mut pending = None;
        for incoming in stashed {
            if let client::Incoming::Render {
                terminal: t,
                snapshot,
                ..
            } = incoming
                && t == terminal
            {
                pending = Some(snapshot);
            }
        }
        let mut last = String::new();
        let mut shown = Instant::now() - WATCH_GAP;
        let mut read = [0u8; 64 * 1024];
        loop {
            if pending.is_some() && shown.elapsed() >= WATCH_GAP {
                let text = emit(pending.as_ref().expect("checked"))?;
                pending = None;
                shown = Instant::now();
                if text != last {
                    let mut out = std::io::stdout().lock();
                    // A reader that went away ends the watch, rather than a panic on write.
                    if writeln!(out, "{text}").and_then(|()| out.flush()).is_err() {
                        return Ok(());
                    }
                    last = text;
                }
            }
            if let Some(ack) = link.connection.renders().due_ack(Instant::now()) {
                link.wr.write_all(&ack).await?;
            }
            // Asleep until the next render, woken each second only to acknowledge.
            let wait = if pending.is_some() {
                WATCH_GAP.saturating_sub(shown.elapsed())
            } else {
                Duration::from_secs(1)
            };
            let n = match tokio::time::timeout(wait, link.rd.read(&mut read)).await {
                Err(_) => {
                    // Adopted by launchd: whoever started this watch is gone, and a quiet
                    // terminal would never make a write fail to say so. Holding the
                    // subscription would also keep the tab from being put to sleep.
                    if std::os::unix::process::parent_id() == 1 {
                        return Ok(());
                    }
                    continue;
                }
                Ok(n) => n?,
            };
            if n == 0 {
                return Ok(());
            }
            link.inbuf.extend_from_slice(&read[..n]);
            let mut changed = false;
            for incoming in link.connection.drain(&mut link.inbuf)? {
                changed |= note(incoming, terminal, &mut pending);
            }
            // A change to this terminal may be its end, and so may news that was missed.
            // Asked again until an answer comes with no news of its own: a change that
            // arrived during the question may be the end, and nothing will say so twice.
            while changed {
                let (agents, meanwhile) = link.agents().await?;
                // A screen that arrived while asking is the newest one there is: kept, or a
                // terminal that then goes quiet would stay on the screen before it.
                changed = false;
                for incoming in meanwhile {
                    changed |= note(incoming, terminal, &mut pending);
                }
                // Gone, or put to sleep: either way there is no screen left to follow.
                if !agents
                    .iter()
                    .any(|r| r.terminal == terminal && r.state != "sleeping")
                {
                    return Ok(());
                }
            }
        }
    })
}

/// One arrival on a `read --watch` connection: a screen of the watched terminal is kept
/// as the newest, and true says the index should be read again (a change to that terminal,
/// or news that was dropped).
fn note(
    incoming: client::Incoming,
    terminal: u32,
    pending: &mut Option<qoop_runtime::daemon::SnapshotWire>,
) -> bool {
    match incoming {
        client::Incoming::Render {
            terminal: t,
            snapshot,
            ..
        } if t == terminal => {
            *pending = Some(snapshot);
            false
        }
        client::Incoming::Event(EventMessage::AgentChanged { terminal: t }) => t == terminal,
        client::Incoming::Event(EventMessage::Resync) => true,
        _ => false,
    }
}

/// `qoop read <terminal> --back UP --lines N`: history rows, the first of them `up` rows
/// before the newest one written, with where they start and how many there are.
pub(crate) fn read_back(
    terminal: u32,
    up: usize,
    rows: usize,
) -> std::io::Result<(u64, u64, Vec<String>)> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let mut link = Link::open("qoop-read").await?;
        match link
            .ask(&ControlRequest::Scrollback {
                id: super::request_id("back"),
                terminal,
                up,
                rows,
            })
            .await?
            .0
        {
            ControlResponse::Scrollback {
                from, total, lines, ..
            } => Ok((from, total, lines)),
            ControlResponse::Failed { reason, .. } => Err(std::io::Error::other(reason)),
            _ => Err(std::io::Error::other("unexpected answer to a history read")),
        }
    })
}

/// `qoop focus <terminal>`: every attached interface shows that terminal, and the pids of
/// the ones that were asked are handed back for whoever wants to raise their windows.
pub(crate) fn run_focus(terminal: u32) -> std::io::Result<Vec<u32>> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let mut link = Link::open("qoop-focus").await?;
        match link
            .ask(&ControlRequest::Focus {
                id: super::request_id("focus"),
                terminal,
            })
            .await?
            .0
        {
            ControlResponse::Focused { clients, .. } => Ok(clients),
            ControlResponse::Failed { reason, .. } => Err(std::io::Error::other(reason)),
            _ => Err(std::io::Error::other("unexpected answer to focus")),
        }
    })
}

/// Why a write must not go to this terminal under this generation, if not.
///
/// The generation is the process, not the state: a write checked against
/// the generation read before it cannot land on the agent that replaced the
/// one that was read.
pub(crate) fn generation_refusal(rows: &[AgentRow], terminal: u32, want: u64) -> Option<String> {
    let Some(row) = rows.iter().find(|r| r.terminal == terminal) else {
        return Some(format!("no such terminal: {terminal}"));
    };
    match row.generation {
        Some(g) if g == want => None,
        Some(g) => Some(format!(
            "terminal {terminal} is generation {g} now, not {want}: read it again before writing"
        )),
        None => Some(format!(
            "terminal {terminal} has no generation yet, not {want}: read it again before writing"
        )),
    }
}

/// Take a trailing `--generation N` off a command's arguments.
///
/// Only as the last two words: a prompt's text is arguments too, and
/// `qoop prompt 3 explain --generation 5 to me` is a sentence about the flag,
/// not the flag. Anything that is not a number after it is left as text.
pub(crate) fn take_generation(args: &mut Vec<String>) -> Option<u64> {
    let [.., flag, value] = args.as_slice() else {
        return None;
    };
    // After `--` it is text, whatever it spells.
    if flag != "--generation" || args[..args.len() - 2].iter().any(|a| a == "--") {
        return None;
    }
    let value = value.parse::<u64>().ok()?;
    args.truncate(args.len() - 2);
    Some(value)
}

/// Refuse a write whose generation moved, over a connection of its own.
///
/// For `send` and `prompt`, which predate this and open their own. Nothing
/// is asked when no generation was given.
pub(crate) fn check_generation(terminal: u32, want: Option<u64>) -> std::io::Result<()> {
    let Some(want) = want else {
        return Ok(());
    };
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let (rows, _) = Link::open("qoop-generation").await?.agents().await?;
        match generation_refusal(&rows, terminal, want) {
            Some(reason) => Err(std::io::Error::other(reason)),
            None => Ok(()),
        }
    })
}

/// The terminal this command runs in, when it runs in one of this session.
///
/// Both halves, because terminal numbers repeat across sessions: tab 2 of
/// another session is not this tab. `home` is the session this process was
/// started in, read before `--session` replaced it.
/// The terminal this command runs in, when it runs in one of this machine's.
fn own_terminal() -> Option<u32> {
    qoop_runtime::legacy::var("QOOP_TERMINAL")
        .ok()?
        .parse()
        .ok()
}

/// `qoop wait`: block until a terminal reaches a state.
///
/// Level triggered: a
/// target already there returns at once. Exits 124 on timeout, 1 when a
/// target closes or nothing can be waited on.
pub(crate) fn run_wait(args: &[String]) -> std::io::Result<()> {
    let help = "usage: qoop wait [target...] [--until STATE[,STATE...]] [--match TEXT] [--timeout SECS] [--json]";
    let mut targets = Vec::new();
    let mut text: Option<String> = None;
    let mut until: Vec<String> = Vec::new();
    let mut timeout = None;
    let mut json = false;
    let mut rest = args.iter().skip(2);
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--match" => {
                let value = rest.next().filter(|t| !t.is_empty());
                text = Some(value.ok_or_else(|| super::usage(help))?.clone());
            }
            "--until" => {
                let value = rest.next().ok_or_else(|| super::usage(help))?;
                for state in value.split(',').filter(|s| !s.is_empty()) {
                    if !STATES.contains(&state) {
                        return Err(super::usage(format!(
                            "unknown state {state}; one of {}",
                            STATES.join(", ")
                        )));
                    }
                    until.push(state.to_string());
                }
            }
            "--timeout" => {
                let secs: u64 = rest
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| super::usage(help))?;
                timeout = Some(Duration::from_secs(secs));
            }
            flag if flag.starts_with("--") => {
                return Err(super::usage(format!("unknown flag {flag}; {help}")));
            }
            target => targets.push(target.to_string()),
        }
    }
    // `--match` follows one screen, so it names one terminal. Alone it waits for the
    // text whatever the state; with `--until` both must hold.
    if text.is_some() && targets.len() != 1 {
        return Err(super::usage(format!(
            "--match needs exactly one target; {help}"
        )));
    }
    if until.is_empty() && text.is_none() {
        until = DEFAULT_UNTIL.iter().map(|s| s.to_string()).collect();
    }
    let deadline = timeout.map(|t| Instant::now() + t);
    let own = own_terminal();
    // Named tabs are checked one by one; with none named, qass is not made to wait on what the
    // owner keeps from it.
    for target in &targets {
        super::qass_guard(target)?;
    }
    let hide_skipped = targets.is_empty() && crate::qass::called_by_qass();

    let rt = tokio::runtime::Runtime::new()?;
    let matched = rt.block_on(async {
        let mut link = Link::open("qoop-wait").await?;
        let (rows, _) = link.agents().await?;
        let watched: Vec<u32> = if targets.is_empty() {
            rows.iter()
                .filter(|r| r.agent && Some(r.terminal) != own)
                .filter(|r| !(hide_skipped && r.qass_skip))
                .map(|r| r.terminal)
                .collect()
        } else {
            let mut out = Vec::new();
            for target in &targets {
                let row = super::resolve_target(&rows, target).map_err(std::io::Error::other)?;
                out.push(row.terminal);
            }
            out
        };
        if watched.is_empty() {
            return Err(std::io::Error::other(
                "no agent terminals to wait on; name a target",
            ));
        }
        // Renders are not what this waits on. Dropping the subscription the
        // daemon made at accept keeps a long wait from being paced by output.
        let all = rows.iter().map(|r| r.terminal).collect();
        link.ask(&ControlRequest::Unsubscribe {
            id: super::request_id("unsubscribe"),
            terminals: all,
        })
        .await?;
        // Except the one screen `--match` reads, which is the live one only, never the
        // scrollback. Subscribing asks the terminal for its current screen.
        if let Some(text) = &text {
            link.screen_text = Some((watched[0], text.clone()));
            link.ask(&ControlRequest::Subscribe {
                id: super::request_id("subscribe"),
                terminals: vec![watched[0]],
            })
            .await?;
        }
        loop {
            let (rows, stashed) = link.agents().await?;
            // A change that arrived while this listing was in flight may be
            // newer than the listing: look again before deciding.
            if stashed_touches(stashed, &watched) {
                continue;
            }
            if let Some(gone) = watched
                .iter()
                .find(|t| !rows.iter().any(|r| r.terminal == **t))
            {
                return Err(std::io::Error::other(format!("terminal {gone} closed")));
            }
            // A row with no identity is a process being swapped, not a state: between
            // a shell and the `sleep` it starts the row briefly reads `idle` with no
            // program, and `wait` answered that at once.
            let hits: Vec<AgentRow> = rows
                .into_iter()
                .filter(|r| watched.contains(&r.terminal) && r.readiness != "spawning")
                .filter(|r| until.is_empty() || until.contains(&r.state))
                .filter(|_| text.is_none() || link.on_screen)
                .collect();
            if !hits.is_empty() {
                return Ok(Some(hits));
            }
            loop {
                match link.next_events(deadline).await? {
                    None => return Ok(None),
                    // No events is a screen that changed under `--match`.
                    Some(events) if events.is_empty() || touches(&events, &watched) => break,
                    Some(_) => {}
                }
            }
        }
    })?;
    let Some(rows) = matched else {
        let what = match (&text, until.is_empty()) {
            (Some(text), true) => format!("{text:?} on screen"),
            (Some(text), false) => format!("{} with {text:?} on screen", until.join(",")),
            (None, _) => until.join(","),
        };
        eprintln!(
            "qoop: timed out after {}s waiting for {what}",
            timeout.map(|t| t.as_secs()).unwrap_or(0),
        );
        std::process::exit(TIMED_OUT);
    };
    if json {
        println!(
            "{}",
            serde_json::to_string(&rows).map_err(std::io::Error::other)?
        );
    } else {
        for row in &rows {
            println!("{}", super::agent_line(row));
        }
    }
    Ok(())
}

/// How long an agent that was busy when a prompt arrived has to stay stopped before
/// `prompt --wait` ends.
const SETTLE: Duration = Duration::from_secs(2);

/// What `Turn` says about the newest row.
pub(crate) enum Finish {
    /// Keep waiting.
    Not,
    /// This row is the answer.
    Now,
    /// This row is the answer if nothing changes before then.
    At(Instant),
}

/// When `prompt --wait` is over, from the rows seen after the send.
///
/// One rule per starting point, and no reading of stamps against the clock. An agent
/// that was stopped at the send (`idle`, `done`, `blocked`, `failed`, `sleeping`) has
/// taken the prompt once it is seen busy, or its activity stamp moves past the one read
/// before the send, or its generation changes; the first stopped row after that ends the
/// wait. Not a stamp that merely reaches the second of the send: that is also the `done`
/// a chained `prompt --wait` just returned on. An agent that was busy at the send may fold
/// the prompt into the turn it is running or queue it for the next one, and a row cannot
/// say which: the wait ends once it has been stopped for [`SETTLE`] with no busy row in
/// between. Blocked is stopped: it needs a person either way.
pub(crate) struct Turn {
    busy_at_send: bool,
    active: Option<i64>,
    generation: Option<u64>,
    taken: bool,
    quiet_since: Option<Instant>,
}

/// The states a turn is over in.
const STOPPED: &[&str] = &["idle", "done", "blocked", "failed", "sleeping"];

impl Turn {
    pub(crate) fn new(before: &AgentRow) -> Self {
        Self {
            busy_at_send: !STOPPED.contains(&before.state.as_str()),
            active: before.last_active_at_unix,
            generation: before.generation,
            taken: false,
            quiet_since: None,
        }
    }

    pub(crate) fn observe(&mut self, row: &AgentRow, now: Instant) -> Finish {
        let busy = !STOPPED.contains(&row.state.as_str());
        if self.busy_at_send {
            if busy {
                self.quiet_since = None;
                return Finish::Not;
            }
            return Finish::At(*self.quiet_since.get_or_insert(now) + SETTLE);
        }
        self.taken = self.taken
            || busy
            || row.last_active_at_unix > self.active
            || row.generation != self.generation;
        if self.taken && !busy {
            Finish::Now
        } else {
            Finish::Not
        }
    }
}

/// How `prompt --wait` ended, when it did not fail.
pub(crate) enum TurnWait {
    Done(AgentRow),
    /// The last row seen, so the message can say what it was doing instead.
    TimedOut(Option<AgentRow>),
}

/// Wait for the turn a prompt just sent to `before.terminal` caused, per [`Turn`].
///
/// The settle is a deadline this sleeps towards, not a poll: the next event either
/// replaces it or it passes and ends the wait. A row with no identity is a process being
/// swapped and is not judged.
pub(crate) async fn wait_for_turn(
    before: &AgentRow,
    timeout: Option<Duration>,
) -> std::io::Result<TurnWait> {
    let terminal = before.terminal;
    let deadline = timeout.map(|t| Instant::now() + t);
    let mut link = Link::open("qoop-prompt").await?;
    let (rows, _) = link.agents().await?;
    link.ask(&ControlRequest::Unsubscribe {
        id: super::request_id("unsubscribe"),
        terminals: rows.iter().map(|r| r.terminal).collect(),
    })
    .await?;
    let mut turn = Turn::new(before);
    loop {
        let (rows, stashed) = link.agents().await?;
        if stashed_touches(stashed, &[terminal]) {
            continue;
        }
        let Some(row) = rows.into_iter().find(|r| r.terminal == terminal) else {
            return Err(std::io::Error::other(format!("terminal {terminal} closed")));
        };
        let mut settle = None;
        if row.readiness != "spawning" {
            match turn.observe(&row, Instant::now()) {
                Finish::Now => return Ok(TurnWait::Done(row)),
                Finish::At(at) => settle = Some(at),
                Finish::Not => {}
            }
        }
        let mut last = Some(row);
        loop {
            let wake = [deadline, settle].into_iter().flatten().min();
            match link.next_events(wake).await? {
                None if settle.is_some_and(|at| Instant::now() >= at) => {
                    return Ok(TurnWait::Done(last.take().expect("seen above")));
                }
                None => return Ok(TurnWait::TimedOut(last)),
                Some(events) if touches(&events, &[terminal]) => break,
                Some(_) => {}
            }
        }
    }
}

/// Print how a `prompt --wait` ended and exit with the status `qoop wait` uses.
pub(crate) fn finish_turn(waited: TurnWait) -> std::io::Result<()> {
    match waited {
        TurnWait::Done(row) => {
            println!("{}", super::agent_line(&row));
            Ok(())
        }
        TurnWait::TimedOut(row) => {
            match row {
                Some(row) => eprintln!("qoop: timed out; last seen {}", super::agent_line(&row)),
                None => eprintln!("qoop: timed out"),
            }
            std::process::exit(TIMED_OUT);
        }
    }
}

/// Parsed `qoop new` arguments, shared with `qoop worktree new`.
#[derive(Default)]
pub(crate) struct NewOpts {
    pub(crate) name: Option<String>,
    pub(crate) cwd: Option<std::path::PathBuf>,
    /// A space id, or a name, which is made when nobody has used it yet.
    pub(crate) space: Option<String>,
    pub(crate) command: Vec<String>,
    /// A command line typed as written, ahead of `command`'s quoted words: a person's own
    /// launcher line from `[agents]`, which may hold quotes or an assignment.
    pub(crate) typed: Option<String>,
    /// Say, before anything is typed, that the terminal this runs in started it (`qoop worktree
    /// new`): the daemon marks it as qass's when that is qass's space, so a task its command
    /// opens at once already carries the mark.
    pub(crate) spawned: bool,
}

const NEW_USAGE: &str =
    "usage: qoop new [--name NAME] [--cwd DIR] [--space ID|NAME] [-- command...]";

pub(crate) fn parse_new_args(args: &[String]) -> Result<NewOpts, String> {
    let mut opts = NewOpts::default();
    let mut rest = args.iter().skip(2);
    while let Some(arg) = rest.next() {
        let mut value = || {
            rest.next()
                .cloned()
                .ok_or_else(|| format!("missing value for {arg}; {NEW_USAGE}"))
        };
        match arg.as_str() {
            "--name" => opts.name = Some(value()?),
            "--cwd" => opts.cwd = Some(value()?.into()),
            "--space" => opts.space = Some(value()?),
            "--" => {
                opts.command = rest.by_ref().cloned().collect();
                break;
            }
            other => return Err(format!("unexpected {other}; {NEW_USAGE}")),
        }
    }
    Ok(opts)
}

/// A space by id, or by a name only one space has. Never made: `sleep` and `wake` act on
/// what exists, and a name two spaces share is refused rather than guessed.
pub(crate) fn resolve_space(spaces: &[WorkspaceRow], wanted: &str) -> std::io::Result<i64> {
    if let Some(space) = wanted
        .parse::<i64>()
        .ok()
        .and_then(|id| spaces.iter().find(|s| s.id == id))
    {
        return Ok(space.id);
    }
    let named: Vec<&WorkspaceRow> = spaces.iter().filter(|s| s.name == wanted).collect();
    match named.as_slice() {
        [one] => Ok(one.id),
        [] => Err(std::io::Error::other(format!(
            "no space with id or name {wanted}"
        ))),
        many => Err(std::io::Error::other(format!(
            "{} spaces are called {wanted}; use an id: {}",
            many.len(),
            many.iter()
                .map(|s| s.id.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// What `qoop sleep` and `qoop wake` are asked about: a space by id or name, or one tab
/// by number (`--tab N`), plus `--json`.
struct Target {
    space: Option<String>,
    tab: Option<u32>,
    json: bool,
}

fn parse_target(args: &[String], help: &str) -> std::io::Result<Target> {
    let mut target = Target {
        space: None,
        tab: None,
        json: false,
    };
    let mut rest = args.iter().skip(2);
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--json" => target.json = true,
            "--tab" => {
                let tab = rest.next().and_then(|t| t.parse().ok());
                target.tab = Some(tab.ok_or_else(|| super::usage(help))?);
            }
            _ if target.space.is_none() && !arg.starts_with('-') => {
                target.space = Some(arg.clone())
            }
            _ => return Err(super::usage(help)),
        }
    }
    Ok(target)
}

/// The space a sleeping tab is in, from the space list.
fn space_of_tab(spaces: &[WorkspaceRow], tab: u32) -> Option<i64> {
    spaces
        .iter()
        .find(|s| s.sleeping.iter().any(|t| t.terminal == tab))
        .map(|s| s.id)
}

/// How many tabs `panes` are: each pane counts as the tab it sleeps in, so the panes of a
/// split are one tab, and a pane not listed asleep counts on its own.
fn tabs_of(spaces: &[WorkspaceRow], panes: &[u32]) -> usize {
    let tab = |pane: u32| {
        spaces
            .iter()
            .flat_map(|s| &s.sleeping)
            .find(|t| t.terminal == pane)
            .and_then(|t| t.lead)
            .unwrap_or(pane)
    };
    panes
        .iter()
        .map(|p| tab(*p))
        .collect::<std::collections::BTreeSet<_>>()
        .len()
}

/// What `qoop wake` says once the daemon answered, judged by the space list read before it
/// and, when no tab came back, the one read after. No tab back from a space (or a tab) with
/// nothing listed asleep had nothing to wake; from one listed asleep before and no longer
/// after, another wake brought it back first; from one still asleep, or when the second list
/// could not be read (`after` is `None`), the wake failed (a stop during it, a pane that
/// could not open). `what` is the space as it was named, else its id.
#[allow(clippy::too_many_arguments)]
fn wake_said(
    before: &[WorkspaceRow],
    after: Option<&[WorkspaceRow]>,
    workspace: i64,
    tab: Option<u32>,
    what: &str,
    terminals: usize,
    resumed: usize,
    changed: Option<&[u32]>,
) -> Result<String, String> {
    if terminals > 0 {
        // Counted by the sleeping tabs listed before the wake: a split is one tab.
        let tabs = changed.map_or(terminals, |panes| tabs_of(before, panes));
        return Ok(format!(
            "space {what}: {} awake again, {} resumed",
            super::count(tabs, "tab"),
            super::count(resumed, "agent")
        ));
    }
    let asleep = |spaces: &[WorkspaceRow]| {
        spaces
            .iter()
            .filter(|s| s.id == workspace)
            .flat_map(|s| &s.sleeping)
            .any(|t| tab.is_none_or(|tab| t.terminal == tab))
    };
    if !asleep(before) {
        Ok(format!("nothing in space {what} was asleep"))
    } else if after.is_some_and(|after| !asleep(after)) {
        Ok(match tab {
            Some(tab) => format!("tab {tab} is awake"),
            None => format!("space {what} is awake"),
        })
    } else {
        Err(format!("space {what}: no tab came back"))
    }
}

/// An older daemon answers a request that named a tab without saying which terminals it
/// changed, because it acted on the whole space whatever was asked.
fn older_daemon(done: &str) -> std::io::Error {
    std::io::Error::other(format!(
        "the running daemon is older: the whole space {done}; restart it (`qoop stop`) to \
         act on one tab"
    ))
}

/// `qoop sleep <space>`, or `qoop sleep --tab N`: end every tab of a space, or that one tab
/// (every pane of its split), and keep them, so they cost nothing until they wake. A tab
/// that could not come back as it is stays awake, and the answer says which and why.
pub(crate) fn run_sleep(args: &[String]) -> std::io::Result<()> {
    let help = "usage: qoop sleep <space> | --tab <terminal> [--json]";
    let target = parse_target(args, help)?;
    if target.space.is_some() == target.tab.is_some() {
        return Err(super::usage(help));
    }
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        // No daemon is nothing awake: there is nothing to put to sleep, and starting one to
        // find that out would wake every space that was left awake.
        let mut link = match Link::open("qoop-sleep").await {
            Ok(link) => link,
            Err(e) if daemon_gone(&e) => {
                println!("nothing is awake");
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let spaces = link.spaces().await?;
        let space = match (&target.space, target.tab) {
            (Some(wanted), _) => resolve_space(&spaces, wanted)?,
            (None, Some(tab)) => link.workspace_of(tab).await?.ok_or_else(|| {
                std::io::Error::other(format!("there is no tab {tab} awake"))
            })?,
            (None, None) => unreachable!("checked above"),
        };
        match link
            .ask(&ControlRequest::SleepWorkspace {
                id: super::request_id("sleep"),
                workspace: space,
                terminal: target.tab,
            })
            .await?
            .0
        {
            ControlResponse::WorkspaceAsleep { changed: None, .. } if target.tab.is_some() => {
                Err(older_daemon("slept"))
            }
            ControlResponse::WorkspaceAsleep {
                terminals,
                changed,
                stayed,
                ..
            } => {
                if target.json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "space": space,
                            "asleep": true,
                            "ended": terminals,
                            "slept": changed.unwrap_or_default(),
                            "stayed": stayed
                                .iter()
                                .map(|s| serde_json::json!({ "tab": s.terminal, "reason": s.reason }))
                                .collect::<Vec<_>>(),
                        })
                    );
                } else {
                    let what = match target.tab {
                        Some(tab) => format!("tab {tab}"),
                        None => format!("space {}", target.space.as_deref().unwrap_or_default()),
                    };
                    // A tab that stays awake keeps the space from being asleep.
                    let state = if stayed.is_empty() {
                        "asleep"
                    } else {
                        "partly asleep"
                    };
                    // The sleep is done: a list that fails now costs the count its splits,
                    // never the answer and the reasons under it.
                    let tabs = match &changed {
                        Some(panes) => link
                            .spaces()
                            .await
                            .map_or(terminals, |s| tabs_of(&s, panes)),
                        None => terminals,
                    };
                    println!("{what} is {state}: {} ended", super::count(tabs, "tab"));
                    // The reason names each pane of the tab and why.
                    for s in &stayed {
                        println!("stays awake: {}", s.reason);
                    }
                }
                Ok(())
            }
            ControlResponse::Failed { reason, .. } => Err(std::io::Error::other(reason)),
            _ => Err(std::io::Error::other("unexpected answer to a sleep")),
        }
    })
}

/// `qoop wake [space]`, or `qoop wake --tab N`: bring a sleeping tab back, every sleeping
/// tab of a space, or with nothing named the tab that went to sleep last. Starts the
/// daemon when none is running, since waking is asking for one.
pub(crate) fn run_wake(args: &[String]) -> std::io::Result<()> {
    let help = "usage: qoop wake [space] | --tab <terminal> [--json]";
    let target = parse_target(args, help)?;
    if target.space.is_some() && target.tab.is_some() {
        return Err(super::usage(help));
    }
    let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
    super::ensure_daemon(&paths::socket_path(&dir))?;
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let mut link = Link::open("qoop-wake").await?;
        let spaces = link.spaces().await?;
        let workspace = match (&target.space, target.tab) {
            (Some(wanted), _) => Some(resolve_space(&spaces, wanted)?),
            (None, Some(tab)) => Some(
                space_of_tab(&spaces, tab)
                    .ok_or_else(|| std::io::Error::other(format!("there is no tab {tab} asleep")))?,
            ),
            (None, None) => None,
        };
        match link
            .ask(&ControlRequest::WakeWorkspace {
                id: super::request_id("wake"),
                workspace,
                terminal: target.tab,
            })
            .await?
            .0
        {
            ControlResponse::WorkspaceAwake { changed: None, .. } if target.tab.is_some() => {
                Err(older_daemon("woke"))
            }
            ControlResponse::WorkspaceAwake {
                workspace,
                terminals,
                resumed,
                changed,
                ..
            } => {
                if target.json {
                    println!("{}", serde_json::json!({ "space": workspace, "asleep": false, "tabs": terminals, "resumed": resumed }));
                    return Ok(());
                }
                let what = target
                    .space
                    .clone()
                    .unwrap_or_else(|| workspace.to_string());
                // Nothing back: the list read again tells a wake that another one beat to it
                // from one that failed.
                let after = if terminals == 0 {
                    link.spaces().await.ok()
                } else {
                    None
                };
                let said = wake_said(
                    &spaces,
                    after.as_deref(),
                    workspace,
                    target.tab,
                    &what,
                    terminals,
                    resumed,
                    changed.as_deref(),
                );
                println!("{}", said.map_err(std::io::Error::other)?);
                Ok(())
            }
            // Nothing named and nothing asleep, but tabs up: a daemon this wake started
            // brought back what a `qoop stop` ended, which is what was asked for.
            ControlResponse::Failed { reason, .. } if workspace.is_none() => {
                match link
                    .ask(&ControlRequest::ListTerminals {
                        id: super::request_id("wake"),
                    })
                    .await?
                    .0
                {
                    ControlResponse::Terminals { terminals, .. } if !terminals.is_empty() => {
                        if target.json {
                            println!("{}", serde_json::json!({ "asleep": false, "tabs": terminals.len() }));
                        } else {
                            println!(
                                "nothing was asleep; {} awake",
                                super::count(terminals.len(), "tab")
                            );
                        }
                        Ok(())
                    }
                    _ => Err(std::io::Error::other(reason)),
                }
            }
            ControlResponse::Failed { reason, .. } => Err(std::io::Error::other(reason)),
            _ => Err(std::io::Error::other("unexpected answer to a wake")),
        }
    })
}

/// `qoop new`: open a terminal, optionally typing a command into its shell.
///
/// Prints only the terminal number, so `t=$(qoop new ...)` works. The
/// command is typed rather than spawned, the same path a person takes: the
/// daemon starts the user's shell and nothing else (ADR-015), and the shell
/// gives the command its environment.
pub(crate) fn run_new(args: &[String]) -> std::io::Result<()> {
    let opts = parse_new_args(args).map_err(super::usage)?;
    open_terminal(&opts, |terminal| println!("{terminal}"))
}

/// Leave the daemon with something to attach to: a shell where the person is, when
/// nothing is open.
///
/// A daemon opens no terminal by itself, so a first start, one after every tab was closed,
/// or one after every space went to sleep would otherwise hand the interface nothing. Under a lock in the runtime
/// directory, so two clients starting at once open one shell. A directory that is gone or
/// that the daemon cannot use costs the shell its place, never the start.
pub(crate) fn ensure_awake() -> std::io::Result<()> {
    let here = std::env::current_dir().ok();
    let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
    let socket = paths::socket_path(&dir);
    let _held = qoop_runtime::lock::StartupLock::acquire(&if qoop_runtime::legacy::migrating() {
        qoop_runtime::legacy::awake_lock(&dir)
    } else {
        dir.join("awake.lock")
    })?;
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        // A daemon that was leaving as this connected closes it: started again, once.
        let mut link = match Link::open("qoop-start").await {
            Ok(link) => link,
            Err(e) if daemon_gone(&e) => {
                super::ensure_daemon(&socket)?;
                Link::open("qoop-start").await?
            }
            Err(e) => return Err(e),
        };
        if let ControlResponse::Terminals { terminals, .. } = link
            .ask(&ControlRequest::ListTerminals {
                id: super::request_id("start"),
            })
            .await?
            .0
            && !terminals.is_empty()
        {
            return Ok(());
        }
        // Nothing awake: the tab that went to sleep last is the one that was left, so it is
        // the one woken, alone. Only with nothing asleep either is there a shell to open, or
        // in a woken space that brought no tab back.
        let woken = match link
            .ask(&ControlRequest::WakeWorkspace {
                id: super::request_id("start"),
                workspace: None,
                terminal: None,
            })
            .await?
            .0
        {
            ControlResponse::WorkspaceAwake { terminals, .. } if terminals > 0 => return Ok(()),
            ControlResponse::WorkspaceAwake { workspace, .. } => Some(workspace),
            // Refused while a tab is still asleep: said, rather than a shell opened beside a
            // tab that did not come back.
            ControlResponse::Failed { reason, .. } => {
                if link
                    .spaces()
                    .await?
                    .iter()
                    .any(|space| !space.sleeping.is_empty())
                {
                    return Err(std::io::Error::other(reason));
                }
                None
            }
            _ => None,
        };
        let mut reason = String::new();
        for cwd in [here.map(|h| h.display().to_string()), None] {
            match link
                .ask(&ControlRequest::CreateTerminal {
                    id: super::request_id("start"),
                    cwd: cwd.clone(),
                    workspace: woken,
                    name: None,
                })
                .await?
                .0
            {
                ControlResponse::TerminalCreated { .. } => return Ok(()),
                ControlResponse::Failed { reason: why, .. } => reason = why,
                _ => reason = "unexpected answer to a create".into(),
            }
            if cwd.is_none() {
                break;
            }
        }
        Err(std::io::Error::other(reason))
    })
}

/// `qoop --space`: the space named, made when the name is new, awake and with a tab in it,
/// so an interface that shows it alone has something to show. Its id.
pub(crate) fn open_space(wanted: &str) -> std::io::Result<i64> {
    let here = std::env::current_dir().ok();
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        // A daemon that was leaving as this connected closes it: started again, once.
        let mut link = match Link::open("qoop-space").await {
            Ok(link) => link,
            Err(e) if daemon_gone(&e) => {
                let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
                super::ensure_daemon(&paths::socket_path(&dir))?;
                Link::open("qoop-space").await?
            }
            Err(e) => return Err(e),
        };
        let space = link.space(wanted).await?;
        let row = link.spaces().await?.into_iter().find(|s| s.id == space);
        let mut tabs = row.as_ref().map_or(0, |s| s.terminals);
        // Nothing of it awake: one of its sleeping tabs is woken, not all of them, so an
        // interface shown this space alone has something to show for the price of one.
        let sleeping = row
            .filter(|s| s.terminals == 0)
            .and_then(|s| s.sleeping.first().map(|t| t.lead.unwrap_or(t.terminal)));
        if let Some(tab) = sleeping {
            match link
                .ask(&ControlRequest::WakeWorkspace {
                    id: super::request_id("space"),
                    workspace: Some(space),
                    terminal: Some(tab),
                })
                .await?
                .0
            {
                ControlResponse::WorkspaceAwake { terminals, .. } => tabs = terminals,
                ControlResponse::Failed { reason, .. } => {
                    return Err(std::io::Error::other(reason));
                }
                _ => return Err(std::io::Error::other("unexpected answer to a wake")),
            }
        }
        if tabs > 0 {
            return Ok(space);
        }
        match link
            .ask(&ControlRequest::CreateTerminal {
                id: super::request_id("space"),
                cwd: here.map(|h| h.display().to_string()),
                workspace: Some(space),
                name: None,
            })
            .await?
            .0
        {
            ControlResponse::TerminalCreated { .. } => Ok(space),
            ControlResponse::Failed { reason, .. } => Err(std::io::Error::other(reason)),
            _ => Err(std::io::Error::other("unexpected answer to a create")),
        }
    })
}

/// Open one terminal as `opts` says, tell `announce` its number, then type its command.
pub(crate) fn open_terminal(opts: &NewOpts, announce: impl FnOnce(u32)) -> std::io::Result<()> {
    // Where the caller is, not where the daemon started: a person opening a
    // tab from here expects to land here. Absolute, because the daemon
    // refuses a relative path rather than guess what it is relative to.
    let here = std::env::current_dir()?;
    let cwd = match &opts.cwd {
        Some(dir) => std::path::absolute(here.join(dir))?,
        None => here,
    };

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let mut link = Link::open("qoop-new").await?;
        let workspace = match &opts.space {
            Some(space) => Some(link.space(space).await?),
            None => None,
        };
        let terminal = match link
            .ask(&ControlRequest::CreateTerminal {
                id: super::request_id("new"),
                cwd: Some(cwd.display().to_string()),
                workspace,
                name: opts.name.clone(),
            })
            .await?
            .0
        {
            ControlResponse::TerminalCreated { terminal, .. } => terminal,
            ControlResponse::Failed { reason, .. } => return Err(std::io::Error::other(reason)),
            _ => return Err(std::io::Error::other("unexpected answer to a create")),
        };
        announce(terminal);
        // Never in the way of the start: a daemon that cannot mark it is not asked again.
        if opts.spawned
            && let (Some(by), Some(token)) = crate::task::here()
        {
            let _ = link
                .ask(&ControlRequest::TaskWrite {
                    id: super::request_id("spawned"),
                    terminal: Some(by),
                    token: Some(token),
                    task: None,
                    action: qoop_runtime::control::TaskOp::Spawned { terminal },
                })
                .await;
        }
        if opts.command.is_empty() && opts.typed.is_none() {
            return Ok(());
        }
        // Typed once the shell has drawn something and gone quiet: a process
        // the daemon can name is not yet a line editor reading its input, and
        // a slow rc file runs in between. `fetch_snapshot` settles for half a
        // second, which is the quiet part.
        let deadline = Instant::now() + SHELL_PATIENCE;
        loop {
            let screen = super::fetch_snapshot(terminal, "qoop-new").await?;
            if screen.lines.iter().any(|l| !l.trim().is_empty()) {
                break;
            }
            if Instant::now() >= deadline {
                return Err(std::io::Error::other(format!(
                    "terminal {terminal} opened but its shell drew nothing within {}s; the command was not typed",
                    SHELL_PATIENCE.as_secs()
                )));
            }
        }
        let line: Vec<String> = opts
            .typed
            .iter()
            .cloned()
            .chain(opts.command.iter().map(|w| shell_quote(w)))
            .collect();
        // An agent started with words: those are not the person's, wherever they are said.
        paths::note_typed(terminal, &super::typed_by(), &line.join(" "));
        link.input(terminal, format!("{}\r", line.join(" ")).into_bytes())
            .await
    })
}

/// A space by id, then by exact name. `None` means a name nobody has used yet, which the
/// caller creates. A number that is neither an id nor a name is refused rather than made
/// into a space called `7`, because it was far more likely meant as an id.
pub(crate) fn pick_space(spaces: &[WorkspaceRow], wanted: &str) -> std::io::Result<Option<i64>> {
    let by_id = wanted
        .parse::<i64>()
        .ok()
        .and_then(|id| spaces.iter().find(|s| s.id == id));
    match by_id.or_else(|| spaces.iter().find(|s| s.name == wanted)) {
        Some(s) => Ok(Some(s.id)),
        None if wanted.parse::<i64>().is_ok() => Err(std::io::Error::other(format!(
            "no space with id or name {wanted}"
        ))),
        None => Ok(None),
    }
}

/// `qoop close`: end a terminal and what it started, as `ctrl-a x` does.
///
/// A number and never a name: a destructive verb does not guess. The
/// terminal this runs in is refused, because a supervisor that closes
/// itself has lost the loop it was running.
pub(crate) fn run_close(args: &[String]) -> std::io::Result<()> {
    let help = "usage: qoop close <terminal> [--generation G]";
    let mut args = args.to_vec();
    let generation = take_generation(&mut args);
    let terminal: u32 = match args.get(2..) {
        Some([n]) => n.parse().map_err(|_| super::usage(help))?,
        _ => return Err(super::usage(help)),
    };
    close_terminal(terminal, generation)?;
    println!("closed terminal {terminal}");
    Ok(())
}

/// What `qoop close` does, saying nothing: refused for the terminal this runs in, for qass's
/// own (see [`super::qass_guard`]) and, with a `generation`, when the process is no longer
/// that one; then closed, and returned once the index no longer lists it.
pub(crate) fn close_terminal(terminal: u32, generation: Option<u64>) -> std::io::Result<()> {
    if own_terminal() == Some(terminal) {
        return Err(std::io::Error::other(
            "refusing to close the terminal this command runs in",
        ));
    }
    super::qass_guard(&terminal.to_string())?;
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let mut link = Link::open("qoop-close").await?;
        if let Some(want) = generation {
            let (rows, _) = link.agents().await?;
            if let Some(reason) = generation_refusal(&rows, terminal, want) {
                return Err(std::io::Error::other(reason));
            }
        }
        match link
            .ask(&ControlRequest::CloseTerminal {
                id: super::request_id("close"),
                terminal,
            })
            .await?
            .0
        {
            ControlResponse::TerminalClosed { .. } => {}
            ControlResponse::Failed { reason, .. } => return Err(std::io::Error::other(reason)),
            _ => return Err(std::io::Error::other("unexpected answer to a close")),
        }
        // The daemon answers when the close is accepted, and the teardown
        // that contains what the terminal started runs after (ADR-014). Said
        // "closed" only once the index agrees, so a supervisor that lists
        // next does not find the terminal it was just told is gone.
        let deadline = Instant::now() + CLOSE_PATIENCE;
        loop {
            let (rows, _) = link.agents().await?;
            if !rows.iter().any(|r| r.terminal == terminal) {
                return Ok(());
            }
            let tick = (Instant::now() + Duration::from_millis(250)).min(deadline);
            if link.next_events(Some(tick)).await?.is_none() && Instant::now() >= deadline {
                return Err(std::io::Error::other(format!(
                    "terminal {terminal} accepted the close but was still listed after {}s",
                    CLOSE_PATIENCE.as_secs()
                )));
            }
        }
    })
}

const SPACE_USAGE: &str = "usage:
  qoop space rm <id|name>              remove a space with no tab in it
  qoop space goal <id|name> [text|-]   what the owner wants from it: no text prints it,
                                       an empty text clears it
  qoop space dir <id|name> [path]      the folder its new tabs start in: no path prints it";

/// `qoop space rm|goal|dir`.
pub(crate) fn run_space(args: &[String]) -> std::io::Result<()> {
    match (
        args.get(2).map(String::as_str),
        args.get(3..).unwrap_or_default(),
    ) {
        (Some("rm"), [wanted]) => space_rm(wanted),
        (Some("goal"), [wanted, words @ ..]) => {
            let goal = if words.is_empty() {
                None
            } else {
                Some(crate::task::text(words)?)
            };
            space_goal(wanted, goal)
        }
        (Some("dir"), [wanted]) => space_dir(wanted, None),
        (Some("dir"), [wanted, dir]) => {
            // Where the caller is for a relative path, as `qoop new --cwd` reads one.
            let dir = std::path::absolute(dir)?;
            space_dir(wanted, Some(dir.display().to_string()))
        }
        _ => Err(super::usage(SPACE_USAGE)),
    }
}

/// A link to the running daemon, never starting one, named plainly when nothing runs.
async fn space_link(name: &str) -> std::io::Result<Link> {
    Link::open(name).await.map_err(|e| {
        if daemon_gone(&e) {
            std::io::Error::other("nothing is awake: `qoop` or `qoop wake` starts it")
        } else {
            e
        }
    })
}

/// Print a space's goal, or set it (an empty text clears it).
fn space_goal(wanted: &str, goal: Option<String>) -> std::io::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let mut link = space_link("qoop-space-goal").await?;
        let spaces = link.spaces().await?;
        let space = resolve_space(&spaces, wanted)?;
        let row = spaces
            .iter()
            .find(|s| s.id == space)
            .expect("resolved above");
        let Some(goal) = goal else {
            match &row.goal {
                Some(goal) => println!("{goal}"),
                None => println!("space {} ({}) has no goal", row.id, row.name),
            }
            return Ok(());
        };
        match link
            .ask(&ControlRequest::SetSpaceGoal {
                id: super::request_id("space-goal"),
                workspace: space,
                goal,
            })
            .await?
            .0
        {
            ControlResponse::Renamed { .. } => Ok(()),
            ControlResponse::Failed { reason, .. } => Err(std::io::Error::other(reason)),
            _ => Err(std::io::Error::other("unexpected answer to a space goal")),
        }
    })
}

/// Print a space's folder, or set it: where a new tab in it starts when nobody names one.
fn space_dir(wanted: &str, dir: Option<String>) -> std::io::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let mut link = space_link("qoop-space-dir").await?;
        let spaces = link.spaces().await?;
        let space = resolve_space(&spaces, wanted)?;
        let row = spaces
            .iter()
            .find(|s| s.id == space)
            .expect("resolved above");
        let Some(dir) = dir else {
            match &row.dir {
                Some(dir) => println!("{dir}"),
                None => println!(
                    "space {} ({}) has no folder: its new tabs start where qoop does",
                    row.id, row.name
                ),
            }
            return Ok(());
        };
        match link
            .ask(&ControlRequest::SetSpaceDir {
                id: super::request_id("space-dir"),
                workspace: space,
                dir,
            })
            .await?
            .0
        {
            ControlResponse::Renamed { .. } => Ok(()),
            ControlResponse::Failed { reason, .. } => Err(std::io::Error::other(reason)),
            _ => Err(std::io::Error::other("unexpected answer to a space folder")),
        }
    })
}

/// `qoop space rm <id|name>`: drop an empty space, as `ctrl-a W` does.
///
/// Only an empty one: a space with a tab in it, awake or asleep, is refused with how many,
/// and the daemon checks the same again, because it is the one that knows. A name two
/// spaces share is refused rather than guessed. Never starts a daemon: with none running
/// there is no space to remove.
fn space_rm(wanted: &str) -> std::io::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let mut link = space_link("qoop-space-rm").await?;
        let spaces = link.spaces().await?;
        let space = resolve_space(&spaces, wanted)?;
        let row = spaces
            .iter()
            .find(|s| s.id == space)
            .expect("resolved above");
        if row.terminals > 0 {
            return Err(std::io::Error::other(format!(
                "space {} ({}) still holds {} tab{}, {} asleep: close them first",
                row.id,
                row.name,
                row.terminals,
                if row.terminals == 1 { "" } else { "s" },
                row.sleeping.len()
            )));
        }
        match link
            .ask(&ControlRequest::CloseWorkspace {
                id: super::request_id("space-rm"),
                workspace: space,
            })
            .await?
            .0
        {
            ControlResponse::WorkspaceClosed { .. } => {
                println!("removed space {} ({})", row.id, row.name);
                Ok(())
            }
            ControlResponse::Failed { reason, .. } => Err(std::io::Error::other(reason)),
            _ => Err(std::io::Error::other(
                "unexpected answer to a space removal",
            )),
        }
    })
}

/// `qoop send <terminal> --key NAME...`: press named keys, one frame each.
pub(crate) fn send_keys(terminal: u32, keys: &[String]) -> std::io::Result<()> {
    for key in keys {
        if key_bytes(key, false).is_none() {
            return Err(std::io::Error::other(format!(
                "unknown key {key}; one of enter esc tab shift-tab backspace space up down left right home end pageup pagedown ctrl-a..ctrl-z"
            )));
        }
    }
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        // The spelling of an arrow depends on what the program asked for, so
        // look only when an arrow is being sent: it costs a screen read.
        let app_cursor = if keys.iter().any(|k| key_needs_mode(k)) {
            super::fetch_snapshot(terminal, "qoop-send")
                .await?
                .modes
                .application_cursor
        } else {
            false
        };
        let mut link = Link::open("qoop-send").await?;
        match link
            .ask(&ControlRequest::ListTerminals {
                id: super::request_id("send"),
            })
            .await?
            .0
        {
            ControlResponse::Terminals { terminals, .. } if terminals.contains(&terminal) => {}
            _ => {
                return Err(std::io::Error::other(format!(
                    "no such terminal: {terminal}"
                )));
            }
        }
        for (i, key) in keys.iter().enumerate() {
            if i > 0 {
                tokio::time::sleep(KEY_GAP).await;
            }
            let bytes = key_bytes(key, app_cursor).expect("checked above");
            link.input(terminal, bytes).await?;
        }
        Ok(())
    })
}

/// The bytes one named key sends, or `None` for a name that is not a key.
///
/// `app_cursor` is the target's application cursor mode: arrows, home and end
/// are `SS3` there and `CSI` everywhere else, and the wrong spelling is a key
/// the program ignores (measured against `less`, see `ModesWire`).
pub(crate) fn key_bytes(name: &str, app_cursor: bool) -> Option<Vec<u8>> {
    let cursor = |c: u8| {
        let lead: &[u8] = if app_cursor { b"\x1bO" } else { b"\x1b[" };
        let mut out = lead.to_vec();
        out.push(c);
        out
    };
    let bytes = match name {
        "enter" => b"\r".to_vec(),
        "esc" => b"\x1b".to_vec(),
        "tab" => b"\t".to_vec(),
        "shift-tab" => b"\x1b[Z".to_vec(),
        "backspace" => b"\x7f".to_vec(),
        "space" => b" ".to_vec(),
        "up" => cursor(b'A'),
        "down" => cursor(b'B'),
        "right" => cursor(b'C'),
        "left" => cursor(b'D'),
        "home" => cursor(b'H'),
        "end" => cursor(b'F'),
        "pageup" => b"\x1b[5~".to_vec(),
        "pagedown" => b"\x1b[6~".to_vec(),
        _ => {
            let letter = name.strip_prefix("ctrl-")?;
            let [c] = letter.as_bytes() else {
                return None;
            };
            if !c.is_ascii_lowercase() {
                return None;
            }
            vec![c & 0x1f]
        }
    };
    Some(bytes)
}

/// Whether a key's spelling depends on the application cursor mode.
pub(crate) fn key_needs_mode(name: &str) -> bool {
    matches!(name, "up" | "down" | "right" | "left" | "home" | "end")
}

/// One argv word as a POSIX shell reads it back: bare when it is plainly
/// safe, single quoted otherwise, with an embedded quote closed and reopened.
pub(crate) fn shell_quote(word: &str) -> String {
    let safe = !word.is_empty()
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_./:=@%+-,".contains(&b));
    if safe {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frames as the daemon writes them: `body` on `channel` for `stream`.
    fn frames(channel: qoop_proto::Channel, stream: u32, body: &str) -> Vec<u8> {
        let mut out = bytes::BytesMut::new();
        for frame in qoop_proto::chunk(
            channel,
            stream,
            0,
            None,
            bytes::Bytes::from(body.to_string()),
        ) {
            frame.encode(&mut out).expect("encode");
        }
        out.to_vec()
    }

    /// The race the live test can only hit sometimes, made certain: the screen of the
    /// followed terminal lands before the answer to the round trip it arrived inside.
    #[tokio::test]
    async fn a_screen_that_lands_inside_a_round_trip_is_read() {
        let (ours, mut theirs) = tokio::net::UnixStream::pair().expect("a socket pair");
        let (rd, wr) = ours.into_split();
        let mut link = Link {
            rd,
            wr,
            inbuf: bytes::BytesMut::new(),
            connection: client::Connection::new(),
            seq: 0,
            input_seq: BTreeMap::new(),
            screen_text: Some((3, "STATIC_MARK_7".to_string())),
            on_screen: false,
        };
        let mut wire = frames(
            qoop_proto::Channel::Terminal,
            3,
            r#"{"cols":80,"rows":24,"cursor":[0,0],"lines":["$ echo STATIC_MARK_7","STATIC_MARK_7"]}"#,
        );
        wire.extend(frames(
            qoop_proto::Channel::Control,
            0,
            r#"{"op":"Subscribed","id":"sub-1","terminals":[3]}"#,
        ));
        theirs
            .write_all(&wire)
            .await
            .expect("the daemon's side writes");
        let (answer, _) = link
            .ask(&ControlRequest::Subscribe {
                id: "sub-1".to_string(),
                terminals: vec![3],
            })
            .await
            .expect("an answer");
        assert!(matches!(answer, ControlResponse::Subscribed { .. }));
        assert!(
            link.on_screen,
            "the screen that came with the answer was dropped"
        );
    }

    /// The same race from the other side: the screen lands in the same read as the
    /// answer, just after it, which the round trip used to drop along with everything
    /// else it had already taken out of the buffer.
    #[tokio::test]
    async fn a_screen_that_lands_just_after_the_answer_is_read() {
        let (ours, mut theirs) = tokio::net::UnixStream::pair().expect("a socket pair");
        let (rd, wr) = ours.into_split();
        let mut link = Link {
            rd,
            wr,
            inbuf: bytes::BytesMut::new(),
            connection: client::Connection::new(),
            seq: 0,
            input_seq: BTreeMap::new(),
            screen_text: Some((3, "STATIC_MARK_7".to_string())),
            on_screen: false,
        };
        let mut wire = frames(
            qoop_proto::Channel::Control,
            0,
            r#"{"op":"Subscribed","id":"sub-1","terminals":[3]}"#,
        );
        wire.extend(frames(
            qoop_proto::Channel::Terminal,
            3,
            r#"{"cols":80,"rows":24,"cursor":[0,0],"lines":["$ echo STATIC_MARK_7","STATIC_MARK_7"]}"#,
        ));
        theirs
            .write_all(&wire)
            .await
            .expect("the daemon's side writes");
        let (answer, _) = link
            .ask(&ControlRequest::Subscribe {
                id: "sub-1".to_string(),
                terminals: vec![3],
            })
            .await
            .expect("an answer");
        assert!(matches!(answer, ControlResponse::Subscribed { .. }));
        assert!(
            link.on_screen,
            "the screen that came right after the answer was dropped"
        );
    }

    #[test]
    fn keys_spell_arrows_by_cursor_mode() {
        assert_eq!(key_bytes("up", false).unwrap(), b"\x1b[A");
        assert_eq!(key_bytes("up", true).unwrap(), b"\x1bOA");
        assert_eq!(key_bytes("end", true).unwrap(), b"\x1bOF");
        assert_eq!(key_bytes("esc", true).unwrap(), b"\x1b");
        assert_eq!(key_bytes("enter", false).unwrap(), b"\r");
        assert_eq!(key_bytes("ctrl-c", false).unwrap(), [3]);
        assert_eq!(key_bytes("ctrl-a", false).unwrap(), [1]);
        assert_eq!(key_bytes("ctrl-z", false).unwrap(), [26]);
        for bad in ["ctrl-", "ctrl-C", "ctrl-1", "ctrl-ab", "Enter", "f1", ""] {
            assert_eq!(key_bytes(bad, false), None, "{bad}");
        }
        assert!(key_needs_mode("left") && !key_needs_mode("pageup"));
    }

    #[test]
    fn only_a_trailing_generation_is_the_flag() {
        let words = |s: &str| s.split(' ').map(str::to_string).collect::<Vec<_>>();
        let mut args = words("qoop prompt 3 hello --generation 7");
        assert_eq!(take_generation(&mut args), Some(7));
        assert_eq!(args, words("qoop prompt 3 hello"));
        for text in [
            "qoop prompt 3 explain --generation 5 to me",
            "qoop prompt 3 set --generation high",
            "qoop prompt 3 --generation",
            // After `--` it is text, whatever it spells.
            "qoop send 3 -- deploy --generation 5",
        ] {
            let mut args = words(text);
            assert_eq!(take_generation(&mut args), None, "{text}");
            assert_eq!(args, words(text), "the text is left whole");
        }
    }

    #[test]
    fn quoting_round_trips_through_a_real_shell() {
        let words = [
            "claude",
            "fix the bug",
            "it's",
            "",
            "$HOME",
            "a\"b",
            "--x=1",
        ];
        let line: Vec<String> = words.iter().map(|w| shell_quote(w)).collect();
        assert_eq!(line[0], "claude");
        assert_eq!(line[1], "'fix the bug'");
        assert_eq!(line[6], "--x=1");
        let script = format!(
            "for a in {}; do printf '%s\\n' \"$a\"; done",
            line.join(" ")
        );
        let out = std::process::Command::new("/bin/sh")
            .args(["-c", &script])
            .output()
            .expect("run sh");
        let back: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect();
        assert_eq!(back, words);
    }

    fn row(terminal: u32, name: Option<&str>, program: Option<&str>) -> AgentRow {
        AgentRow {
            command: None,
            clients: Vec::new(),
            menu: None,
            typed_by: None,
            parked: false,
            keep_awake: false,
            qass_skip: false,
            background: 0,
            subagents: 0,
            task: None,
            news: false,
            terminal,
            workspace: 1,
            name: name.map(str::to_string),
            program: program.map(str::to_string),
            behind: None,
            state: "idle".into(),
            agent: true,
            generation: None,
            readiness: "ready".to_string(),
            source: None,
            rule: None,
            manifest: None,
            agent_session: None,
            agent_title: None,
            last_active_at_unix: None,
            stop: None,
            cwd: None,
            slept_at_unix: None,
            activity: None,
            pgid: None,
            discards: None,
            status: None,
        }
    }

    fn at(state: &str, stamp: i64) -> AgentRow {
        let mut r = row(1, None, Some("claude"));
        r.state = state.into();
        r.generation = Some(4);
        r.last_active_at_unix = Some(stamp);
        r
    }

    fn settles_at(finish: Finish) -> Option<Instant> {
        match finish {
            Finish::At(at) => Some(at),
            _ => None,
        }
    }

    #[test]
    fn from_idle_the_turn_ends_on_the_first_stop_after_it_was_taken() {
        let t0 = Instant::now();
        let mut turn = Turn::new(&at("idle", 100));
        assert!(
            matches!(turn.observe(&at("idle", 100), t0), Finish::Not),
            "not taken"
        );
        assert!(matches!(turn.observe(&at("working", 106), t0), Finish::Not));
        assert!(matches!(turn.observe(&at("done", 108), t0), Finish::Now));
        // A turn so fast that no busy row was seen still moved the stamp.
        let mut fast = Turn::new(&at("idle", 100));
        assert!(matches!(fast.observe(&at("idle", 105), t0), Finish::Now));
        // Blocked after the send is where a person is needed: over.
        let mut asks = Turn::new(&at("idle", 100));
        assert!(matches!(asks.observe(&at("blocked", 106), t0), Finish::Now));
    }

    #[test]
    fn a_chained_wait_does_not_take_the_done_that_ended_the_previous_turn() {
        // `prompt --wait a; prompt --wait b` in one second: b is sent right after the
        // `done` a returned on, stamped in the same second, and nothing follows. The
        // stamp did not move past the row read before the send, so nothing was taken.
        let t0 = Instant::now();
        let before = at("done", 105);
        let mut turn = Turn::new(&before);
        assert!(matches!(turn.observe(&at("done", 105), t0), Finish::Not));
        assert!(matches!(turn.observe(&at("done", 105), t0), Finish::Not));
    }

    #[test]
    fn a_prompt_folded_into_the_running_turn_ends_after_one_quiet_settle() {
        // Working at the send, then a single done and nothing busy after it.
        let t0 = Instant::now();
        let mut turn = Turn::new(&at("working", 100));
        assert!(matches!(turn.observe(&at("working", 100), t0), Finish::Not));
        let done_at = t0 + Duration::from_secs(5);
        assert_eq!(
            settles_at(turn.observe(&at("done", 110), done_at)),
            Some(done_at + SETTLE)
        );
        // The same row seen again keeps the same deadline.
        assert_eq!(
            settles_at(turn.observe(&at("done", 110), done_at + SETTLE / 2)),
            Some(done_at + SETTLE)
        );
    }

    #[test]
    fn a_queued_prompt_resets_the_settle_and_ends_after_its_own_turn() {
        let t0 = Instant::now();
        let mut turn = Turn::new(&at("working", 100));
        let first_stop = t0 + Duration::from_secs(1);
        assert!(settles_at(turn.observe(&at("idle", 106), first_stop)).is_some());
        // The queued prompt starts inside the window: busy again, no deadline.
        assert!(matches!(
            turn.observe(&at("working", 106), first_stop + SETTLE / 2),
            Finish::Not
        ));
        let second_stop = t0 + Duration::from_secs(9);
        assert_eq!(
            settles_at(turn.observe(&at("done", 114), second_stop)),
            Some(second_stop + SETTLE)
        );
    }

    #[test]
    fn a_turn_starting_in_the_second_of_the_send_is_not_missed() {
        // Every stamp after the send is the send's own second.
        let t0 = Instant::now();
        let mut turn = Turn::new(&at("working", 100));
        assert!(settles_at(turn.observe(&at("idle", 105), t0)).is_some());
        assert!(matches!(turn.observe(&at("working", 105), t0), Finish::Not));
        let stop = t0 + Duration::from_secs(3);
        assert_eq!(
            settles_at(turn.observe(&at("idle", 105), stop)),
            Some(stop + SETTLE)
        );
    }

    #[test]
    fn a_space_is_found_by_id_then_name_and_an_unknown_id_is_refused() {
        let spaces = vec![
            WorkspaceRow {
                dir: None,
                keep_awake: false,
                qass_skip: false,
                id: 1,
                name: "default".into(),
                terminals: 1,
                asleep: false,
                sleeping: Vec::new(),
                closable: true,
                ink: None,
                link: None,
                goal: None,
            },
            WorkspaceRow {
                dir: None,
                keep_awake: false,
                qass_skip: false,
                id: 2,
                name: "work".into(),
                terminals: 0,
                asleep: false,
                sleeping: Vec::new(),
                closable: true,
                ink: None,
                link: None,
                goal: None,
            },
        ];
        assert_eq!(pick_space(&spaces, "2").unwrap(), Some(2));
        assert_eq!(pick_space(&spaces, "work").unwrap(), Some(2));
        assert_eq!(pick_space(&spaces, "fresh").unwrap(), None);
        assert!(pick_space(&spaces, "7").is_err());
        let named = vec![WorkspaceRow {
            dir: None,
            keep_awake: false,
            qass_skip: false,
            id: 3,
            name: "2024".into(),
            terminals: 0,
            asleep: false,
            sleeping: Vec::new(),
            closable: true,
            ink: None,
            link: None,
            goal: None,
        }];
        assert_eq!(
            pick_space(&named, "2024").unwrap(),
            Some(3),
            "a number that is no id is tried as a name"
        );
    }

    /// No tab back from a wake is "nothing was asleep" only when the list before it showed
    /// nothing asleep there. From a space with something asleep it is a failure, unless the
    /// list after shows another wake brought it back first (two wakes at once). A split that
    /// comes back is one tab.
    #[test]
    fn a_wake_that_brings_nothing_back_says_nothing_was_asleep_only_when_so() {
        let pane = |terminal, lead| qoop_runtime::control::SleepingTab {
            terminal,
            lead: Some(lead),
            title: None,
            agent: None,
            session: None,
            cwd: "/".into(),
        };
        let space = |id, sleeping| WorkspaceRow {
            dir: None,
            keep_awake: false,
            qass_skip: false,
            id,
            name: "docs".into(),
            terminals: 2,
            asleep: false,
            sleeping,
            closable: true,
            ink: None,
            link: None,
            goal: None,
        };
        let before = vec![space(1, vec![pane(6, 6), pane(7, 6)]), space(2, Vec::new())];
        let still = before.clone();
        let woken = vec![space(1, Vec::new()), space(2, Vec::new())];
        let failed = |s: &str| -> Result<String, String> { Err(s.to_string()) };
        let said = |s: &str| -> Result<String, String> { Ok(s.to_string()) };
        let nothing = |after: Option<&[WorkspaceRow]>, workspace, tab, what: &str| {
            wake_said(&before, after, workspace, tab, what, 0, 0, Some(&[]))
        };
        // Listed asleep before, and still after or with no second list: it failed.
        assert_eq!(
            nothing(Some(&still), 1, None, "docs"),
            failed("space docs: no tab came back")
        );
        assert_eq!(
            nothing(None, 1, Some(7), "1"),
            failed("space 1: no tab came back")
        );
        // Asleep before, awake after: another wake was first.
        assert_eq!(
            nothing(Some(&woken), 1, None, "docs"),
            said("space docs is awake")
        );
        assert_eq!(
            nothing(Some(&woken), 1, Some(7), "1"),
            said("tab 7 is awake")
        );
        // Nothing listed asleep there.
        assert_eq!(
            nothing(Some(&still), 2, None, "2"),
            said("nothing in space 2 was asleep")
        );
        assert_eq!(
            nothing(None, 1, Some(9), "1"),
            said("nothing in space 1 was asleep")
        );
        assert_eq!(
            wake_said(&before, None, 1, None, "docs", 2, 1, Some(&[6, 7])),
            said("space docs: 1 tab awake again, 1 agent resumed")
        );
    }

    #[test]
    fn new_takes_a_space_by_name_and_everything_after_the_separator() {
        let argv = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let opts = parse_new_args(&argv(&[
            "qoop", "new", "--space", "work", "--name", "fix", "--", "claude", "--resume",
        ]))
        .unwrap();
        assert_eq!(opts.space.as_deref(), Some("work"));
        assert_eq!(opts.name.as_deref(), Some("fix"));
        assert_eq!(opts.command, vec!["claude", "--resume"]);
        let missing = parse_new_args(&argv(&["qoop", "new", "--cwd"]));
        assert!(missing.is_err_and(|e| e.starts_with("missing value for --cwd")));
    }
}
