//! The connection to the daemons, on a thread of its own.
//!
//! The window speaks to the TUI's mux (`qoop_tui::mux::serve`) over an in-memory pipe, as the
//! TUI does: the mux holds this machine's daemon and every saved remote, gives a remote's
//! terminals and spaces their own numbers, and merges the agent and space lists. Reading the
//! pipe, reassembling frames and parsing every screen's JSON happen here, never on the
//! window's thread. A screen goes into one slot per terminal, the newest replacing an older
//! one the window has not taken yet, so memory stays at one screen per shown terminal.
//!
//! When this machine's daemon goes away (`qoop update` restarts it) the mux ends with it; the
//! link connects again, asks for the lists again and shows the same terminals.

use std::collections::{BTreeSet, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::BytesMut;
use qoop_runtime::client::{self, Connection, Incoming};
use qoop_runtime::control::{ControlRequest, ControlResponse, EventMessage, TerminalStream};
use qoop_runtime::daemon::{ClientWire, SnapshotWire};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// The newest screen per terminal that the window has not taken yet.
pub type Latest = Arc<Mutex<HashMap<TerminalStream, SnapshotWire>>>;

/// What the window asks of the daemons.
#[derive(Debug)]
pub enum Up {
    Input(TerminalStream, Vec<u8>),
    /// Text pasted: the daemon adds the markers when the program asked for bracketed paste.
    Paste(TerminalStream, Vec<u8>),
    Resize(TerminalStream, u16, u16),
    /// Receive renders for these (a session opened, or the window came back into view).
    Show(Vec<TerminalStream>),
    /// Stop the renders for these.
    Hide(Vec<TerminalStream>),
    Request(Box<ControlRequest>),
    /// What the checkout of a tab on this machine changed, read here in this process (git in
    /// the folder `cwd`), answered as `Changes` under `id` like a daemon's answer.
    Changes {
        id: String,
        terminal: TerminalStream,
        cwd: String,
    },
}

/// What the daemons told the window.
#[derive(Debug)]
pub enum Down {
    /// Linked to this machine's daemon, again or for the first time; the lists follow.
    Connected,
    Answer(Box<ControlResponse>),
    Event(EventMessage),
    /// New screens are waiting in `Latest`.
    Screens,
    /// The connection ended; the link is trying again.
    Disconnected(String),
}

/// Start the link to the daemon at `socket` and the saved remotes, and keep it across
/// restarts of either.
pub fn start(
    socket: PathBuf,
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
            runtime.block_on(keep_linked(socket, host, up_rx, down_tx, slot));
        })
        .expect("spawn the link thread");
    (up_tx, down_rx, latest)
}

/// One connection after another until the window goes: a backoff from 100 ms doubling to
/// 2 s between tries, back to 100 ms once one connects.
async fn keep_linked(
    socket: PathBuf,
    host: ([u8; 3], [u8; 3]),
    mut up: UnboundedReceiver<Up>,
    down: UnboundedSender<Down>,
    latest: Latest,
) {
    let mut shown: BTreeSet<TerminalStream> = BTreeSet::new();
    let mut backoff = Duration::from_millis(100);
    loop {
        let mut connected = false;
        let linked = session(
            &socket,
            host,
            &mut shown,
            &mut up,
            &down,
            &latest,
            &mut connected,
        )
        .await;
        match linked {
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

/// The window's end of the pipe to the mux: a reading half and a writing half, each driven by
/// a task of its own once linked.
pub struct Wire {
    pub reader: Reader,
    pub writer: Writer,
}

pub struct Reader {
    rd: ReadHalf<DuplexStream>,
    inbuf: BytesMut,
    conn: Connection,
}

pub struct Writer {
    wr: WriteHalf<DuplexStream>,
    control_seq: u32,
    terminal_seqs: HashMap<TerminalStream, u32>,
}

impl Wire {
    pub fn new(pipe: DuplexStream) -> Self {
        let (rd, wr) = tokio::io::split(pipe);
        Wire {
            reader: Reader {
                rd,
                inbuf: BytesMut::new(),
                conn: Connection::new(),
            },
            writer: Writer {
                wr,
                control_seq: 0,
                terminal_seqs: HashMap::new(),
            },
        }
    }
}

/// One half at a time from a test, before the halves go to their own tasks.
#[cfg(test)]
impl Wire {
    pub async fn request(&mut self, request: &ControlRequest) -> io::Result<()> {
        self.writer.request(request).await
    }

    pub async fn read(
        &mut self,
        latest: &Latest,
    ) -> io::Result<(Vec<ControlResponse>, Vec<EventMessage>, bool)> {
        self.reader.read(latest).await
    }
}

impl Writer {
    pub async fn request(&mut self, request: &ControlRequest) -> io::Result<()> {
        let frame = client::encode_control_seq(request, &mut self.control_seq)?;
        self.wr.write_all(&frame).await
    }

    async fn send(&mut self, terminal: TerminalStream, wire: &ClientWire) -> io::Result<()> {
        let seq = self.terminal_seqs.entry(terminal).or_default();
        let frame = client::encode_command_to(terminal, wire, seq)?;
        self.wr.write_all(&frame).await
    }
}

impl Reader {
    /// Read once: screens go to `latest`, the rest is returned.
    pub async fn read(
        &mut self,
        latest: &Latest,
    ) -> io::Result<(Vec<ControlResponse>, Vec<EventMessage>, bool)> {
        let n = self.rd.read_buf(&mut self.inbuf).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the daemon closed the connection",
            ));
        }
        let (mut answers, mut events, mut screens) = (Vec::new(), Vec::new(), false);
        for incoming in self.conn.drain(&mut self.inbuf)? {
            match incoming {
                Incoming::Render {
                    terminal, snapshot, ..
                } => {
                    let mut slot = latest.lock().unwrap_or_else(|e| e.into_inner());
                    screens |= slot.is_empty();
                    slot.insert(terminal, snapshot);
                }
                Incoming::Control(answer) => answers.push(answer),
                Incoming::Event(event) => events.push(event),
                _ => {}
            }
        }
        Ok((answers, events, screens))
    }
}

/// A request id the daemon has never seen. It keeps the answer to an id for 30 days and
/// answers a repeat with the first answer, so a reused id would bring back a terminal made
/// long ago.
pub fn fresh_id(what: &str) -> String {
    static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("desktop-{what}-{}-{nanos}-{n}", std::process::id())
}

/// The mux over this machine's daemon at `socket` and the saved remotes, and the window's
/// end of the pipe to it. The mux ends when this machine's daemon does.
pub async fn through_mux(
    socket: &Path,
    remotes: Vec<qoop_remote::Remote>,
) -> io::Result<(Wire, tokio::task::JoinHandle<()>)> {
    let local = client::connect(socket, qoop_proto::INTERFACE_ID).await?;
    // A megabyte each way: a full screen is tens of kilobytes, and this end reads as fast
    // as it can into the one slot per terminal.
    let (ours, theirs) = tokio::io::duplex(1 << 20);
    let mux = tokio::spawn(qoop_tui::mux::serve(theirs, Some(local), remotes, None));
    Ok((Wire::new(ours), mux))
}

/// The lists the window keeps, asked once linked: spaces, agents, this machine's schedules
/// and qass's switch.
pub fn opening_asks() -> Vec<ControlRequest> {
    vec![
        ControlRequest::ListWorkspaces {
            id: fresh_id("spaces"),
        },
        ControlRequest::ListAgents {
            id: fresh_id("agents"),
        },
        ControlRequest::ScheduleRead {
            id: fresh_id("schedules"),
        },
        ControlRequest::QassWatch {
            id: fresh_id("qass"),
            on: None,
        },
    ]
}

/// What a batch of events asks to be read again: one list per batch, however many changes.
pub fn rereads(events: &[EventMessage]) -> Vec<ControlRequest> {
    let any = |f: fn(&EventMessage) -> bool| events.iter().any(f);
    let mut asks = Vec::new();
    if any(|e| matches!(e, EventMessage::Resync)) {
        return opening_asks();
    }
    // No event names a new space: one made by the CLI, the phone or another window shows
    // up when its first agent does, so the spaces are read with the agents.
    if any(|e| matches!(e, EventMessage::AgentChanged { .. })) {
        asks.push(ControlRequest::ListAgents {
            id: fresh_id("agents"),
        });
        asks.push(ControlRequest::ListWorkspaces {
            id: fresh_id("spaces"),
        });
    }
    if any(|e| matches!(e, EventMessage::SchedulesChanged)) {
        asks.push(ControlRequest::ScheduleRead {
            id: fresh_id("schedules"),
        });
    }
    asks
}

#[allow(clippy::too_many_arguments)]
async fn session(
    socket: &Path,
    host: ([u8; 3], [u8; 3]),
    shown: &mut BTreeSet<TerminalStream>,
    up: &mut UnboundedReceiver<Up>,
    down: &UnboundedSender<Down>,
    latest: &Latest,
    connected: &mut bool,
) -> io::Result<()> {
    // As `qoop` reads them; a list that does not read is no remotes rather than no window.
    let remotes = qoop_remote::saved::load().unwrap_or_default();
    let (wire, mux) = through_mux(socket, remotes).await?;
    *connected = true;
    let ran = linked(wire, host, shown, up, down, latest).await;
    mux.abort();
    ran
}

async fn linked(
    wire: Wire,
    host: ([u8; 3], [u8; 3]),
    shown: &mut BTreeSet<TerminalStream>,
    up: &mut UnboundedReceiver<Up>,
    down: &UnboundedSender<Down>,
    latest: &Latest,
) -> io::Result<()> {
    let Wire { reader, mut writer } = wire;
    for ask in opening_asks() {
        writer.request(&ask).await?;
    }
    if !shown.is_empty() {
        writer
            .request(&ControlRequest::Subscribe {
                id: fresh_id("show"),
                terminals: shown.iter().copied().collect(),
            })
            .await?;
    }
    // What a program inside asks for with `OSC 10 ?` and `OSC 11 ?`: this window's colours.
    // An older daemon refuses the request, which changes nothing.
    writer
        .request(&ControlRequest::HostColors {
            id: fresh_id("colors"),
            fg: host.0,
            bg: host.1,
        })
        .await?;
    let _ = down.send(Down::Connected);
    // Reading has a task of its own: a long write (a paste of megabytes) waits on the mux,
    // which waits on this end to read what a streaming tab sends; one loop doing both would
    // hold both ends still. The lists a batch of events asks for come back to the writer.
    let (asks_tx, mut asks) = unbounded_channel();
    let mut reading = tokio::spawn(read_all(reader, latest.clone(), down.clone(), asks_tx));
    let ran = loop {
        // Only receiving races here, which loses nothing when cancelled: every write runs in
        // an arm's body, never cut in half.
        tokio::select! {
            ended = &mut reading => break ended.unwrap_or_else(|e| Err(io::Error::other(e))),
            ask = asks.recv() => {
                if let Some(ask) = ask
                    && let Err(e) = writer.request(&ask).await
                {
                    break Err(e);
                }
            }
            asked = up.recv() => match asked {
                None => break Ok(()),
                // git in the folder, by its own deadline, beside the link: an answer like any.
                Some(Up::Changes { id, terminal, cwd }) => {
                    let down = down.clone();
                    tokio::spawn(async move {
                        let answer = qoop_runtime::changes::read(id, terminal, Some(cwd), false).await;
                        let _ = down.send(Down::Answer(Box::new(answer)));
                    });
                }
                Some(asked) => {
                    if let Err(e) = write_up(&mut writer, shown, asked).await {
                        break Err(e);
                    }
                }
            },
        }
    };
    reading.abort();
    ran
}

/// Everything the mux sends, until it ends: screens into `latest`, the rest to the window.
async fn read_all(
    mut reader: Reader,
    latest: Latest,
    down: UnboundedSender<Down>,
    asks: UnboundedSender<ControlRequest>,
) -> io::Result<()> {
    loop {
        let (answers, events, screens) = reader.read(&latest).await?;
        if screens {
            let _ = down.send(Down::Screens);
        }
        for ask in rereads(&events) {
            let _ = asks.send(ask);
        }
        for answer in answers {
            let _ = down.send(Down::Answer(Box::new(answer)));
        }
        for event in events {
            let _ = down.send(Down::Event(event));
        }
    }
}

/// One thing the window asked for, written to the mux.
async fn write_up(
    writer: &mut Writer,
    shown: &mut BTreeSet<TerminalStream>,
    asked: Up,
) -> io::Result<()> {
    match asked {
        Up::Input(terminal, bytes) => {
            for piece in client::split_input(&bytes) {
                let input = ClientWire::Input {
                    bytes: piece.to_vec(),
                    from_terminal: None,
                    from_token: None,
                };
                writer.send(terminal, &input).await?;
            }
        }
        Up::Paste(terminal, bytes) => {
            for piece in client::split_input(&bytes) {
                let paste = ClientWire::Paste {
                    bytes: piece.to_vec(),
                };
                writer.send(terminal, &paste).await?;
            }
        }
        Up::Resize(terminal, cols, rows) => {
            writer
                .send(terminal, &ClientWire::Resize { cols, rows })
                .await?;
        }
        Up::Show(terminals) => {
            shown.extend(&terminals);
            writer
                .request(&ControlRequest::Subscribe {
                    id: fresh_id("show"),
                    terminals,
                })
                .await?;
        }
        Up::Hide(terminals) => {
            for t in &terminals {
                shown.remove(t);
            }
            writer
                .request(&ControlRequest::Unsubscribe {
                    id: fresh_id("hide"),
                    terminals,
                })
                .await?;
        }
        Up::Request(request) => writer.request(&request).await?,
        // Read in this process, never written to the mux (`linked` takes it first).
        Up::Changes { .. } => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ids_are_never_reused() {
        assert_ne!(fresh_id("new"), fresh_id("new"));
    }

    #[test]
    fn a_batch_of_changes_reads_each_list_once() {
        let events = [
            EventMessage::AgentChanged { terminal: 1 },
            EventMessage::AgentChanged { terminal: 2 },
            EventMessage::SchedulesChanged,
        ];
        let asks: Vec<&str> = rereads(&events)
            .iter()
            .map(|a| match a {
                ControlRequest::ListAgents { .. } => "agents",
                ControlRequest::ListWorkspaces { .. } => "spaces",
                ControlRequest::ScheduleRead { .. } => "schedules",
                _ => "other",
            })
            .collect();
        assert_eq!(asks, ["agents", "spaces", "schedules"]);
        assert_eq!(rereads(&[EventMessage::Resync]).len(), opening_asks().len());
        assert!(rereads(&[EventMessage::TasksChanged]).is_empty());
    }

    /// A paste of megabytes while a tab streams megabytes back: with small pipes both ways, the
    /// far end only reads once it has written everything. Reading on its own task, the link
    /// drains the stream while its write waits, so both finish; one loop doing both would hold
    /// both ends still.
    #[tokio::test]
    async fn a_long_paste_while_a_tab_streams_moves_both_ways() {
        const PASTE: usize = 4 << 20;
        let (ours, mut theirs) = tokio::io::duplex(64 * 1024);
        let (up_tx, mut up_rx) = unbounded_channel();
        let (down_tx, mut down_rx) = unbounded_channel();
        let link = tokio::spawn(async move {
            let mut shown = BTreeSet::new();
            let latest = Latest::default();
            linked(
                Wire::new(ours),
                ([0; 3], [0; 3]),
                &mut shown,
                &mut up_rx,
                &down_tx,
                &latest,
            )
            .await
        });
        up_tx
            .send(Up::Paste(1, vec![b'a'; PASTE]))
            .expect("the link is up");
        let far = async {
            let mut seq = 0u32;
            for n in 0..64 {
                let body = serde_json::to_vec(&ControlResponse::Failed {
                    id: format!("noise-{n}"),
                    reason: "x".repeat(64 * 1024),
                })
                .expect("json");
                let frames = qoop_proto::chunk(
                    qoop_proto::Channel::Control,
                    0,
                    seq,
                    None,
                    bytes::Bytes::from(body),
                );
                seq = seq.wrapping_add(frames.len() as u32);
                let mut buf = BytesMut::new();
                for frame in &frames {
                    frame.encode(&mut buf).expect("a frame");
                }
                theirs.write_all(&buf).await.expect("the stream is read");
            }
            let mut got = 0usize;
            let mut chunk = vec![0u8; 64 * 1024];
            while got < PASTE {
                let n = theirs.read(&mut chunk).await.expect("read");
                assert!(n > 0, "the link closed");
                got += n;
            }
        };
        tokio::time::timeout(Duration::from_secs(30), far)
            .await
            .expect("the paste and the stream both went through");
        let mut heard = 0;
        while heard < 64 {
            let down = tokio::time::timeout(Duration::from_secs(5), down_rx.recv())
                .await
                .expect("every streamed answer reaches the window")
                .expect("the link is up");
            heard += usize::from(matches!(down, Down::Answer(_)));
        }
        link.abort();
    }

    /// The mux reads the remotes list and writes the space order beside it: a test's own, never
    /// the person's. Set once for the whole test binary, before any mux in it starts.
    fn isolated_remotes() {
        static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
        DIR.get_or_init(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            // SAFETY: set once, before any mux in this binary reads it.
            unsafe {
                std::env::set_var(
                    qoop_remote::saved::REMOTES_ENV,
                    dir.path().join("remotes.json"),
                )
            };
            dir
        });
    }

    /// A window linked through the mux gets the merged agent and space lists from a daemon.
    #[tokio::test]
    async fn the_link_gets_agents_and_spaces_through_the_mux() {
        isolated_remotes();
        let d = qoop_testkit::TestDaemon::start().await;
        let (mut wire, mux) = through_mux(d.socket(), Vec::new()).await.expect("linked");
        for ask in opening_asks() {
            wire.request(&ask).await.expect("ask");
        }
        let latest = Latest::default();
        let (mut agents, mut spaces) = (None, None);
        let deadline = std::time::Instant::now() + qoop_testkit::PATIENCE;
        while (agents.is_none() || spaces.is_none()) && std::time::Instant::now() < deadline {
            let read = tokio::time::timeout(Duration::from_secs(1), wire.read(&latest)).await;
            let Ok(read) = read else { continue };
            for answer in read.expect("read").0 {
                match answer {
                    ControlResponse::Agents { agents: a, .. } => agents = Some(a),
                    ControlResponse::Workspaces { workspaces, .. } => spaces = Some(workspaces),
                    _ => {}
                }
            }
        }
        let terminal = d.terminals().streams()[0];
        let agents = agents.expect("an Agents answer");
        assert!(agents.iter().any(|r| r.terminal == terminal), "{agents:?}");
        assert!(!spaces.expect("a Workspaces answer").is_empty());
        mux.abort();
    }

    async fn ask(c: &mut qoop_testkit::TestClient, request: ControlRequest) -> ControlResponse {
        c.control(&request, qoop_testkit::PATIENCE)
            .await
            .expect("an answer")
    }

    /// A real daemon's sleeping agent tab, read through the mux as the window reads it, is on
    /// home: the daemon lists it under its space, not as an agent row.
    #[tokio::test]
    async fn a_sleeping_agent_tab_of_a_real_daemon_is_on_home() {
        use crate::state::{Chip, Kind, Model};
        isolated_remotes();
        let d = qoop_testkit::TestDaemon::start().await;
        let mut c = d.attach("sleeper").await;
        let space = match ask(
            &mut c,
            ControlRequest::CreateWorkspace {
                id: "space".into(),
                name: "dozing".into(),
            },
        )
        .await
        {
            ControlResponse::WorkspaceCreated { workspace, .. } => workspace,
            other => panic!("{other:?}"),
        };
        let tab = match ask(
            &mut c,
            ControlRequest::CreateTerminal {
                id: "tab".into(),
                cwd: None,
                workspace: Some(space),
                name: None,
            },
        )
        .await
        {
            ControlResponse::TerminalCreated { terminal, .. } => terminal,
            other => panic!("{other:?}"),
        };
        // A fresh shell is refused while it starts: asked again until it has settled.
        let deadline = std::time::Instant::now() + qoop_testkit::PATIENCE;
        for n in 0.. {
            let answer = ask(
                &mut c,
                ControlRequest::SleepWorkspace {
                    id: format!("sleep-{n}"),
                    workspace: space,
                    terminal: None,
                },
            )
            .await;
            match answer {
                ControlResponse::WorkspaceAsleep { .. } => break,
                other if std::time::Instant::now() > deadline => panic!("{other:?}"),
                _ => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        }
        // What a sleep of an agent's tab writes down.
        d.store()
            .checkpoint(vec![qoop_store::Checkpoint {
                terminal: i64::from(tab),
                agent: Some("claude".into()),
                agent_session: None,
            }])
            .await
            .expect("a checkpoint");

        let (mut wire, mux) = through_mux(d.socket(), Vec::new()).await.expect("linked");
        for ask in opening_asks() {
            wire.request(&ask).await.expect("ask");
        }
        let latest = Latest::default();
        let mut model = Model::default();
        let (mut agents, mut spaces) = (false, false);
        let deadline = std::time::Instant::now() + qoop_testkit::PATIENCE;
        while !(agents && spaces) && std::time::Instant::now() < deadline {
            let read = tokio::time::timeout(Duration::from_secs(1), wire.read(&latest)).await;
            let Ok(read) = read else { continue };
            for answer in read.expect("read").0 {
                match answer {
                    ControlResponse::Agents { agents: a, .. } => {
                        model.rows = a;
                        agents = true;
                    }
                    ControlResponse::Workspaces { workspaces, .. } => {
                        model.spaces = workspaces;
                        spaces = true;
                    }
                    _ => {}
                }
            }
        }
        mux.abort();
        assert!(
            !model.rows.iter().any(|r| r.terminal == tab && r.agent),
            "not an awake agent row"
        );
        let group = model
            .groups(Chip::All)
            .into_iter()
            .find(|g| g.space.id == space)
            .expect("the sleeping space is on home");
        let kinds: Vec<(u32, Kind)> = group
            .rows
            .iter()
            .map(|e| (e.terminal(), e.kind()))
            .collect();
        assert_eq!(kinds, [(tab, Kind::Sleeping)]);
        assert_eq!(model.count(Chip::Sleeping), 1);
    }
}
